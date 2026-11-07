//! Routes, request guards and the static UI.
//!
//! Every response carries a strict Content-Security-Policy and related headers. Requests pass, in order:
//! the host check (DNS rebinding), then for `/api/*` the CSRF check (a custom header on state-changing
//! calls, and an `Origin` that matches the server), then authentication. No CORS headers are sent, so other
//! sites cannot read responses.

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{ConnectInfo, FromRequest, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use fosta_studio::AppState;
use serde::Deserialize;
use tokio::sync::broadcast;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};

use crate::{
    api::{self, error_response, WebEvent},
    auth::Auth,
};

/// Header every API call must send. A cross-site form or `<img>` cannot set it, and a cross-site `fetch`
/// that sets it needs a CORS preflight this server never approves.
pub const CSRF_HEADER: &str = "x-fosta-studio";

const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; \
     form-action 'self'; frame-ancestors 'none'";

pub struct Server {
    pub state: Arc<AppState>,
    pub events: broadcast::Sender<WebEvent>,
    pub auth: Auth,
    /// Extra origins allowed to call the API (`FOSTA_STUDIO_ALLOWED_ORIGINS`), e.g. a reverse proxy's.
    pub allowed_origins: Vec<String>,
    /// Host names requests may be addressed to; empty means any.
    pub allowed_hosts: Vec<String>,
    pub ui_dir: PathBuf,
}

/// `FOSTA_STUDIO_ALLOWED_HOSTS`, or only loopback names when authentication is off (a DNS-rebinding page
/// could otherwise drive an unauthenticated server through the victim's browser).
pub fn allowed_hosts(configured: &[String], auth_off: bool) -> Vec<String> {
    if !configured.is_empty() {
        return configured.iter().map(|host| host.to_ascii_lowercase()).collect();
    }
    if auth_off {
        return ["localhost", "127.0.0.1", "[::1]"].map(String::from).to_vec();
    }
    Vec::new()
}

pub fn router(server: Server) -> Router {
    let server = Arc::new(server);
    let index = server.ui_dir.join("index.html");
    let ui = ServeDir::new(&server.ui_dir).fallback(ServeFile::new(index));
    let api = Router::new()
        .route("/invoke/{command}", post(api::invoke))
        .route("/events", get(api::events))
        .route("/logout", post(logout))
        .layer(middleware::from_fn_with_state(Arc::clone(&server), require_session))
        .layer(middleware::from_fn_with_state(Arc::clone(&server), check_csrf))
        .route("/login", post(login))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    let app = Router::new()
        .nest("/api", api)
        .fallback_service(ui)
        .layer(middleware::from_fn_with_state(Arc::clone(&server), require_session_for_pages))
        .route("/healthz", get(|| async { "ok" }));
    app.layer(middleware::from_fn_with_state(Arc::clone(&server), check_host))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            // `same-origin`, not `no-referrer`: with the latter, browsers send `Origin: null` on form posts.
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .with_state(server)
}

fn client_ip(request: &Request) -> IpAddr {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip())
}

/// The host name of a `Host` header, without the port (`[::1]` keeps its brackets).
fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return host.split_once(']').map_or(host, |(name, _)| &host[..=name.len()]);
    }
    host.rsplit_once(':').map_or(host, |(name, _)| name)
}

async fn check_host(State(server): State<Arc<Server>>, request: Request, next: Next) -> Response {
    if !server.allowed_hosts.is_empty() {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let name = host_name(host).to_ascii_lowercase();
        if !server.allowed_hosts.contains(&name) && request.uri().path() != "/healthz" {
            return error_response(
                StatusCode::MISDIRECTED_REQUEST,
                &format!("Host '{host}' is not allowed; add it to FOSTA_STUDIO_ALLOWED_HOSTS"),
            );
        }
    }
    next.run(request).await
}

/// Whether the request's `Origin` (when the browser sent one) is this server or an allowed origin.
fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let host = headers.get(header::HOST).and_then(|value| value.to_str().ok()).unwrap_or_default();
    let same_origin = !host.is_empty() && (origin == format!("http://{host}") || origin == format!("https://{host}"));
    same_origin || allowed.iter().any(|allowed| allowed == origin)
}

async fn check_csrf(State(server): State<Arc<Server>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if !origin_allowed(headers, &server.allowed_origins) {
        return error_response(StatusCode::FORBIDDEN, "Cross-origin requests are not allowed");
    }
    if headers
        .get("sec-fetch-site")
        .is_some_and(|site| !matches!(site.as_bytes(), b"same-origin" | b"none"))
    {
        return error_response(StatusCode::FORBIDDEN, "Cross-site requests are not allowed");
    }
    if request.method() != Method::GET && headers.get(CSRF_HEADER).is_none() {
        return error_response(StatusCode::FORBIDDEN, "Missing the X-Fosta-Studio request header");
    }
    next.run(request).await
}

fn has_session(server: &Server, headers: &HeaderMap) -> bool {
    server
        .auth
        .has_session(headers.get(header::COOKIE).and_then(|value| value.to_str().ok()))
}

async fn require_session(State(server): State<Arc<Server>>, request: Request, next: Next) -> Response {
    if has_session(&server, request.headers()) {
        return next.run(request).await;
    }
    error_response(
        StatusCode::UNAUTHORIZED,
        "Sign in first: open the login address printed in the server log",
    )
}

/// Pages and assets: a valid session, or `?token=…` (which sets the cookie and drops the token from the
/// address), else the sign-in page.
async fn require_session_for_pages(State(server): State<Arc<Server>>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path.starts_with("/api/") || path == "/healthz" {
        return next.run(request).await;
    }
    if let Some(token) = query_token(request.uri().query()) {
        let client = client_ip(&request);
        if !server.auth.may_attempt(client) {
            return login_page(
                StatusCode::TOO_MANY_REQUESTS,
                Some("Too many failed attempts. Try again in a few minutes."),
            );
        }
        if !server.auth.check_token(client, &token) {
            return login_page(StatusCode::UNAUTHORIZED, Some("That access token is not valid."));
        }
        return signed_in(&server.auth);
    }
    if has_session(&server, request.headers()) {
        return next.run(request).await;
    }
    if request.method() == Method::GET && (path == "/" || path.ends_with(".html")) {
        return login_page(StatusCode::UNAUTHORIZED, None);
    }
    error_response(StatusCode::UNAUTHORIZED, "Sign in first")
}

fn query_token(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "token")
        .map(|(_, value)| percent_decode(value))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 3 <= bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        decoded.push(byte);
                        index += 3;
                        continue;
                    }
                    None => decoded.push(b'%'),
                }
            }
            b'+' => decoded.push(b' '),
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Sets the session cookie and redirects to the app, dropping the token from the address bar and history.
fn signed_in(auth: &Auth) -> Response {
    let mut response = Redirect::to("/").into_response();
    if let Ok(cookie) = HeaderValue::from_str(&auth.session_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

#[derive(Deserialize)]
struct LoginForm {
    token: String,
}

/// `POST /api/login` from the sign-in page's form.
async fn login(State(server): State<Arc<Server>>, request: Request) -> Response {
    if !origin_allowed(request.headers(), &server.allowed_origins) {
        return error_response(StatusCode::FORBIDDEN, "Cross-origin requests are not allowed");
    }
    let client = client_ip(&request);
    if !server.auth.may_attempt(client) {
        return login_page(
            StatusCode::TOO_MANY_REQUESTS,
            Some("Too many failed attempts. Try again in a few minutes."),
        );
    }
    let Ok(Form(form)) = Form::<LoginForm>::from_request(request, &()).await else {
        return login_page(StatusCode::BAD_REQUEST, Some("Enter the access token."));
    };
    if !server.auth.check_token(client, &form.token) {
        return login_page(StatusCode::UNAUTHORIZED, Some("That access token is not valid."));
    }
    signed_in(&server.auth)
}

/// `POST /api/logout`: ends every session (one token per server, so every browser is signed out).
async fn logout(State(server): State<Arc<Server>>) -> Response {
    let mut response = match server.auth.revoke_sessions() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            eprintln!("fosta-studio-server: cannot sign out: {error}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Could not end the sessions; see the server log")
        }
    };
    if let Ok(cookie) = HeaderValue::from_str(&server.auth.clear_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

fn login_page(status: StatusCode, message: Option<&str>) -> Response {
    let message = message.map_or_else(String::new, |text| format!("<p class=\"error\">{}</p>", escape(text)));
    let page = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in · Fosta Studio</title>
<style>
  :root {{ color-scheme: light dark; font-family: system-ui, sans-serif; }}
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: Canvas; color: CanvasText; }}
  main {{ width: min(420px, calc(100vw - 32px)); }}
  h1 {{ font-size: 1.4rem; margin: 0 0 .5rem; }}
  p {{ line-height: 1.5; opacity: .85; }}
  form {{ display: flex; gap: .5rem; margin-top: 1rem; }}
  input {{ flex: 1; padding: .55rem .7rem; font: inherit; border: 1px solid #8888; border-radius: 6px; }}
  button {{ padding: .55rem 1rem; font: inherit; border: 0; border-radius: 6px; background: #2563eb; color: #fff; cursor: pointer; }}
  .error {{ color: #dc2626; opacity: 1; }}
  code {{ font-size: .9em; }}
</style>
</head>
<body>
<main>
  <h1>Fosta Studio</h1>
  <p>Enter the access token, or open the sign-in address printed in the server log
  (<code>docker logs &lt;container&gt;</code>).</p>
  {message}
  <form method="post" action="/api/login">
    <input name="token" type="password" autocomplete="current-password" placeholder="Access token" required autofocus>
    <button type="submit">Sign in</button>
  </form>
</main>
</body>
</html>"#
    );
    let mut response = (status, Html(page)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `fosta-studio-server healthcheck` (the image's HEALTHCHECK): `GET /healthz` on the local port.
pub fn healthcheck(listen: SocketAddr) -> Result<(), String> {
    let ip = if listen.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        listen.ip()
    };
    let address = SocketAddr::new(ip, listen.port());
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).map_err(|error| format!("{address}: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(
            format!(
                "GET /healthz HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
                listen.port()
            )
            .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply);
    if reply.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(format!("unhealthy: {}", reply.lines().next().unwrap_or("no reply")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn origins_must_match_the_host_or_the_allow_list() {
        let allowed = vec![String::from("https://studio.example.com")];
        assert!(origin_allowed(&headers(&[("host", "localhost:8080")]), &allowed));
        assert!(origin_allowed(
            &headers(&[("host", "localhost:8080"), ("origin", "http://localhost:8080")]),
            &allowed
        ));
        assert!(origin_allowed(
            &headers(&[("host", "internal:8080"), ("origin", "https://studio.example.com")]),
            &allowed
        ));
        assert!(!origin_allowed(
            &headers(&[("host", "localhost:8080"), ("origin", "http://evil.example")]),
            &allowed
        ));
        assert!(!origin_allowed(
            &headers(&[("host", "localhost:8080"), ("origin", "null")]),
            &allowed
        ));
    }

    #[test]
    fn host_names_drop_the_port() {
        assert_eq!(host_name("localhost:8080"), "localhost");
        assert_eq!(host_name("example.com"), "example.com");
        assert_eq!(host_name("[::1]:8080"), "[::1]");
        assert_eq!(allowed_hosts(&[], false), Vec::<String>::new());
        assert!(allowed_hosts(&[], true).contains(&String::from("localhost")));
    }

    #[test]
    fn tokens_are_read_from_the_query() {
        assert_eq!(query_token(Some("a=1&token=abc%2Fd+e")).as_deref(), Some("abc/d e"));
        assert_eq!(query_token(Some("a=1")), None);
        assert_eq!(query_token(None), None);
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }
}
