//! `postgres.explain`: the plan of one statement as `EXPLAIN (FORMAT JSON)`, optionally with `ANALYZE` (which runs the
//! statement, so it runs in a transaction that is rolled back, and the gateway classifies it by the statement inside).

use serde_json::{json, Value as Json};

use super::{map_err, PgEngine};
use crate::{
    contract::{Category, Classification, StatementInfo},
    engines::common::{classify, split::Dialect},
    error::{AppError, AppResult},
};

pub const EXPLAIN: &str = "postgres.explain";
pub const OPERATIONS: &[&str] = &[EXPLAIN];

/// A plain plan only reads; with `analyze` the statement itself runs, so it is classified as `EXPLAIN ANALYZE <statement>`.
pub fn classify_operation(name: &str, args: &Json) -> Classification {
    if name != EXPLAIN {
        return Classification::single(StatementInfo::refused(name, name, "unknown operation"));
    }
    let text = args.get("sql").and_then(Json::as_str).unwrap_or_default();
    if args.get("analyze").and_then(Json::as_bool).unwrap_or(false) {
        classify(Dialect::Postgres, &format!("EXPLAIN ANALYZE {text}")).into()
    } else {
        Classification::single(StatementInfo::new(Category::Read, name, text))
    }
}

impl PgEngine {
    pub(super) async fn explain(&self, args: &Json) -> AppResult<Json> {
        let sql = args
            .get("sql")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| AppError::new("Give the statement to explain as `sql`."))?;
        let analyze = args.get("analyze").and_then(Json::as_bool).unwrap_or(false);
        let options = if analyze { "ANALYZE, BUFFERS, FORMAT JSON" } else { "FORMAT JSON" };
        let mut client = self.client.lock().await;
        // One prepared statement: a second command in `sql` is refused by the server. Always rolled back.
        let tx = client.transaction().await.map_err(map_err)?;
        let result = tx.query_one(&format!("EXPLAIN ({options}) {sql}"), &[]).await;
        let _ = tx.rollback().await;
        let row = result.map_err(map_err)?;
        let value = super::types::row_values(&row).into_iter().next();
        let plan = match value {
            Some(crate::contract::Value::Json(text)) | Some(crate::contract::Value::Text(text)) => {
                serde_json::from_str(&text).unwrap_or(Json::String(text))
            }
            _ => Json::Null,
        };
        Ok(json!({ "plan": plan, "analyze": analyze, "sql": sql }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_reads_but_analyze_is_classified_by_the_statement() {
        let plain = classify_operation(EXPLAIN, &json!({ "sql": "DELETE FROM t" }));
        assert_eq!(plain.worst(), Some(Category::Read));
        let analyze = classify_operation(EXPLAIN, &json!({ "sql": "DELETE FROM t WHERE id = 1", "analyze": true }));
        assert_eq!(analyze.worst(), Some(Category::Write));
        let select = classify_operation(EXPLAIN, &json!({ "sql": "SELECT 1", "analyze": true }));
        assert_eq!(select.worst(), Some(Category::Read));
        assert!(classify_operation("postgres.vacuum", &Json::Null).refusal().is_some());
    }
}
