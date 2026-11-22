//! Schema compare: three stateless commands. `schema_snapshot` is a metadata read of one side (like `describe_object`
//! and `schema_graph`: straight to the engine, contained, not gateway-classified); `schema_diff` and
//! `schema_sync_script` are pure over the snapshots the UI sends back, so changing an option re-diffs without reading
//! the databases again. The script is text only: the UI opens it in a query tab on the target, where the gateway
//! applies the workspace's rules.

use std::time::Duration;

use super::objects::metadata_context;
use crate::{
    compare,
    contract::{
        require, unsupported_capability, Capability, CompareOptions, SchemaDialect, SchemaDiff, SchemaSnapshot, SnapshotScope, SyncScript,
    },
    error::{AppError, AppResult},
    runtime::registry,
    AppState,
};

/// How long a cancelled snapshot read may take to stop on the server before its task is aborted.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Everything schema compare reads about one scope (a database, and a schema for engines with schemas). With
/// `request_id`, `cancel_request` stops it.
pub async fn schema_snapshot(
    state: &AppState,
    connection_id: String,
    scope: SnapshotScope,
    request_id: Option<String>,
) -> AppResult<SchemaSnapshot> {
    if scope.database.trim().is_empty() {
        return Err(AppError::new("Choose a database to compare"));
    }
    let handle = state.engines.handle(&connection_id)?;
    require(handle.conn.capabilities(), Capability::SchemaCompare)?;
    let (cancel, _registered) = state.engines.requests.register(request_id.as_deref());
    let ctx = metadata_context(state, &connection_id).with_cancel(cancel.clone());
    handle
        .call_cancellable("read the schema for compare", cancel, CANCEL_GRACE, |conn| async move {
            let source = conn.schema_snapshot().ok_or_else(|| unsupported_capability("schema compare"))?;
            source.snapshot(&scope, &ctx).await
        })
        .await
}

/// The target's dialect, after checking that the two snapshots may be compared.
fn dialect(source: &SchemaSnapshot, target: &SchemaSnapshot) -> AppResult<&'static dyn SchemaDialect> {
    compare::check_compatible(source, target)?;
    registry()
        .get(&target.engine)?
        .schema_dialect()
        .ok_or_else(|| unsupported_capability("schema compare"))
}

/// What would make `target` match `source`.
pub fn schema_diff(_state: &AppState, source: SchemaSnapshot, target: SchemaSnapshot, options: CompareOptions) -> AppResult<SchemaDiff> {
    let dialect = dialect(&source, &target)?;
    Ok(compare::diff(&source, &target, &options, dialect))
}

/// The sync script for the diff items in `include` (their ids), in the target's dialect. Never run here.
pub fn schema_sync_script(
    _state: &AppState,
    source: SchemaSnapshot,
    target: SchemaSnapshot,
    options: CompareOptions,
    include: Vec<String>,
) -> AppResult<SyncScript> {
    let dialect = dialect(&source, &target)?;
    let generated_at = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    Ok(compare::sync_script(&source, &target, &options, &include, dialect, &generated_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ColumnDef, TableDef};

    fn snapshot(engine: &str, schema: &str, tables: Vec<TableDef>) -> SchemaSnapshot {
        SchemaSnapshot {
            engine: engine.into(),
            server_version: "17.2".into(),
            scope: SnapshotScope::new("shop", Some(schema)),
            case_insensitive_names: false,
            tables,
            views: Vec::new(),
            skipped: Vec::new(),
            read_at: String::new(),
        }
    }

    #[test]
    fn engines_must_match_and_have_a_dialect() {
        let pg = snapshot("postgres", "a", Vec::new());
        assert!(dialect(&pg, &snapshot("postgres", "b", Vec::new())).is_ok());
        let error = dialect(&pg, &snapshot("sqlite", "main", Vec::new())).err().unwrap();
        assert!(error.to_string().contains("same engine"), "{error}");
        let error = dialect(&snapshot("redis", "a", Vec::new()), &snapshot("redis", "b", Vec::new()))
            .err()
            .unwrap();
        assert!(error.to_string().to_lowercase().contains("schema compare"), "{error}");
    }

    #[test]
    fn the_pure_commands_diff_and_script_postgres_snapshots() {
        let table = TableDef {
            columns: vec![ColumnDef::new("id", "integer", false)],
            ..TableDef::new("orders")
        };
        let source = snapshot("postgres", "compare_a", vec![table]);
        let target = snapshot("postgres", "compare_b", Vec::new());
        let options = CompareOptions::default();
        let diff = compare::diff(&source, &target, &options, dialect(&source, &target).unwrap());
        assert_eq!(diff.items.len(), 1);
        let script = compare::sync_script(
            &source,
            &target,
            &options,
            &["table:orders".to_owned()],
            dialect(&source, &target).unwrap(),
            "now",
        );
        assert!(script.sql.contains("CREATE TABLE compare_b.orders"), "{}", script.sql);
        assert!(script.transactional && script.statements == 1, "{script:?}");
    }
}
