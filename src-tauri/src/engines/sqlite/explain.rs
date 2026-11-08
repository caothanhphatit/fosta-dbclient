//! `sqlite.explain`: the query plan of one statement (`EXPLAIN QUERY PLAN`, which never runs it).

use rusqlite::Connection;
use serde_json::{json, Value as Json};

use super::{map_err, SqliteEngine};
use crate::{
    contract::{Category, Classification, StatementInfo},
    error::{AppError, AppResult},
};

pub const EXPLAIN: &str = "sqlite.explain";
pub const OPERATIONS: &[&str] = &[EXPLAIN];

/// A plan only reads, whatever the statement is.
pub fn classify_operation(name: &str, args: &Json) -> Classification {
    if name != EXPLAIN {
        return Classification::single(StatementInfo::refused(name, name, "unknown operation"));
    }
    let text = args.get("sql").and_then(Json::as_str).unwrap_or_default();
    Classification::single(StatementInfo::new(Category::Read, name, text))
}

fn plan(c: &Connection, sql: &str) -> AppResult<Json> {
    // `prepare` takes one statement; a second one is an error, so nothing stacked can run.
    let mut statement = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).map_err(map_err)?;
    let rows = statement
        .query_map([], |row| {
            Ok(json!({ "id": row.get::<_, i64>(0)?, "parent": row.get::<_, i64>(1)?, "detail": row.get::<_, String>(3)? }))
        })
        .map_err(map_err)?;
    Ok(Json::Array(rows.collect::<std::result::Result<_, _>>().map_err(map_err)?))
}

impl SqliteEngine {
    pub(super) async fn explain(&self, args: &Json) -> AppResult<Json> {
        let sql = args
            .get("sql")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| AppError::new("Give the statement to explain as `sql`."))?
            .to_owned();
        let conn = self.conn.clone();
        let text = sql.clone();
        let plan = tokio::task::spawn_blocking(move || plan(&super::lock(&conn), &text))
            .await
            .map_err(|error| AppError::new(format!("The worker thread failed: {error}")))??;
        Ok(json!({ "plan": plan, "sql": sql }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_lists_its_steps_and_never_stacks_statements() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        let steps = plan(&c, "SELECT * FROM t WHERE id = 1").unwrap();
        assert!(steps[0]["detail"].as_str().unwrap().contains("SEARCH t"), "{steps}");
        assert!(plan(&c, "SELECT 1; DROP TABLE t").is_err());
        assert!(c.prepare("SELECT * FROM t").is_ok(), "the table is still there");
        assert_eq!(
            classify_operation(EXPLAIN, &json!({ "sql": "DROP TABLE t" })).worst(),
            Some(Category::Read)
        );
        assert!(classify_operation("sqlite.vacuum", &Json::Null).refusal().is_some());
    }
}
