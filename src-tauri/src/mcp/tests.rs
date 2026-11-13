//! Tests of the MCP server: the protocol over a real loopback socket, the transport guards, the tools' workspace gating
//! and the `run_query` approvals, all on the fake engine; plus a live test against the dev PostgreSQL.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};

use super::{capped, connection_access, http, tools, ClientInfo, McpAccess};
use crate::{
    audit::{AuditEntry, AuditLog, Outcome},
    config::{AppConfig, ConnectionProfile, Workspace},
    contract::testing::FakeEngine,
    gateway::{
        rules::{Action, Preset, SafetySettings, PRODUCTION_ICON},
        Gateway,
    },
    AppState, Capabilities,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn profile(id: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: id.into(),
        name: format!("{id} name"),
        engine: "fake".into(),
        host: "db.internal.example".into(),
        port: 5432,
        user: "secret-user".into(),
        database: Some("shop".into()),
        ..Default::default()
    }
}

fn workspace(id: &str, icon: &str, safety: SafetySettings, mcp: McpAccess, connections: &[&str]) -> Workspace {
    Workspace {
        id: id.into(),
        name: format!("WS {id}"),
        color: String::new(),
        icon: icon.into(),
        safety: Some(safety),
        assistant_queries: Default::default(),
        assistant_learn: Default::default(),
        mcp,
        connections: connections.iter().map(|id| profile(id)).collect(),
        tasks: Vec::new(),
        saved_queries: Vec::new(),
    }
}

/// Workspaces: `dev` (Open, ask before writes: c-dev), `prod` (Production, asks before writes in the file but is capped:
/// c-prod), `ro` (Read-only rules, read-only: c-ro), `hidden` (MCP off: c-hidden), `typed` (writes need a typed
/// confirmation: c-typed), `blocked` (writes blocked: c-blocked).
fn config() -> AppConfig {
    let mut typed = SafetySettings::preset(Preset::Custom);
    typed.rules.write = Action::Typed;
    let mut blocked = SafetySettings::preset(Preset::Custom);
    blocked.rules.write = Action::Block;
    let mut config = AppConfig {
        workspaces: vec![
            workspace(
                "dev",
                "laptop",
                SafetySettings::preset(Preset::Open),
                McpAccess::AskBeforeWrites,
                &["c-dev"],
            ),
            workspace(
                "prod",
                PRODUCTION_ICON,
                SafetySettings::preset(Preset::Guarded),
                McpAccess::AskBeforeWrites,
                &["c-prod"],
            ),
            workspace(
                "ro",
                "laptop",
                SafetySettings::preset(Preset::ReadOnly),
                McpAccess::AskBeforeWrites,
                &["c-ro"],
            ),
            workspace(
                "hidden",
                "laptop",
                SafetySettings::preset(Preset::Open),
                McpAccess::Off,
                &["c-hidden"],
            ),
            workspace("typed", "laptop", typed, McpAccess::AskBeforeWrites, &["c-typed"]),
            workspace("blocked", "laptop", blocked, McpAccess::AskBeforeWrites, &["c-blocked"]),
        ],
        ..AppConfig::default()
    };
    config.active_workspace = "dev".into();
    config.mcp.enabled = true;
    config
}

struct Harness {
    app: Arc<AppState>,
    engine: &'static FakeEngine,
    audit: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        let mut state = AppState::for_test(&format!("mcp-{name}"), Capabilities::desktop(), config());
        let audit = std::env::temp_dir().join(format!("fosta-mcp-audit-{name}-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&audit);
        state.engines.gateway = Gateway::with_audit(AuditLog::new(audit.clone(), 1000));
        let engine = FakeEngine::leak();
        for id in ["c-dev", "c-prod", "c-ro", "c-hidden", "c-typed", "c-blocked"] {
            state.engines.insert(id, engine, engine.connection());
        }
        let app = Arc::new(state);
        app.mcp.attach(&app);
        app.mcp.set_token(TOKEN);
        Self { app, engine, audit }
    }

    fn audit(&self) -> Vec<AuditEntry> {
        AuditLog::new(self.audit.clone(), 1000).read_all().unwrap_or_default()
    }

    fn runs(&self) -> usize {
        self.engine.calls.lock().unwrap().iter().filter(|call| *call == "run").count()
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<String, String> {
        tools::call(&self.app, &ClientInfo::new("claude-code", "2.0"), name, &arguments).await
    }

    /// Waits for the one pending approval.
    async fn pending(&self) -> super::McpApproval {
        for _ in 0..200 {
            if let Some(approval) = self.app.mcp.pending().into_iter().next() {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no approval arrived");
    }
}

// --- Access levels --------------------------------------------------------------------------

#[test]
fn production_and_read_only_rules_cap_the_access() {
    let open = SafetySettings::preset(Preset::Open);
    let read_only = SafetySettings::preset(Preset::ReadOnly);
    assert_eq!(capped(McpAccess::AskBeforeWrites, "laptop", &open), McpAccess::AskBeforeWrites);
    assert_eq!(capped(McpAccess::AskBeforeWrites, PRODUCTION_ICON, &open), McpAccess::ReadOnly);
    assert_eq!(capped(McpAccess::AskBeforeWrites, "laptop", &read_only), McpAccess::ReadOnly);
    assert_eq!(capped(McpAccess::Off, PRODUCTION_ICON, &read_only), McpAccess::Off);
    let config = config();
    assert_eq!(connection_access(&config, "c-dev"), McpAccess::AskBeforeWrites);
    assert_eq!(connection_access(&config, "c-prod"), McpAccess::ReadOnly);
    assert_eq!(connection_access(&config, "c-ro"), McpAccess::ReadOnly);
    assert_eq!(connection_access(&config, "c-hidden"), McpAccess::Off);
    assert_eq!(connection_access(&config, "missing"), McpAccess::Off);
    // A connection's own Read-only rules cap it too.
    let mut own = config.clone();
    own.workspaces[0].connections[0].safety = Some(SafetySettings::preset(Preset::ReadOnly));
    assert_eq!(connection_access(&own, "c-dev"), McpAccess::ReadOnly);
    assert_eq!(McpAccess::default(), McpAccess::Off, "off unless chosen");
    assert!(!AppConfig::default().mcp.enabled, "the server is off by default");
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_a_production_workspace_caps_its_mcp_access() {
    let harness = Harness::new("save");
    let meta = crate::config::WorkspaceMeta {
        id: "prod".into(),
        name: "WS prod".into(),
        color: String::new(),
        icon: PRODUCTION_ICON.into(),
        safety: None,
        assistant_queries: None,
        assistant_learn: None,
        mcp: Some(McpAccess::AskBeforeWrites),
    };
    let saved = crate::commands::workspaces::save_workspace(&harness.app, meta, None).unwrap();
    assert_eq!(saved.workspace("prod").unwrap().mcp, McpAccess::ReadOnly);
}

#[test]
fn client_names_are_cleaned_and_labelled() {
    assert_eq!(ClientInfo::new("claude-code", "1").label, "Claude Code");
    assert_eq!(ClientInfo::new("cursor-vscode", "1").label, "Cursor");
    let odd = ClientInfo::new("evil\u{1b}[2J\nname", "");
    assert!(!odd.name.chars().any(char::is_control), "{odd:?}");
    assert_eq!(ClientInfo::new("", "").name, "unknown");
    assert!(ClientInfo::new(&"x".repeat(500), "").name.len() <= 60);
}

// --- Transport guards -----------------------------------------------------------------------

#[test]
fn only_loopback_peers_hosts_and_origins_pass() {
    assert!(http::peer_allowed(Some(IpAddr::V4(Ipv4Addr::LOCALHOST))));
    assert!(http::peer_allowed(Some(IpAddr::V6(Ipv6Addr::LOCALHOST))));
    assert!(http::peer_allowed(Some("::ffff:127.0.0.1".parse().unwrap())));
    assert!(!http::peer_allowed(Some("192.168.1.20".parse().unwrap())));
    assert!(!http::peer_allowed(Some("::ffff:10.0.0.1".parse().unwrap())));
    assert!(!http::peer_allowed(None));
    assert!(http::is_loopback_name("LOCALHOST") && http::is_loopback_name("[::1]"));
    assert!(!http::is_loopback_name("evil.example"));
    // Desktop: no Origin (CLI clients) or a loopback one.
    assert!(http::origin_allowed(None, "127.0.0.1:3910", true));
    assert!(http::origin_allowed(Some("http://localhost:6274"), "127.0.0.1:3910", true));
    assert!(!http::origin_allowed(Some("https://evil.example"), "127.0.0.1:3910", true));
    assert!(!http::origin_allowed(Some("http://127.0.0.1.evil.example"), "127.0.0.1:3910", true));
    assert!(!http::origin_allowed(Some("null"), "127.0.0.1:3910", true));
    // Web: the server itself only.
    assert!(http::origin_allowed(Some("https://studio.example"), "studio.example", false));
    assert!(!http::origin_allowed(Some("https://evil.example"), "studio.example", false));
}

struct Server {
    base: String,
    client: reqwest::Client,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl Server {
    fn start(harness: &Harness) -> Self {
        let (port, stop) = http::serve_loopback(&tokio::runtime::Handle::current(), Arc::downgrade(&harness.app), 0).expect("listener");
        Self {
            base: format!("http://127.0.0.1:{port}/mcp"),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            _stop: stop,
        }
    }

    fn post(&self, body: &Value, session: Option<&str>) -> reqwest::RequestBuilder {
        let mut request = self.client.post(&self.base).bearer_auth(TOKEN).json(body);
        if let Some(session) = session {
            request = request.header(http::SESSION_HEADER, session);
        }
        request
    }

    async fn send(request: reqwest::RequestBuilder) -> (u16, reqwest::header::HeaderMap, Value) {
        let response = request.send().await.expect("response");
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let text = response.text().await.unwrap_or_default();
        (status, headers, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn initialize(&self) -> String {
        let (status, headers, body) = Self::send(self.post(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "claude-code", "version": "2.0" } } }),
            None,
        ))
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18", "the client's version is echoed");
        assert_eq!(body["result"]["serverInfo"]["name"], "fosta-studio");
        assert!(body["result"]["capabilities"]["tools"].is_object());
        headers[http::SESSION_HEADER].to_str().unwrap().to_owned()
    }

    async fn call(&self, session: &str, id: u64, name: &str, arguments: Value) -> Value {
        let (status, _, body) = Self::send(self.post(
            &json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }),
            Some(session),
        ))
        .await;
        assert_eq!(status, 200, "{body}");
        body
    }
}

fn text_of(body: &Value) -> &str {
    body["result"]["content"][0]["text"].as_str().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_protocol_initializes_lists_and_calls_tools() {
    let harness = Harness::new("protocol");
    let server = Server::start(&harness);
    let session = server.initialize().await;

    let (status, _, _) =
        Server::send(server.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), Some(&session))).await;
    assert_eq!(status, 202, "notifications are accepted without a body");

    let (_, _, list) = Server::send(server.post(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }), Some(&session))).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "list_connections",
            "list_databases",
            "list_tables",
            "describe_table",
            "schema_overview",
            "run_read_query",
            "run_query"
        ]
    );
    assert!(list["result"]["tools"][0]["inputSchema"]["type"] == "object");

    let connections = server.call(&session, 3, "list_connections", json!({})).await;
    let text = text_of(&connections);
    assert_eq!(connections["result"]["isError"], false);
    assert!(
        text.contains("c-dev") && text.contains("c-prod") && text.contains("ask-before-writes"),
        "{text}"
    );
    assert!(!text.contains("c-hidden"), "a workspace without MCP stays invisible: {text}");
    assert!(
        !text.contains("db.internal.example") && !text.contains("secret-user"),
        "no hosts or users: {text}"
    );

    let tables = server.call(&session, 4, "list_tables", json!({ "connection": "c-dev" })).await;
    assert!(
        text_of(&tables).contains("items") && text_of(&tables).contains("<untrusted_data>"),
        "{tables}"
    );

    let rows = server
        .call(
            &session,
            5,
            "run_read_query",
            json!({ "connection": "c-dev", "query": "SELECT * FROM items", "max_rows": 2 }),
        )
        .await;
    let text = text_of(&rows);
    assert_eq!(rows["result"]["isError"], false, "{rows}");
    assert!(
        text.contains("<untrusted_data>") && text.contains("\"rowCount\":2") && text.contains("\"truncated\":true"),
        "{text}"
    );

    let hidden = server.call(&session, 6, "list_tables", json!({ "connection": "c-hidden" })).await;
    assert_eq!(hidden["result"]["isError"], true);
    assert!(text_of(&hidden).contains("Unknown connection"), "{hidden}");

    let unknown = server.call(&session, 7, "drop_everything", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32602);
    let (_, _, method) = Server::send(server.post(&json!({ "jsonrpc": "2.0", "id": 8, "method": "resources/list" }), Some(&session))).await;
    assert_eq!(method["error"]["code"], -32601);
    let (_, _, ping) = Server::send(server.post(&json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" }), Some(&session))).await;
    assert_eq!(ping["result"], json!({}));

    let (status, _, _) = Server::send(server.post(&json!({ "jsonrpc": "2.0", "id": 10, "method": "tools/list" }), None)).await;
    assert_eq!(status, 400, "a session is required after initialize");
    let (status, _, _) = Server::send(server.post(&json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/list" }), Some("nope"))).await;
    assert_eq!(status, 404, "an unknown session makes the client initialize again");

    let delete = server
        .client
        .delete(&server.base)
        .bearer_auth(TOKEN)
        .header(http::SESSION_HEADER, &session);
    assert_eq!(Server::send(delete).await.0, 204);
    let (status, _, _) = Server::send(server.post(&json!({ "jsonrpc": "2.0", "id": 12, "method": "tools/list" }), Some(&session))).await;
    assert_eq!(status, 404);

    let audit = harness.audit();
    assert!(
        audit
            .iter()
            .any(|entry| entry.caller == "mcp" && entry.client == "claude-code" && entry.statement.starts_with("list_tables")),
        "{audit:?}"
    );
    assert!(audit
        .iter()
        .any(|entry| entry.caller == "mcp" && entry.outcome == Some(Outcome::Ran) && entry.detail.contains("2 row(s)")));
    assert!(harness.app.mcp.calls() >= 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_without_the_token_or_from_elsewhere_are_refused() {
    let harness = Harness::new("auth");
    let server = Server::start(&harness);
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });

    let response = server.client.post(&server.base).json(&ping).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 401);
    assert!(response.headers().contains_key("www-authenticate"));
    let wrong = server.client.post(&server.base).bearer_auth("wrong").json(&ping);
    assert_eq!(Server::send(wrong).await.0, 401);
    let basic = server
        .client
        .post(&server.base)
        .header("authorization", format!("Basic {TOKEN}"))
        .json(&ping);
    assert_eq!(Server::send(basic).await.0, 401);

    // DNS rebinding: a page on another name, or another origin, is refused even with the token.
    assert_eq!(Server::send(server.post(&ping, None).header("host", "evil.example")).await.0, 403);
    assert_eq!(
        Server::send(server.post(&ping, None).header("origin", "https://evil.example"))
            .await
            .0,
        403
    );
    assert_eq!(
        Server::send(server.post(&ping, None).header("origin", "http://localhost:6274"))
            .await
            .0,
        200
    );

    assert_eq!(Server::send(server.client.get(&server.base).bearer_auth(TOKEN)).await.0, 405);
    assert_eq!(
        Server::send(server.post(&ping, None).header(http::VERSION_HEADER, "1999-01-01"))
            .await
            .0,
        400
    );
    let (status, _, body) = Server::send(server.client.post(&server.base).bearer_auth(TOKEN).body("{not json")).await;
    assert_eq!((status, body["error"]["code"].as_i64()), (400, Some(-32700)));
    let (status, _, _) = Server::send(server.post(&json!([ping]), None)).await;
    assert_eq!(status, 400, "batches are not supported");

    harness
        .app
        .config
        .update(|config| {
            config.mcp.enabled = false;
            Ok(())
        })
        .unwrap();
    assert_eq!(Server::send(server.post(&ping, None)).await.0, 403, "off in Settings");
}

// --- Tools: gating, read-only enforcement, approvals --------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn production_and_read_only_workspaces_never_run_writes() {
    let harness = Harness::new("caps");
    for connection in ["c-prod", "c-ro"] {
        let refused = harness
            .call(
                "run_query",
                json!({ "connection": connection, "query": "UPDATE items SET name = 'x'" }),
            )
            .await
            .unwrap_err();
        assert!(refused.contains("read-only"), "{refused}");
        // Reads still work there.
        assert!(harness
            .call(
                "run_read_query",
                json!({ "connection": connection, "query": "SELECT * FROM items" })
            )
            .await
            .is_ok());
    }
    assert!(harness.app.mcp.pending().is_empty(), "nothing was offered for approval");
    let audit = harness.audit();
    assert!(
        audit.iter().any(|entry| entry.caller == "mcp"
            && entry.connection_id == "c-prod"
            && entry.outcome == Some(Outcome::Blocked)
            && entry.client == "claude-code"),
        "{audit:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_read_query_runs_only_reads() {
    let harness = Harness::new("reads");
    let refused = harness
        .call(
            "run_read_query",
            json!({ "connection": "c-dev", "query": "UPDATE items SET name = 'x'" }),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("only read-only statements"), "{refused}");
    assert_eq!(harness.runs(), 0, "the write never reached a session");
    let bad = harness
        .call(
            "run_read_query",
            json!({ "connection": "c-dev", "query": "SELECT * FROM items", "max_rows": 501 }),
        )
        .await
        .unwrap_err();
    assert!(bad.contains("max_rows"), "{bad}");
    let ok = harness
        .call("run_read_query", json!({ "connection": "c-dev", "query": "SELECT * FROM items" }))
        .await
        .unwrap();
    assert!(ok.contains("\"columns\":[\"id\",\"name\"]"), "{ok}");
    let audit = harness.audit();
    assert!(audit
        .iter()
        .any(|entry| entry.caller == "mcp" && entry.outcome == Some(Outcome::Blocked) && entry.category == "write"));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_query_waits_for_the_user_and_runs_once_approved() {
    let harness = Harness::new("approve");
    let app = Arc::clone(&harness.app);
    let call = tokio::spawn(async move {
        tools::call(
            &app,
            &ClientInfo::new("claude-code", "2.0"),
            "run_query",
            &json!({ "connection": "c-dev", "query": "UPDATE items SET name = 'x'" }),
        )
        .await
    });
    let approval = harness.pending().await;
    assert_eq!(approval.client_label, "Claude Code");
    assert_eq!(approval.statement, "UPDATE items SET name = 'x'");
    assert!(
        approval.problem["token"].is_null(),
        "the token stays in the backend: {}",
        approval.problem
    );
    assert_eq!(approval.problem["level"], "confirm", "Open rules still ask for MCP writes");
    assert_eq!(harness.runs(), 0, "nothing runs before the answer");
    harness.app.mcp.answer(&approval.id, true, None).unwrap();
    let result = call.await.unwrap().unwrap();
    assert!(result.contains("The statement ran"), "{result}");
    assert_eq!(harness.runs(), 1);
    assert!(
        harness.app.mcp.answer(&approval.id, true, None).is_err(),
        "an approval is answered once"
    );
    let audit = harness.audit();
    let mcp: Vec<&AuditEntry> = audit
        .iter()
        .filter(|entry| entry.caller == "mcp" && entry.client == "claude-code")
        .collect();
    assert!(mcp.iter().any(|entry| entry.outcome == Some(Outcome::Confirmed)), "{audit:?}");
    assert!(
        mcp.iter()
            .any(|entry| entry.outcome == Some(Outcome::Ran) && entry.category == "write"),
        "{audit:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_query_is_denied_by_the_user_or_on_timeout() {
    let harness = Harness::new("deny");
    let app = Arc::clone(&harness.app);
    let query = json!({ "connection": "c-dev", "query": "DELETE FROM items" });
    let arguments = query.clone();
    let call = tokio::spawn(async move { tools::call(&app, &ClientInfo::new("cursor", "1"), "run_query", &arguments).await });
    let approval = harness.pending().await;
    harness.app.mcp.answer(&approval.id, false, None).unwrap();
    let denied = call.await.unwrap().unwrap_err();
    assert!(denied.contains("denied"), "{denied}");

    harness.app.mcp.set_approval_timeout(Duration::from_millis(50));
    let timed_out = harness.call("run_query", query).await.unwrap_err();
    assert!(timed_out.contains("Nobody approved"), "{timed_out}");
    assert!(harness.app.mcp.pending().is_empty(), "a timed-out approval is withdrawn");
    assert_eq!(harness.runs(), 0);
    let blocked: Vec<String> = harness
        .audit()
        .into_iter()
        .filter(|entry| entry.caller == "mcp" && entry.outcome == Some(Outcome::Blocked))
        .map(|entry| entry.detail)
        .collect();
    assert!(blocked.iter().any(|detail| detail == "denied by the user"), "{blocked:?}");
    assert!(blocked.iter().any(|detail| detail == "no answer in time"), "{blocked:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_rules_need_the_name_and_blocked_rules_never_ask() {
    let harness = Harness::new("typed");
    let blocked = harness
        .call(
            "run_query",
            json!({ "connection": "c-blocked", "query": "UPDATE items SET name = 'x'" }),
        )
        .await
        .unwrap_err();
    assert!(blocked.contains("block"), "{blocked}");
    assert!(harness.app.mcp.pending().is_empty());

    // The fake engine reports no tables, so the workspace name is what must be typed.
    for (typed, ok) in [("wrong", false), ("WS typed", true)] {
        let app = Arc::clone(&harness.app);
        let call = tokio::spawn(async move {
            tools::call(
                &app,
                &ClientInfo::new("zed", "1"),
                "run_query",
                &json!({ "connection": "c-typed", "query": "UPDATE items SET name = 'x'" }),
            )
            .await
        });
        let approval = harness.pending().await;
        assert_eq!(approval.problem["level"], "typed");
        assert_eq!(approval.problem["expected"], "WS typed");
        harness.app.mcp.answer(&approval.id, true, Some(typed.into())).unwrap();
        assert_eq!(call.await.unwrap().is_ok(), ok, "typed {typed}");
    }
    assert_eq!(harness.runs(), 1, "only the matching name ran");
}

/// Needs the dev PostgreSQL (`127.0.0.1:5432`, postgres / studio-root, database `shop`). Runs on one thread so the
/// test secret store holds the saved password; the connection is opened on demand by the first tool call.
#[tokio::test]
#[ignore = "needs the dev Postgres on 127.0.0.1:5432 (dev/compose)"]
async fn live_postgres_over_http() {
    let _store = crate::secrets::testing::memory_store(&[("conn:live-pg:password", "studio-root")]);
    let mut config = config();
    config.workspaces[0].connections = vec![ConnectionProfile {
        id: "live-pg".into(),
        name: "Live Postgres".into(),
        engine: "postgres".into(),
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        database: Some("shop".into()),
        save_password: true,
        ..Default::default()
    }];
    let state = AppState::for_test("mcp-live", Capabilities::desktop(), config);
    let app = Arc::new(state);
    app.mcp.attach(&app);
    app.mcp.set_token(TOKEN);
    let harness = Harness {
        app,
        engine: FakeEngine::leak(),
        audit: PathBuf::new(),
    };
    let server = Server::start(&harness);
    let session = server.initialize().await;
    let tables = server
        .call(&session, 2, "list_tables", json!({ "connection": "live-pg", "database": "shop" }))
        .await;
    assert_eq!(tables["result"]["isError"], false, "{tables}");
    assert!(!text_of(&tables).trim().is_empty());
    let rows = server
        .call(
            &session,
            3,
            "run_read_query",
            json!({ "connection": "live-pg", "query": "SELECT 1 AS one, 'two' AS two" }),
        )
        .await;
    assert_eq!(rows["result"]["isError"], false, "{rows}");
    assert!(text_of(&rows).contains("\"columns\":[\"one\",\"two\"]"), "{rows}");
    let write = server
        .call(
            &session,
            4,
            "run_read_query",
            json!({ "connection": "live-pg", "query": "CREATE TABLE mcp_should_not_exist (id int)" }),
        )
        .await;
    assert_eq!(write["result"]["isError"], true, "{write}");
    harness.app.engines.disconnect("live-pg").await;
}

// --- Approvals that outlive a settings change ---------------------------------------------------

/// Starts `run_query` on c-dev in the background and waits for its approval.
async fn pending_write(harness: &Harness) -> (tokio::task::JoinHandle<Result<String, String>>, super::McpApproval) {
    let app = Arc::clone(&harness.app);
    let call = tokio::spawn(async move {
        tools::call(
            &app,
            &ClientInfo::new("claude-code", "2.0"),
            "run_query",
            &json!({ "connection": "c-dev", "query": "UPDATE items SET name = 'x'" }),
        )
        .await
    });
    let approval = harness.pending().await;
    (call, approval)
}

fn change(harness: &Harness, edit: impl FnOnce(&mut AppConfig)) {
    harness
        .app
        .config
        .update(|config| {
            edit(config);
            Ok(())
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_after_the_server_was_turned_off_runs_nothing() {
    let harness = Harness::new("approve-off");
    let (call, approval) = pending_write(&harness).await;
    change(&harness, |config| config.mcp.enabled = false);
    harness.app.mcp.answer(&approval.id, true, None).unwrap();
    let refused = call.await.unwrap().unwrap_err();
    assert!(refused.contains("turned off"), "{refused}");
    assert_eq!(harness.runs(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_after_the_rules_were_tightened_runs_nothing() {
    let harness = Harness::new("approve-tight");
    // Writes now blocked.
    let (call, approval) = pending_write(&harness).await;
    change(&harness, |config| {
        let mut blocked = SafetySettings::preset(Preset::Custom);
        blocked.rules.write = Action::Block;
        config.workspaces[0].safety = Some(blocked);
    });
    harness.app.mcp.answer(&approval.id, true, None).unwrap();
    let refused = call.await.unwrap().unwrap_err();
    assert!(refused.contains("block"), "{refused}");

    // Writes now need the name typed: the plain approval no longer holds.
    change(&harness, |config| {
        config.workspaces[0].safety = Some(SafetySettings::preset(Preset::Open))
    });
    let (call, approval) = pending_write(&harness).await;
    change(&harness, |config| {
        let mut typed = SafetySettings::preset(Preset::Custom);
        typed.rules.write = Action::Typed;
        config.workspaces[0].safety = Some(typed);
    });
    harness.app.mcp.answer(&approval.id, true, None).unwrap();
    let refused = call.await.unwrap().unwrap_err();
    assert!(refused.contains("rules changed"), "{refused}");

    // The workspace is now read-only for MCP.
    change(&harness, |config| {
        config.workspaces[0].safety = Some(SafetySettings::preset(Preset::Open))
    });
    let (call, approval) = pending_write(&harness).await;
    change(&harness, |config| config.workspaces[0].mcp = McpAccess::ReadOnly);
    harness.app.mcp.answer(&approval.id, true, None).unwrap();
    let refused = call.await.unwrap().unwrap_err();
    assert!(refused.contains("access changed to read-only"), "{refused}");

    assert_eq!(harness.runs(), 0, "nothing ran after any change");
    let audit = harness.audit();
    assert!(
        audit
            .iter()
            .any(|entry| entry.caller == "mcp" && entry.outcome == Some(Outcome::Blocked) && entry.detail.contains("access changed")),
        "{audit:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_the_server_off_withdraws_pending_approvals_and_ends_sessions() {
    let harness = Harness::new("withdraw");
    harness.app.mcp.open_session(ClientInfo::new("zed", "1"), "2025-06-18").unwrap();
    let (call, _) = pending_write(&harness).await;
    change(&harness, |config| config.mcp.enabled = false);
    harness.app.mcp.apply().unwrap();
    let refused = call.await.unwrap().unwrap_err();
    assert!(refused.contains("Withdrawn"), "{refused}");
    assert!(harness.app.mcp.pending().is_empty());
    assert_eq!(harness.app.mcp.session_count(), 0);
    assert_eq!(harness.runs(), 0);
}

/// One thread, so the test secret store holds the rotated token.
#[tokio::test]
async fn rotating_the_token_withdraws_pending_approvals_and_ends_sessions() {
    let store = crate::secrets::testing::memory_store(&[]);
    let harness = Harness::new("rotate");
    harness.app.mcp.open_session(ClientInfo::new("zed", "1"), "2025-06-18").unwrap();
    let (call, _) = pending_write(&harness).await;
    let token = harness.app.mcp.rotate_token().unwrap();
    assert_ne!(token, TOKEN);
    assert_eq!(store.accounts(), ["app:mcp-token"]);
    assert!(call.await.unwrap().unwrap_err().contains("Withdrawn"));
    assert_eq!(harness.app.mcp.session_count(), 0);
    assert!(harness.app.mcp.is_token(&token) && !harness.app.mcp.is_token(TOKEN));
}

// --- Limits -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn statements_longer_than_the_dialog_shows_are_refused() {
    let harness = Harness::new("long");
    let query = format!("UPDATE items SET name = '{}'", "x".repeat(tools::MAX_STATEMENT_CHARS));
    for tool in ["run_query", "run_read_query"] {
        let refused = harness
            .call(tool, json!({ "connection": "c-dev", "query": query }))
            .await
            .unwrap_err();
        assert!(refused.contains("longer than 20000 characters"), "{refused}");
    }
    assert!(harness.app.mcp.pending().is_empty(), "never offered for approval");
    assert_eq!(harness.runs(), 0);
    // At the cap it is offered, in full.
    let statement = format!("UPDATE items SET a='{}'", "x".repeat(tools::MAX_STATEMENT_CHARS - 21));
    assert_eq!(statement.chars().count(), tools::MAX_STATEMENT_CHARS);
    let app = Arc::clone(&harness.app);
    let arguments = json!({ "connection": "c-dev", "query": statement });
    let call = tokio::spawn(async move { tools::call(&app, &ClientInfo::new("zed", "1"), "run_query", &arguments).await });
    let approval = harness.pending().await;
    assert_eq!(approval.statement, statement);
    harness.app.mcp.answer(&approval.id, false, None).unwrap();
    assert!(call.await.unwrap().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_max_pending_approvals_wait_at_once() {
    let harness = Harness::new("max-pending");
    let mut calls = Vec::new();
    for _ in 0..super::MAX_PENDING {
        let app = Arc::clone(&harness.app);
        calls.push(tokio::spawn(async move {
            tools::call(
                &app,
                &ClientInfo::new("zed", "1"),
                "run_query",
                &json!({ "connection": "c-dev", "query": "DELETE FROM items" }),
            )
            .await
        }));
    }
    for _ in 0..500 {
        if harness.app.mcp.pending().len() == super::MAX_PENDING {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(harness.app.mcp.pending().len(), super::MAX_PENDING);
    let refused = harness
        .call("run_query", json!({ "connection": "c-dev", "query": "DELETE FROM items" }))
        .await
        .unwrap_err();
    assert!(refused.contains("too many statements"), "{refused}");
    harness.app.mcp.withdraw_all();
    for call in calls {
        assert!(call.await.unwrap().is_err());
    }
    assert_eq!(harness.runs(), 0);
    assert!(harness
        .audit()
        .iter()
        .any(|entry| entry.outcome == Some(Outcome::Blocked) && entry.detail.contains("too many statements")));
}

#[tokio::test]
async fn at_most_max_sessions_are_kept_and_idle_ones_expire() {
    let harness = Harness::new("sessions");
    let mcp = &harness.app.mcp;
    let first = mcp.open_session(ClientInfo::new("zed", "1"), "2025-06-18").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let mut last = String::new();
    for _ in 0..super::MAX_SESSIONS {
        last = mcp.open_session(ClientInfo::new("cursor", "1"), "2025-06-18").unwrap();
    }
    assert_eq!(mcp.session_count(), super::MAX_SESSIONS);
    assert!(mcp.session(&first).is_none(), "the least recently used one was dropped");
    assert!(mcp.session(&last).is_some());

    mcp.set_session_idle(Duration::from_millis(30));
    std::thread::sleep(Duration::from_millis(60));
    assert!(mcp.session(&last).is_none(), "an idle session expires");
    assert_eq!(mcp.session_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_session_answers_404_over_http() {
    let harness = Harness::new("idle-http");
    let server = Server::start(&harness);
    let session = server.initialize().await;
    harness.app.mcp.set_session_idle(Duration::from_millis(30));
    tokio::time::sleep(Duration::from_millis(60)).await;
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let (status, _, _) = Server::send(server.post(&list, Some(&session))).await;
    assert_eq!(status, 404);
}

// --- Audit of refusals -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn every_tools_call_is_audited_also_when_refused() {
    let harness = Harness::new("audit-all");
    let server = Server::start(&harness);
    let session = server.initialize().await;
    let rpc = |id: u64, params: Value| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params });
    for (id, params) in [
        (2, json!({ "arguments": {} })),
        (3, json!({ "name": "drop_everything\u{1b}[2J", "arguments": {} })),
        (4, json!({ "name": "list_tables", "arguments": [1, 2] })),
    ] {
        let (_, _, body) = Server::send(server.post(&rpc(id, params), Some(&session))).await;
        assert_eq!(body["error"]["code"], -32602, "{body}");
    }
    assert!(harness.call("run_read_query", json!({ "connection": "c-dev" })).await.is_err());
    assert!(harness
        .call(
            "run_read_query",
            json!({ "connection": "c-dev", "query": "SELECT 1", "max_rows": 0 })
        )
        .await
        .is_err());
    assert!(harness.call("list_tables", json!({})).await.is_err());

    let audit: Vec<AuditEntry> = harness.audit().into_iter().filter(|entry| entry.caller == "mcp").collect();
    let refused = |statement: &str, detail: &str| {
        audit.iter().any(|entry| {
            entry.outcome == Some(Outcome::Blocked)
                && entry.client == "claude-code"
                && entry.statement.starts_with(statement)
                && entry.detail.contains(detail)
        })
    };
    assert!(refused("tools/call", "needs `name`"), "{audit:?}");
    assert!(refused("drop_everything[2J", "Unknown tool"), "the name is cleaned: {audit:?}");
    assert!(refused("list_tables", "must be an object"), "{audit:?}");
    assert!(refused("run_read_query", "`query` is required"), "{audit:?}");
    assert!(refused("run_read_query", "max_rows"), "{audit:?}");
    assert!(refused("list_tables", "`connection` is required"), "{audit:?}");
    assert_eq!(harness.app.mcp.calls(), 6);
}

// --- Workspace commands ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn mcp_never_starts_a_workspace_command() {
    let harness = Harness::new("command");
    let def = crate::connection_command::testing::command("pf", "fosta-no-such-program", &[], None, 5);
    change(&harness, |config| {
        let mut with_command = workspace("cmd", "laptop", SafetySettings::preset(Preset::Open), McpAccess::ReadOnly, &[]);
        with_command.tasks = vec![def.clone()];
        with_command.connections = vec![ConnectionProfile {
            command: Some("pf".into()),
            ..profile("c-cmd")
        }];
        config.workspaces.push(with_command);
    });
    let refused = harness.call("list_tables", json!({ "connection": "c-cmd" })).await.unwrap_err();
    assert!(refused.contains("is not running") && refused.contains("never start"), "{refused}");
    assert!(!harness.app.tasks.is_active("pf"), "the command was not started");
    assert_eq!(harness.app.tasks.held_by("c-cmd"), None);
}

// --- The stdio bridge -------------------------------------------------------------------------------

#[test]
fn the_bridge_refuses_to_run_while_the_server_is_off() {
    let mut config = config();
    assert!(super::bridge::require_enabled(&config).is_ok());
    config.mcp.enabled = false;
    let refused = super::bridge::require_enabled(&config).unwrap_err();
    assert!(refused.contains("the MCP server is off"), "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bridge_initializes_again_after_a_404() {
    let harness = Harness::new("bridge");
    let server = Server::start(&harness);
    let relay = super::bridge::Relay::new(server.base.clone(), TOKEN.to_owned()).unwrap();
    let (output, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    let initialize = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "claude-ai", "version": "1" } } });
    relay.forward(initialize, &output).await;
    let answer: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
    assert_eq!(answer["result"]["serverInfo"]["name"], "fosta-studio", "{answer}");
    let first = relay.session().expect("a session");

    // The app restarted (or the session expired): the next request gets 404 and the bridge initializes again.
    assert!(harness.app.mcp.close_session(&first));
    relay
        .forward(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }), &output)
        .await;
    let answer: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
    assert!(answer["result"]["tools"].is_array(), "{answer}");
    let second = relay.session().expect("a new session");
    assert_ne!(first, second);
    assert!(harness.app.mcp.session(&second).is_some());

    // A wrong token is a clear JSON-RPC error.
    let wrong = super::bridge::Relay::new(server.base.clone(), "wrong".to_owned()).unwrap();
    wrong.forward(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }), &output).await;
    let answer: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
    assert_eq!(answer["error"]["code"], -32000, "{answer}");
}

#[test]
fn run_read_query_rows_are_redacted_like_the_assistants() {
    use crate::contract::{ColumnInfo, Value as Cell, ValueKind};
    let column = |name: &str| ColumnInfo::result(name, "text", ValueKind::Text);
    let result = crate::gateway::assistant::ReadResult {
        columns: vec![column("email"), column("user_pass"), column("note")],
        rows: vec![vec![
            Cell::Text("ann@example.com".into()),
            Cell::Text("$P$BxvVKjpM3qHo1rBv4bAEHdN8E1GCNb.".into()),
            Cell::Text("key sk-abcdefghijklmnopqrstuvwx1234".into()),
        ]],
        truncated: false,
        affected: None,
        elapsed_ms: 1,
    };
    let redacted = tools::format_result(&result, 100, true);
    assert!(
        !redacted.contains("$P$Bxv") && !redacted.contains("sk-abc") && redacted.contains("ann@example.com"),
        "{redacted}"
    );
    // What the user approved with run_query is returned as it is.
    let approved = tools::format_result(&result, 100, false);
    assert!(approved.contains("$P$Bxv"), "{approved}");
}
