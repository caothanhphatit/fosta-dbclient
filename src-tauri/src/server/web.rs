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
        .route("/export/{ticket}", get(crate::export::download))
        .route("/logout", post(logout))
        // Unknown API paths are a JSON 404 (after the session check), not the UI's index.html with 200.
        .fallback(api_not_found)
        .layer(middleware::from_fn_with_state(Arc::clone(&server), require_session))
        .layer(middleware::from_fn_with_state(Arc::clone(&server), check_csrf))
        .route("/login", post(login))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    // MCP clients authenticate with the MCP-only token (Settings → MCP server) as a bearer token: never the access
    // token, never the session cookie, so neither a web page nor an MCP client can reach the UI or `/api` through it.
    // With authentication off the endpoint refuses everything.
    let (allow_server, failed_server) = (Arc::clone(&server), Arc::clone(&server));
    let mcp_gate = fosta_studio::mcp::http::WebGate {
        allow: Arc::new(move |client| !allow_server.auth.is_off() && allow_server.auth.mcp_may_attempt(client)),
        failed: Arc::new(move |client| failed_server.auth.record_mcp_failure(client)),
    };
    let app = Router::new()
        .nest("/api", api)
        .fallback_service(ui)
        .layer(middleware::from_fn_with_state(Arc::clone(&server), require_session_for_pages))
        .route("/healthz", get(|| async { "ok" }))
        .route("/mcp", fosta_studio::mcp::http::web_route(Arc::downgrade(&server.state), mcp_gate));
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
        if !access_token_valid(&server, client, &token) {
            return login_page(StatusCode::UNAUTHORIZED, Some("That access token is not valid."));
        }
        return signed_in(&server.auth);
    }
    if has_session(&server, request.headers()) {
        return next.run(request).await;
    }
    if request.method() == Method::GET && (path == "/" || path.ends_with(".html")) {
        let message = query_param(request.uri().query(), "error").and_then(|code| login_error(&code));
        return login_page(StatusCode::UNAUTHORIZED, message);
    }
    error_response(StatusCode::UNAUTHORIZED, "Sign in first")
}

/// Messages for `/?error=…`: a failed sign-in redirects there (303), so reloading the page does not post the form again.
fn login_error(code: &str) -> Option<&'static str> {
    match code {
        "invalid" => Some("That access token is not valid."),
        "missing" => Some("Enter the access token."),
        "throttled" => Some("Too many failed attempts. Try again in a few minutes."),
        _ => None,
    }
}

fn login_failed(code: &str) -> Response {
    Redirect::to(&format!("/?error={code}")).into_response()
}

fn query_token(query: Option<&str>) -> Option<String> {
    query_param(query, "token")
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
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
        return login_failed("throttled");
    }
    let Ok(Form(form)) = Form::<LoginForm>::from_request(request, &()).await else {
        return login_failed("missing");
    };
    if !access_token_valid(&server, client, &form.token) {
        return login_failed("invalid");
    }
    signed_in(&server.auth)
}

/// The access token signs in; the MCP token never does (it is refused even if it were set as the access token).
fn access_token_valid(server: &Server, client: IpAddr, candidate: &str) -> bool {
    if server.state.mcp.is_token(candidate) {
        server.auth.record_failure(client);
        return false;
    }
    server.auth.check_token(client, candidate)
}

async fn api_not_found(request: Request) -> Response {
    error_response(StatusCode::NOT_FOUND, &format!("No such API endpoint: {}", request.uri().path()))
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

    #[test]
    fn failed_sign_ins_redirect_to_a_page_that_names_the_problem() {
        let response = login_failed("invalid");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/?error=invalid");
        assert_eq!(login_error("invalid"), Some("That access token is not valid."));
        assert_eq!(login_error("<script>"), None);
    }

    #[tokio::test]
    async fn unknown_api_paths_are_a_json_404() {
        let request = Request::builder().uri("/api/config").body(axum::body::Body::empty()).unwrap();
        let response = api_not_found(request).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    }

    /// The whole router over a real socket: the MCP token and the access token never stand in for each other.
    mod mcp_routes {
        use std::sync::OnceLock;

        use fosta_studio::{
            config::DATA_DIR_ENV,
            secrets::{self, file::FileStore},
            Capabilities,
        };
        use serde_json::{json, Value};

        use super::super::*;
        use crate::api::WebEvents;

        /// The tests share the process-wide data directory and secret store, so they run one at a time.
        static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        fn data_dir() -> &'static PathBuf {
            static DIR: OnceLock<PathBuf> = OnceLock::new();
            DIR.get_or_init(|| {
                let dir = std::env::temp_dir().join(format!("fosta-web-mcp-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                std::env::set_var(DATA_DIR_ENV, &dir);
                let (store, _) = FileStore::open(&dir, None).unwrap();
                secrets::install(Box::new(store)).unwrap();
                dir
            })
        }

        struct Running {
            base: String,
            origin: String,
            state: Arc<AppState>,
            access: Option<String>,
            client: reqwest::Client,
        }

        /// Serves the router on 127.0.0.1 with the MCP server on.
        async fn start(auth_off: bool) -> Running {
            let dir = data_dir();
            let (sender, _) = broadcast::channel(16);
            let state = Arc::new(
                AppState::new(
                    Capabilities::web(false, true),
                    tokio::runtime::Handle::current(),
                    Arc::new(WebEvents(sender.clone())),
                )
                .unwrap(),
            );
            state.mcp.attach(&state);
            state
                .config
                .update(|config| {
                    config.mcp.enabled = true;
                    Ok(())
                })
                .unwrap();
            state.mcp.apply().unwrap();
            let (auth, access) = if auth_off {
                (Auth::off(), None)
            } else {
                let (auth, token, _) = Auth::open(dir, None, false).unwrap();
                (auth, Some(token))
            };
            let app = router(Server {
                state: Arc::clone(&state),
                events: sender,
                auth,
                allowed_origins: Vec::new(),
                allowed_hosts: allowed_hosts(&[], auth_off),
                ui_dir: dir.join("no-ui"),
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
            });
            Running {
                base: format!("http://127.0.0.1:{port}"),
                origin: format!("http://127.0.0.1:{port}"),
                state,
                access,
                client: reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap(),
            }
        }

        impl Running {
            fn ping(&self, token: &str) -> reqwest::RequestBuilder {
                self.client
                    .post(format!("{}/mcp", self.base))
                    .bearer_auth(token)
                    .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            }

            fn invoke(&self, command: &str) -> reqwest::RequestBuilder {
                self.client
                    .post(format!("{}/api/invoke/{command}", self.base))
                    .header(CSRF_HEADER, "1")
                    .json(&json!({}))
            }
        }

        async fn status(request: reqwest::RequestBuilder) -> u16 {
            request.send().await.unwrap().status().as_u16()
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_mcp_token_and_the_access_token_never_stand_in_for_each_other() {
            let _serial = SERIAL.lock().await;
            let server = start(false).await;
            let access = server.access.clone().unwrap();

            // The browser signs in with the access token and reads the MCP token from Settings → MCP server.
            let login = server
                .client
                .post(format!("{}/api/login", server.base))
                .form(&[("token", access.as_str())])
                .send()
                .await
                .unwrap();
            assert_eq!(login.status().as_u16(), 303);
            let cookie = login.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned();
            let mcp: String = server
                .invoke("mcp_token")
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(mcp.len(), 64);
            assert_ne!(mcp, access);

            // `/mcp` takes the MCP token only.
            assert_eq!(status(server.ping(&mcp)).await, 200);
            assert_eq!(status(server.ping(&access)).await, 401, "the access token is refused by /mcp");
            assert_eq!(
                status(server.client.post(format!("{}/mcp", server.base)).header("cookie", &cookie)).await,
                401
            );

            // Origin must equal Host.
            assert_eq!(status(server.ping(&mcp).header("origin", &server.origin)).await, 200);
            assert_eq!(status(server.ping(&mcp).header("origin", "http://evil.example")).await, 403);
            let lookalike = format!("{}.evil.example", server.origin);
            assert_eq!(status(server.ping(&mcp).header("origin", lookalike)).await, 403);

            // The MCP token never signs in and never calls /api.
            assert_eq!(status(server.client.get(format!("{}/?token={mcp}", server.base))).await, 401);
            let login = server
                .client
                .post(format!("{}/api/login", server.base))
                .form(&[("token", mcp.as_str())])
                .send()
                .await
                .unwrap();
            assert_eq!(login.headers()[header::LOCATION], "/?error=invalid");
            assert!(!login.headers().contains_key(header::SET_COOKIE));
            assert_eq!(status(server.invoke("mcp_answer").bearer_auth(&mcp)).await, 401);
            assert_eq!(status(server.invoke("save_workspace").bearer_auth(&mcp)).await, 401);
            let forged = format!("{}={mcp}", crate::auth::COOKIE_NAME);
            assert_eq!(status(server.invoke("mcp_status").header("cookie", forged)).await, 401);

            // Export tickets: only the session that asked for one redeems it, once; never the MCP token.
            let prepare = || async {
                server
                    .invoke("export_prepare")
                    .header("cookie", &cookie)
                    .json(&json!({
                        "connectionId": "missing",
                        "request": {
                            "browse": { "object": { "name": "t", "kind": "table" }, "limit": 100 },
                            "format": "csv",
                            "fileName": "t.csv"
                        }
                    }))
                    .send()
                    .await
                    .unwrap()
                    .json::<String>()
                    .await
                    .unwrap()
            };
            let export = |ticket: &str| server.client.get(format!("{}/api/export/{ticket}", server.base));
            let spent = |response: reqwest::Response| async move {
                response.status().as_u16() == 422 && response.text().await.unwrap().contains("expired or was already used")
            };
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let other_login = server
                .client
                .post(format!("{}/api/login", server.base))
                .form(&[("token", access.as_str())])
                .send()
                .await
                .unwrap();
            let other_cookie = other_login.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned();
            assert_ne!(other_cookie, cookie);
            let ticket = prepare().await;
            assert_eq!(ticket.len(), 32);
            assert_eq!(status(export(&ticket).bearer_auth(&mcp)).await, 401);
            let forged = format!("{}={mcp}", crate::auth::COOKIE_NAME);
            assert_eq!(status(export(&ticket).header("cookie", forged)).await, 401);
            assert!(spent(export(&ticket).header("cookie", &other_cookie).send().await.unwrap()).await);
            // The refused attempt spent it.
            assert!(spent(export(&ticket).header("cookie", &cookie).send().await.unwrap()).await);
            let ticket = prepare().await;
            let redeemed = export(&ticket).header("cookie", &cookie).send().await.unwrap();
            assert!(
                !spent(redeemed).await,
                "its own session redeems a ticket (the unknown connection fails after that)"
            );
            assert!(spent(export(&ticket).header("cookie", &cookie).send().await.unwrap()).await);

            // The browser rotates it: the old one stops working at once.
            let rotated: String = server
                .invoke("rotate_mcp_token")
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_ne!(rotated, mcp);
            assert_eq!(status(server.ping(&mcp)).await, 401);
            assert_eq!(status(server.ping(&rotated)).await, 200);

            // Turned off: refused.
            let off = server
                .invoke("set_mcp_settings")
                .header("cookie", &cookie)
                .json(&json!({ "enabled": false, "port": null }));
            assert_eq!(status(off).await, 200);
            assert_eq!(status(server.ping(&rotated)).await, 403);
            let status_body: Value = server
                .invoke("mcp_status")
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(status_body["enabled"], false);
        }

        /// Wrong MCP tokens are limited in a bucket of their own: they lock that client out of `/mcp`, never anyone
        /// out of sign-in (F5 of the 0.2.0 audit).
        #[tokio::test(flavor = "multi_thread")]
        async fn wrong_mcp_tokens_do_not_lock_the_sign_in() {
            let _serial = SERIAL.lock().await;
            let server = start(false).await;
            let access = server.access.clone().unwrap();
            let mcp = server.state.mcp.token().unwrap();
            for _ in 0..crate::auth::MAX_FAILURES_PER_CLIENT {
                assert_eq!(status(server.ping("not-the-mcp-token")).await, 401);
            }
            // This client is now refused at /mcp, right token or not…
            assert_eq!(status(server.ping(&mcp)).await, 401);
            // …but still signs in with the access token, by form and by URL.
            let login = server
                .client
                .post(format!("{}/api/login", server.base))
                .form(&[("token", access.as_str())])
                .send()
                .await
                .unwrap();
            assert_eq!(login.headers()[header::LOCATION], "/");
            assert!(login.headers().contains_key(header::SET_COOKIE));
            let by_url = server.client.get(format!("{}/?token={access}", server.base)).send().await.unwrap();
            assert_eq!(by_url.status().as_u16(), 303);
            assert!(by_url.headers().contains_key(header::SET_COOKIE));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn with_authentication_off_the_mcp_endpoint_refuses_everything() {
            let _serial = SERIAL.lock().await;
            let server = start(true).await;
            let mcp = server.state.mcp.token().unwrap();
            assert_eq!(status(server.ping(&mcp)).await, 401);
            assert_eq!(status(server.ping("anything")).await, 401);
            assert_eq!(
                status(server.client.post(format!("{}/mcp", server.base)).json(&json!({}))).await,
                401
            );
        }
    }
}
