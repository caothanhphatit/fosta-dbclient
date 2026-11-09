//! The gateway on per-engine classification: samples for every registered engine, hard refusals, engine operations.

use serde_json::json;

use super::{rules::Preset, *};
use crate::{
    contract::{testing::FakeEngine, ObjectRef, SqlDialectId},
    runtime::registry,
};

fn context(preset: Preset) -> Context {
    Context {
        workspace_id: "w".into(),
        workspace_name: "Production".into(),
        connection_id: "c".into(),
        connection_name: "Main DB".into(),
        safety: SafetySettings::preset(preset),
    }
}

fn worst(engine: &str, language: &str, text: &str) -> Option<Category> {
    registry()
        .get(engine)
        .unwrap()
        .classify(&ClassifyInput::Statement { language, text })
        .worst()
}

#[test]
#[cfg(feature = "engines-all")]
fn every_engine_classifies_its_default_language() {
    let samples: &[(&str, &str, &str, Category, &str, Category)] = &[
        ("mariadb", "sql", "SELECT 1", Category::Read, "DROP TABLE t", Category::Destructive),
        (
            "mysql",
            "sql",
            "SHOW TABLES",
            Category::Read,
            "DELETE FROM t",
            Category::Destructive,
        ),
        (
            "mssql",
            "sql",
            "SELECT TOP 1 * FROM t",
            Category::Read,
            "TRUNCATE TABLE t",
            Category::Destructive,
        ),
        (
            "mongodb",
            "mongo-command",
            r#"{ "find": "t" }"#,
            Category::Read,
            r#"{ "drop": "t" }"#,
            Category::Destructive,
        ),
        ("postgres", "sql", "SELECT 1", Category::Read, "DROP TABLE t", Category::Destructive),
        ("sqlite", "sql", "SELECT 1", Category::Read, "DROP TABLE t", Category::Destructive),
        (
            "clickhouse",
            "sql",
            "SELECT 1",
            Category::Read,
            "DROP TABLE t",
            Category::Destructive,
        ),
        (
            "oracle",
            "sql",
            "SELECT 1 FROM dual",
            Category::Read,
            "DROP TABLE t",
            Category::Destructive,
        ),
        ("redis", "redis", "GET k", Category::Read, "FLUSHALL", Category::Destructive),
        (
            "elasticsearch",
            "es-console",
            "GET /orders/_search",
            Category::Read,
            "DELETE /orders",
            Category::Destructive,
        ),
        (
            "opensearch",
            "es-console",
            "GET /orders/_search",
            Category::Read,
            "DELETE /orders",
            Category::Destructive,
        ),
    ];
    let path = std::env::temp_dir().join(format!("fosta-gateway-assistant-{}.jsonl", std::process::id()));
    let gateway = Gateway::with_audit(crate::audit::AuditLog::new(path.clone(), 1000));
    let open = context(Preset::Open);
    for (engine, language, read, read_category, danger, danger_category) in samples {
        assert_eq!(worst(engine, language, read), Some(*read_category), "{engine}: {read}");
        assert_eq!(worst(engine, language, danger), Some(*danger_category), "{engine}: {danger}");
        // The assistant's read-only query tool, generic through the registry: the read runs, the dangerous one is refused.
        let found = registry().get(engine).unwrap();
        assert!(assistant::check(&gateway, found, &open, language, read).is_ok(), "{engine}: {read}");
        let refused = assistant::check(&gateway, found, &open, language, danger).unwrap_err();
        assert!(assistant::is_refusal(&refused), "{engine}: {danger}: {refused}");
        let unknown = registry().get(engine).unwrap().classify(&ClassifyInput::Capability {
            name: "nope.nothing",
            args: &json!({}),
        });
        assert!(unknown.refusal().is_some(), "{engine} must refuse unknown operations");
    }
    assert_eq!(
        classify::classify_sql(SqlDialectId::Postgres, "SELECT 1 FROM t").worst(),
        Some(Category::Read)
    );
    let _ = std::fs::remove_file(path);
}

fn authorize(gateway: &Gateway, caller: Caller, preset: Preset, classification: &Classification) -> AppResult<()> {
    let context = context(preset);
    gateway.authorize(&Request {
        caller,
        context: &context,
        engine: "fake",
        scope: "c",
        text: "x",
        classification,
        confirmation: None,
        reusable: false,
        client: None,
    })
}

#[test]
fn refusals_hold_whatever_the_preset_and_the_assistant_only_reads() {
    let path = std::env::temp_dir().join(format!("fosta-gateway-engine-{}.jsonl", std::process::id()));
    let gateway = Gateway::with_audit(crate::audit::AuditLog::new(path.clone(), 1000));
    let refused = Classification::single(StatementInfo::refused("SUBSCRIBE", "SUBSCRIBE ch", "Pub/sub is not supported"));
    let error = authorize(&gateway, Caller::EngineView, Preset::Open, &refused).unwrap_err();
    assert_eq!(error.code(), Some(BLOCKED));
    assert!(error.to_string().contains("Pub/sub"), "{error}");
    let write = Classification::single(StatementInfo::new(Category::Write, "redis.edit", "SET k v"));
    assert!(authorize(&gateway, Caller::EngineView, Preset::Open, &write).is_ok());
    assert_eq!(
        authorize(&gateway, Caller::Assistant, Preset::Open, &write).unwrap_err().code(),
        Some(BLOCKED)
    );
    let read = Classification::single(StatementInfo::new(Category::Read, "redis.info", "INFO"));
    assert!(authorize(&gateway, Caller::Assistant, Preset::ReadOnly, &read).is_ok());
    let audit = crate::audit::AuditLog::new(path.clone(), 1000).read_all().unwrap();
    assert_eq!(audit[0].engine, "fake");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn engine_operations_and_browse_go_through_the_engine() {
    let manager = ConnectionManager::default();
    let engine = FakeEngine::leak();
    manager.insert("c", engine, engine.connection());
    let open = context(Preset::Open);
    // The fake engine answers no operation: the gateway refuses it before the connection sees it.
    let error = invoke(&manager, &open, Caller::EngineView, "fake.op", json!({}), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(BLOCKED));
    let error = invoke(&manager, &open, Caller::QueryTab, "fake.op", json!({}), None)
        .await
        .unwrap_err();
    assert!(error.code().is_none());
    let page = browse(
        &manager,
        &open,
        "c",
        &BrowseRequest::new(ObjectRef::table("shop", None, "items"), 10),
    )
    .await
    .unwrap();
    assert_eq!(page.rows.len(), 1);
    // The fake engine has no raw filter language.
    let mut raw = BrowseRequest::new(ObjectRef::table("shop", None, "items"), 10);
    raw.filter = Filter::Raw {
        language: crate::contract::RawFilterLang::Sql,
        text: "1=1".into(),
    };
    assert_eq!(
        browse(&manager, &open, "c", &raw).await.unwrap_err().code(),
        Some("UNSUPPORTED_CAPABILITY")
    );
    // Edits need the editRows capability, which the fake engine does not offer.
    let set = RowChangeSet {
        object: ObjectRef::table("shop", None, "items"),
        changes: Vec::new(),
    };
    let error = apply_edits(&manager, &open, Caller::GridSave, &set, None, None).await.unwrap_err();
    assert_eq!(error.code(), Some("UNSUPPORTED_CAPABILITY"));
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_stream_through_the_gateway_and_writes_need_confirmation() {
    let manager = ConnectionManager::default();
    let engine = FakeEngine::leak();
    manager.insert("c", engine, engine.connection());
    manager.open_session("c", "tab".into(), Default::default()).await.unwrap();
    let mut events = Vec::new();
    let mut sink = |event: crate::contract::StreamEvent| {
        events.push(event);
        crate::contract::Flow::Continue
    };
    let read = RunRequest {
        text: "SELECT 1".into(),
        ..RunRequest::default()
    };
    let summary = run(&manager, &context(Preset::Guarded), "tab", &read, None, &mut sink)
        .await
        .unwrap();
    assert_eq!(summary.statements, 1);
    assert_eq!(events.len(), 2);
    let write = RunRequest {
        text: "UPDATE t SET a = 1".into(),
        ..RunRequest::default()
    };
    let mut ignore = |_: crate::contract::StreamEvent| crate::contract::Flow::Continue;
    let error = run(&manager, &context(Preset::Guarded), "tab", &write, None, &mut ignore)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(CONFIRMATION_REQUIRED));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_classifies_text_without_running_it() {
    let manager = ConnectionManager::default();
    let engine = FakeEngine::leak();
    manager.insert("c", engine, engine.connection());
    manager.open_session("c", "tab".into(), Default::default()).await.unwrap();
    assert_eq!(session_categories(&manager, "tab", "SELECT 1").unwrap(), [Category::Read]);
    assert_eq!(
        session_categories(&manager, "tab", "UPDATE t SET a = 1").unwrap(),
        [Category::Write]
    );
    // A session the backend no longer has says so by code (the query tab reopens it first).
    let error = session_categories(&manager, "gone", "SELECT 1").unwrap_err();
    assert_eq!(error.code(), Some("SESSION_LOST"));
}
