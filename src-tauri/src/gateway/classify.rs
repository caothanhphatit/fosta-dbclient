//! Statement classification for the gateway. The SQL classifier lives in `engines::common::sqlclassify` (engines
//! classify their own statements with it) and is re-exported here; MongoDB database commands are classified by their
//! name, and the old `apply_changes` command's row changes as writes to one table.

use crate::drivers::types::RowChange;

pub use crate::contract::classify::{Category, Classification, StatementInfo};
pub use crate::engines::common::sqlclassify::{classify_sql, dialect_for, excerpt, normalise_executable_comments, parse};

/// Grid saves and imports (the old `apply_changes` command): the app builds `INSERT` / `UPDATE … WHERE key LIMIT 1` /
/// `DELETE … WHERE key LIMIT 1` from these, so they are writes to one table.
pub fn classify_changes(table: &str, changes: &[RowChange]) -> Classification {
    let count = |wanted: fn(&RowChange) -> bool| changes.iter().filter(|change| wanted(change)).count();
    let parts = [
        ("INSERT", count(|change| matches!(change, RowChange::Insert { .. }))),
        ("UPDATE", count(|change| matches!(change, RowChange::Update { .. }))),
        ("DELETE", count(|change| matches!(change, RowChange::Delete { .. }))),
    ];
    Classification {
        statements: parts
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(kind, count)| StatementInfo {
                category: Category::Write,
                kind: (*kind).to_owned(),
                tables: vec![table.to_owned()],
                excerpt: format!("{kind} {count} row(s) of {table}, each matched by its key"),
                reason: None,
                refused: None,
            })
            .collect(),
    }
}

/// MongoDB database command names by category; anything else is [`Category::Unknown`].
const MONGO_READ: [&str; 31] = [
    "find",
    "aggregate",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "dbStats",
    "collStats",
    "dataSize",
    "explain",
    "getMore",
    "killCursors",
    "buildInfo",
    "hello",
    "isMaster",
    "ismaster",
    "ping",
    "serverStatus",
    "connectionStatus",
    "hostInfo",
    "validate",
    "getParameter",
    "currentOp",
    "listCommands",
    "replSetGetStatus",
    "getLog",
    "top",
    "whatsmyuri",
    "usersInfo",
    "rolesInfo",
];
const MONGO_WRITE: [&str; 5] = ["insert", "update", "delete", "findAndModify", "bulkWrite"];
const MONGO_DESTRUCTIVE: [&str; 5] = ["drop", "dropDatabase", "renameCollection", "dropIndexes", "convertToCapped"];
const MONGO_DDL: [&str; 5] = ["create", "createIndexes", "collMod", "dropSearchIndex", "createSearchIndexes"];
const MONGO_TRANSACTION: [&str; 3] = ["startSession", "commitTransaction", "abortTransaction"];

/// Classifies a MongoDB database command written as JSON (the query tab's input).
pub fn classify_mongo(text: &str) -> Classification {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return Classification::single(StatementInfo {
                category: Category::Unknown,
                kind: "unparsed".to_owned(),
                tables: Vec::new(),
                excerpt: excerpt(text),
                reason: Some(format!("Not a JSON command ({error})")),
                refused: None,
            })
        }
    };
    let Some((name, target)) = value.as_object().and_then(|object| object.iter().next()) else {
        return Classification::single(StatementInfo {
            category: Category::Unknown,
            kind: "unparsed".to_owned(),
            tables: Vec::new(),
            excerpt: excerpt(text),
            reason: Some("A MongoDB command is a JSON object whose first key is the command name".to_owned()),
            refused: None,
        });
    };
    let tables = target.as_str().map(|collection| vec![collection.to_owned()]).unwrap_or_default();
    let (category, reason) = mongo_category(name, &value);
    Classification::single(StatementInfo {
        category,
        kind: name.clone(),
        tables,
        excerpt: excerpt(text),
        reason,
        refused: None,
    })
}

fn mongo_category(name: &str, command: &serde_json::Value) -> (Category, Option<String>) {
    if name == "aggregate" {
        let writes = command
            .get("pipeline")
            .and_then(|pipeline| pipeline.as_array())
            .is_some_and(|stages| {
                stages
                    .iter()
                    .any(|stage| stage.get("$out").is_some() || stage.get("$merge").is_some())
            });
        return if writes {
            (Category::Write, Some("$out / $merge write the result into a collection".to_owned()))
        } else {
            (Category::Read, None)
        };
    }
    if name == "delete" && mongo_matches_everything(command, "deletes") {
        return (Category::Destructive, Some("An empty filter deletes every document".to_owned()));
    }
    if name == "update" && mongo_updates_everything(command) {
        return (
            Category::Destructive,
            Some("An empty filter with multi updates every document".to_owned()),
        );
    }
    if name == "explain" {
        // Explain never applies writes, whatever it explains.
        return (Category::Read, None);
    }
    if MONGO_READ.contains(&name) {
        (Category::Read, None)
    } else if MONGO_WRITE.contains(&name) {
        (Category::Write, None)
    } else if MONGO_DESTRUCTIVE.contains(&name) {
        (Category::Destructive, Some(format!("{name} removes data")))
    } else if MONGO_DDL.contains(&name) {
        (Category::Ddl, None)
    } else if MONGO_TRANSACTION.contains(&name) {
        (Category::Transaction, None)
    } else if name.contains("User") || name.contains("Role") || name.starts_with("replSet") || ADMIN_MONGO.contains(&name) {
        (Category::Admin, None)
    } else {
        (
            Category::Unknown,
            Some(format!("{name} is not a known command, so it is treated like a write")),
        )
    }
}

const ADMIN_MONGO: [&str; 8] = [
    "killOp",
    "shutdown",
    "setParameter",
    "fsync",
    "fsyncUnlock",
    "logRotate",
    "compact",
    "setFeatureCompatibilityVersion",
];

fn mongo_matches_everything(command: &serde_json::Value, list: &str) -> bool {
    command.get(list).and_then(|items| items.as_array()).is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("q").and_then(|q| q.as_object()).is_some_and(|q| q.is_empty()))
    })
}

fn mongo_updates_everything(command: &serde_json::Value) -> bool {
    command.get("updates").and_then(|items| items.as_array()).is_some_and(|items| {
        items.iter().any(|item| {
            let empty = item.get("q").and_then(|q| q.as_object()).is_some_and(|q| q.is_empty());
            let multi = item.get("multi").and_then(|multi| multi.as_bool()).unwrap_or(false);
            empty && multi
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::SqlDialectId;

    fn categories(engine: SqlDialectId, sql: &str) -> Vec<Category> {
        classify_sql(engine, sql)
            .statements
            .iter()
            .map(|statement| statement.category)
            .collect()
    }

    fn one(engine: SqlDialectId, sql: &str) -> StatementInfo {
        let classification = classify_sql(engine, sql);
        assert_eq!(classification.statements.len(), 1, "{sql}: {classification:?}");
        classification.statements[0].clone()
    }

    #[test]
    fn reads_on_every_dialect() {
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql, SqlDialectId::MsSql] {
            assert_eq!(categories(engine, "SELECT * FROM orders WHERE id = 1"), [Category::Read]);
            assert_eq!(
                categories(engine, "SELECT o.id FROM orders o JOIN customers c ON c.id = o.customer_id"),
                [Category::Read]
            );
        }
        assert_eq!(categories(SqlDialectId::MariaDb, "SHOW TABLES"), [Category::Read]);
        assert_eq!(categories(SqlDialectId::MariaDb, "EXPLAIN SELECT 1"), [Category::Read]);
        assert_eq!(categories(SqlDialectId::MariaDb, "USE shop"), [Category::Read]);
        assert_eq!(
            categories(SqlDialectId::MsSql, "SELECT TOP 5 * FROM sales.Invoices"),
            [Category::Read]
        );
        assert_eq!(categories(SqlDialectId::MsSql, "SET NOCOUNT ON"), [Category::Read]);
    }

    #[test]
    fn writes_ddl_destructive_and_admin() {
        let maria = SqlDialectId::MariaDb;
        assert_eq!(one(maria, "INSERT INTO t (a) VALUES (1)").category, Category::Write);
        let replace = one(maria, "REPLACE INTO t (a) VALUES (1)");
        assert_eq!((replace.category, replace.kind.as_str()), (Category::Write, "REPLACE"));
        assert_eq!(one(maria, "UPDATE t SET a = 1 WHERE id = 2").category, Category::Write);
        assert_eq!(one(maria, "DELETE FROM t WHERE id = 2").category, Category::Write);
        let delete_all = one(maria, "DELETE FROM orders");
        assert_eq!(delete_all.category, Category::Destructive);
        assert_eq!(delete_all.tables, ["orders"]);
        assert!(delete_all.reason.unwrap().contains("without WHERE"));
        assert_eq!(one(maria, "UPDATE t SET a = 1").category, Category::Destructive);
        let drop = one(maria, "DROP TABLE shop.orders");
        assert_eq!((drop.category, drop.kind.as_str()), (Category::Destructive, "DROP TABLE"));
        assert_eq!(drop.tables, ["shop.orders"]);
        assert_eq!(one(maria, "TRUNCATE TABLE orders").category, Category::Destructive);
        assert_eq!(one(maria, "ALTER TABLE t DROP COLUMN a").category, Category::Destructive);
        assert_eq!(one(maria, "ALTER TABLE t ADD COLUMN b INT").category, Category::Ddl);
        assert_eq!(one(maria, "CREATE TABLE t (id INT PRIMARY KEY)").category, Category::Ddl);
        assert_eq!(one(maria, "CREATE INDEX i ON t (a)").category, Category::Ddl);
        assert_eq!(one(maria, "GRANT SELECT ON shop.* TO 'app'@'%'").category, Category::Admin);
        assert_eq!(one(maria, "KILL 42").category, Category::Admin);
        assert_eq!(one(maria, "SET GLOBAL max_connections = 10").category, Category::Admin);
        assert_eq!(one(maria, "CREATE USER 'x'@'%' IDENTIFIED BY 'y'").category, Category::Admin);
        assert_eq!(one(SqlDialectId::MsSql, "DROP TABLE dbo.Orders").category, Category::Destructive);
        assert_eq!(
            one(
                SqlDialectId::MsSql,
                "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET t.a = s.a;"
            )
            .category,
            Category::Write
        );
        assert_eq!(one(SqlDialectId::MsSql, "EXEC sp_who").category, Category::Write);
    }

    #[test]
    fn session_settings_and_transactions() {
        let maria = SqlDialectId::MariaDb;
        assert_eq!(one(maria, "SET @x = 1").category, Category::Read);
        assert_eq!(one(maria, "SET NAMES utf8mb4").category, Category::Read);
        assert_eq!(one(maria, "SET autocommit = 0").category, Category::Transaction);
        assert_eq!(one(maria, "SET SESSION max_statement_time = 0").category, Category::Admin);
        assert_eq!(one(maria, "SET SESSION TRANSACTION READ WRITE").category, Category::Admin);
        assert_eq!(
            one(maria, "SET TRANSACTION ISOLATION LEVEL READ COMMITTED").category,
            Category::Transaction
        );
        for sql in ["START TRANSACTION", "BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT a"] {
            assert_eq!(one(maria, sql).category, Category::Transaction, "{sql}");
        }
        assert_eq!(categories(SqlDialectId::MsSql, "BEGIN TRANSACTION"), [Category::Transaction]);
        assert_eq!(categories(SqlDialectId::MsSql, "COMMIT TRANSACTION"), [Category::Transaction]);
    }

    #[test]
    fn comments_multi_statements_and_executable_comments() {
        let maria = SqlDialectId::MariaDb;
        assert_eq!(categories(maria, "-- just a note\nSELECT 1 /* inline */"), [Category::Read]);
        assert_eq!(categories(maria, "# hash comment\nSELECT 1"), [Category::Read]);
        assert_eq!(
            categories(maria, "SELECT 1; DELETE FROM t WHERE id = 1; DROP TABLE t"),
            [Category::Read, Category::Write, Category::Destructive]
        );
        // The server runs the inside of /*! … */ (and MariaDB's /*M! … */); so does the classifier.
        assert!(categories(maria, "SELECT 1 /*! ; DROP TABLE t */").contains(&Category::Destructive));
        assert!(categories(maria, "SELECT 1 /*M!100100 ; DROP TABLE t */").contains(&Category::Destructive));
        assert!(categories(maria, "/*!40101 DELETE FROM t */").contains(&Category::Destructive));
        // A plain comment that mentions DROP is still a comment.
        assert_eq!(categories(maria, "SELECT 1 /* DROP TABLE t */"), [Category::Read]);
    }

    #[test]
    fn ctes_and_select_into() {
        let mssql = SqlDialectId::MsSql;
        let cte_delete = one(
            mssql,
            "WITH old AS (SELECT id FROM t WHERE y < 2000) DELETE FROM t WHERE id IN (SELECT id FROM old)",
        );
        assert!(cte_delete.category >= Category::Write, "{cte_delete:?}");
        assert_eq!(
            one(SqlDialectId::MariaDb, "WITH x AS (SELECT 1 AS a) SELECT * FROM x").category,
            Category::Read
        );
        assert_eq!(one(mssql, "SELECT * INTO archive FROM orders").category, Category::Ddl);
        assert_eq!(one(mssql, "SELECT * INTO #tmp FROM orders").category, Category::Write);
        assert_eq!(
            one(SqlDialectId::MariaDb, "SELECT COUNT(*) INTO @n FROM orders").category,
            Category::Read
        );
        let outfile = one(SqlDialectId::MariaDb, "SELECT * FROM orders INTO OUTFILE '/tmp/x.csv'");
        assert!(outfile.category >= Category::Write, "{outfile:?}");
        // EXPLAIN only plans; EXPLAIN ANALYZE runs the statement.
        assert_eq!(one(SqlDialectId::MariaDb, "EXPLAIN DELETE FROM t").category, Category::Read);
        assert_eq!(
            one(SqlDialectId::MariaDb, "EXPLAIN ANALYZE DELETE FROM t").category,
            Category::Destructive
        );
    }

    #[test]
    fn unparseable_sql_is_unknown() {
        let unknown = one(SqlDialectId::MariaDb, "FROBNICATE the database");
        assert_eq!(unknown.category, Category::Unknown);
        assert!(unknown.reason.unwrap().contains("treated like a write"));
        assert!(classify_sql(SqlDialectId::MariaDb, "  ").statements.is_empty());
    }

    #[test]
    fn mongo_commands_by_name() {
        let category = |text: &str| classify_mongo(text).statements[0].category;
        assert_eq!(category(r#"{ "find": "products", "filter": { "stock": 0 } }"#), Category::Read);
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$match": {} } ], "cursor": {} }"#),
            Category::Read
        );
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$out": "copy" } ], "cursor": {} }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$merge": { "into": "x" } } ], "cursor": {} }"#),
            Category::Write
        );
        assert_eq!(category(r#"{ "count": "c" }"#), Category::Read);
        assert_eq!(category(r#"{ "listCollections": 1 }"#), Category::Read);
        assert_eq!(category(r#"{ "insert": "c", "documents": [ { "a": 1 } ] }"#), Category::Write);
        assert_eq!(
            category(r#"{ "update": "c", "updates": [ { "q": { "a": 1 }, "u": { "$set": { "b": 2 } } } ] }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "update": "c", "updates": [ { "q": {}, "u": { "$set": { "b": 2 } }, "multi": true } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": { "a": 1 }, "limit": 1 } ] }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": {}, "limit": 0 } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "findAndModify": "c", "query": {}, "remove": true }"#),
            Category::Write
        );
        assert_eq!(category(r#"{ "drop": "c" }"#), Category::Destructive);
        assert_eq!(category(r#"{ "dropDatabase": 1 }"#), Category::Destructive);
        assert_eq!(category(r#"{ "renameCollection": "a.b", "to": "a.c" }"#), Category::Destructive);
        assert_eq!(category(r#"{ "createIndexes": "c", "indexes": [] }"#), Category::Ddl);
        assert_eq!(category(r#"{ "collMod": "c" }"#), Category::Ddl);
        assert_eq!(category(r#"{ "createUser": "x", "pwd": "y", "roles": [] }"#), Category::Admin);
        assert_eq!(category(r#"{ "frobnicate": 1 }"#), Category::Unknown);
        assert_eq!(category("not json"), Category::Unknown);
        assert_eq!(classify_mongo(r#"{ "drop": "events" }"#).statements[0].tables, ["events"]);
    }

    #[test]
    fn grid_changes_are_writes_to_one_table() {
        use crate::drivers::types::CellValue;
        let cell = |value: &str| CellValue {
            column: "id".into(),
            value: Some(value.into()),
        };
        let changes = vec![
            RowChange::Insert { values: vec![cell("1")] },
            RowChange::Delete { key: vec![cell("2")] },
            RowChange::Delete { key: vec![cell("3")] },
        ];
        let classification = classify_changes("orders", &changes);
        assert_eq!(classification.categories(), [Category::Write]);
        assert_eq!(classification.statements.len(), 2);
        assert_eq!(classification.tables(), ["orders"]);
    }
}
