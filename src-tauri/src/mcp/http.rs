//! Streamable HTTP transport (MCP 2025-06-18 and later): one endpoint, `/mcp`.
//!
//! - `POST /mcp` carries one JSON-RPC message; requests are answered with `application/json`, notifications and
//!   responses with `202 Accepted`. `initialize` opens a session (`Mcp-Session-Id`), which every later request sends;
//!   an unknown session is `404` so the client initializes again. `DELETE /mcp` ends a session; `GET` is `405` (this
//!   server sends no requests of its own).
//! - Every request needs `Authorization: Bearer <token>`: the app's MCP token on the desktop, the server's access
//!   token on the web edition. No cookies are read, so a web page cannot ride on a browser session.
//! - Desktop ([`serve_loopback`]): bound to 127.0.0.1 only; non-loopback peers, a `Host` other than a loopback name and
//!   a non-loopback `Origin` are refused (DNS rebinding). Web: the server's host check applies, and an `Origin` must be
//!   the server itself.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Weak},
};

use axum::{
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, MethodRouter},
    Router,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::protocol::{self, Message, INVALID_REQUEST, PARSE_ERROR, PROTOCOL_VERSIONS};
use crate::{
    error::{AppError, AppResult},
    AppState,
};

pub const SESSION_HEADER: &str = "mcp-session-id";
pub const VERSION_HEADER: &str = "mcp-protocol-version";
const MAX_BODY: usize = 1024 * 1024;

/// Checks a bearer token from `peer` (the web server's access token, with its rate limit).
pub type TokenCheck = Arc<dyn Fn(IpAddr, &str) -> bool + Send + Sync>;

struct Endpoint {
    app: Weak<AppState>,
    /// The desktop listener: loopback peers, hosts and origins only; the app's MCP token.
    loopback: bool,
    /// `None`: the app's MCP token ([`super::McpState::token_matches`]).
    check: Option<TokenCheck>,
}

fn route<S: Clone + Send + Sync + 'static>(endpoint: Endpoint) -> MethodRouter<S> {
    any(endpoint_handler).with_state(Arc::new(endpoint))
}

/// The web edition's `/mcp` route, authenticated by `check` (the server's access token).
pub fn web_route<S: Clone + Send + Sync + 'static>(app: Weak<AppState>, check: TokenCheck) -> MethodRouter<S> {
    route(Endpoint {
        app,
        loopback: false,
        check: Some(check),
    })
}

/// Binds 127.0.0.1:`port` (0 picks one) and serves `/mcp` there until the returned sender fires or is dropped.
pub fn serve_loopback(executor: &tokio::runtime::Handle, app: Weak<AppState>, port: u16) -> AppResult<(u16, oneshot::Sender<()>)> {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|error| {
        AppError::new(format!(
            "Cannot listen on 127.0.0.1:{port} ({error}). Pick another port in Settings → MCP server."
        ))
    })?;
    listener.set_nonblocking(true)?;
    let bound = listener.local_addr()?.port();
    let _runtime = executor.enter();
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let (stop, stopped) = oneshot::channel::<()>();
    let service = Router::new()
        .route(
            "/mcp",
            route(Endpoint {
                app,
                loopback: true,
                check: None,
            }),
        )
        .into_make_service_with_connect_info::<SocketAddr>();
    executor.spawn(async move {
        let shutdown = async {
            let _ = stopped.await;
        };
        if let Err(error) = axum::serve(listener, service).with_graceful_shutdown(shutdown).await {
            tracing::warn!(%error, "MCP server stopped");
        }
    });
    Ok((bound, stop))
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

fn plain_error(status: StatusCode, message: &str) -> Response {
    json_response(status, &json!({ "error": message }))
}

fn rpc_error(status: StatusCode, id: &Value, code: i64, message: &str) -> Response {
    json_response(status, &protocol::error(id, code, message))
}

/// The host name of a `Host` header or an origin's authority, without the port (`[::1]` keeps its brackets).
fn host_name(authority: &str) -> &str {
    if authority.starts_with('[') {
        return authority.split_once(']').map_or(authority, |(name, _)| &authority[..=name.len()]);
    }
    authority.rsplit_once(':').map_or(authority, |(name, _)| name)
}

pub(crate) fn is_loopback_name(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

pub(crate) fn peer_allowed(peer: Option<IpAddr>) -> bool {
    peer.is_some_and(|ip| ip.is_loopback() || matches!(ip, IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())))
}

/// Whether an `Origin` may call: none at all (CLI clients), a loopback page on the desktop, the server itself on the web.
pub(crate) fn origin_allowed(origin: Option<&str>, host: &str, loopback: bool) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(authority) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else {
        return false;
    };
    if loopback {
        !authority.contains('/') && is_loopback_name(host_name(authority))
    } else {
        !host.is_empty() && authority.eq_ignore_ascii_case(host)
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|token| !token.is_empty())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The checks every request passes, in order: peer, host, origin, enabled, token, protocol version.
// The error is the HTTP response itself, built at most once per request.
#[allow(clippy::result_large_err)]
fn guard(endpoint: &Endpoint, peer: Option<IpAddr>, headers: &HeaderMap) -> Result<Arc<AppState>, Response> {
    let app = endpoint
        .app
        .upgrade()
        .ok_or_else(|| plain_error(StatusCode::SERVICE_UNAVAILABLE, "Fosta Studio is shutting down"))?;
    let host = header_str(headers, header::HOST.as_str()).unwrap_or_default();
    if endpoint.loopback {
        if !peer_allowed(peer) {
            return Err(plain_error(StatusCode::FORBIDDEN, "Only this computer may connect"));
        }
        if !is_loopback_name(host_name(host)) {
            return Err(plain_error(StatusCode::FORBIDDEN, "Host not allowed: use 127.0.0.1 or localhost"));
        }
    }
    if !origin_allowed(header_str(headers, header::ORIGIN.as_str()), host, endpoint.loopback) {
        return Err(plain_error(StatusCode::FORBIDDEN, "Origin not allowed"));
    }
    if !app.config.read(|config| config.mcp.enabled) {
        return Err(plain_error(
            StatusCode::FORBIDDEN,
            "The MCP server is off. Turn it on in Fosta Studio: Settings → MCP server.",
        ));
    }
    let valid = bearer(headers).is_some_and(|token| match &endpoint.check {
        Some(check) => check(peer.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), token),
        None => app.mcp.token_matches(token),
    });
    if !valid {
        let mut response = plain_error(StatusCode::UNAUTHORIZED, "Missing or wrong bearer token");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"fosta-studio\""));
        return Err(response);
    }
    if let Some(version) = header_str(headers, VERSION_HEADER) {
        if !PROTOCOL_VERSIONS.contains(&version) {
            return Err(plain_error(
                StatusCode::BAD_REQUEST,
                &format!("Unsupported MCP-Protocol-Version {version}"),
            ));
        }
    }
    Ok(app)
}

async fn endpoint_handler(State(endpoint): State<Arc<Endpoint>>, request: Request) -> Response {
    let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|info| info.0.ip());
    let app = match guard(&endpoint, peer, request.headers()) {
        Ok(app) => app,
        Err(response) => return response,
    };
    let session = header_str(request.headers(), SESSION_HEADER).map(str::to_owned);
    match *request.method() {
        Method::POST => {
            let body = match to_bytes(request.into_body(), MAX_BODY).await {
                Ok(body) => body,
                Err(_) => return plain_error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large"),
            };
            post(&app, session.as_deref(), &body).await
        }
        Method::DELETE => match session {
            Some(id) if app.mcp.close_session(&id) => StatusCode::NO_CONTENT.into_response(),
            Some(_) => plain_error(StatusCode::NOT_FOUND, "Unknown session"),
            None => plain_error(StatusCode::BAD_REQUEST, "Missing Mcp-Session-Id"),
        },
        _ => {
            let mut response = plain_error(StatusCode::METHOD_NOT_ALLOWED, "Use POST (this server opens no event stream)");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
            response
        }
    }
}

async fn post(app: &AppState, session: Option<&str>, body: &[u8]) -> Response {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return rpc_error(StatusCode::BAD_REQUEST, &Value::Null, PARSE_ERROR, "The body is not JSON");
    };
    let (id, method, params) = match protocol::classify(&message) {
        Message::Request { id, method, params } => (id, method, params),
        Message::Ignored => return StatusCode::ACCEPTED.into_response(),
        Message::Invalid(why) => return rpc_error(StatusCode::BAD_REQUEST, &Value::Null, INVALID_REQUEST, why),
    };
    if method == "initialize" {
        let (client, version) = protocol::initialize_params(params);
        let session = match app.mcp.open_session(client, version) {
            Ok(session) => session,
            Err(error) => return rpc_error(StatusCode::INTERNAL_SERVER_ERROR, id, -32603, &error.to_string()),
        };
        let mut response = json_response(StatusCode::OK, &protocol::initialize_result(id, version));
        if let Ok(value) = HeaderValue::from_str(&session) {
            response.headers_mut().insert(SESSION_HEADER, value);
        }
        return response;
    }
    if method == "ping" && session.is_none() {
        return json_response(StatusCode::OK, &json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
    }
    let Some(session) = session else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            id,
            INVALID_REQUEST,
            "Missing Mcp-Session-Id: send initialize first",
        );
    };
    let Some((client, _)) = app.mcp.session(session) else {
        return rpc_error(
            StatusCode::NOT_FOUND,
            id,
            INVALID_REQUEST,
            "Unknown or expired session: initialize again",
        );
    };
    json_response(StatusCode::OK, &protocol::handle(app, &client, id, method, params).await)
}
