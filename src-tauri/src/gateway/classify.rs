//! Statement classification for the gateway. Engines classify their own statements (`Engine::classify`); the shared SQL
//! classifier and parser (`engines::common::sqlclassify`, used by every SQL engine and by the grid filter check) are
//! re-exported here.

pub use crate::contract::classify::{Category, Classification, StatementInfo};
pub use crate::engines::common::sqlclassify::{classify_sql, dialect_for, excerpt, normalise_executable_comments, parse};

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
}
