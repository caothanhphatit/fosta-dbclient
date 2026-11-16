//! Contract suites for MySQL against `dev/compose/mysql.yml` (data from `dev/seed/mysql/contract.sql`; the `shop` demo
//! database from `dev/seed/mysql.sql`). The shared suites and the MariaDB checks of `contract_tests.rs` run here too; the
//! vector operations do not (they are MariaDB's), and the shop has no vector column. Override the server with
//! FOSTA_CONTRACT_MYSQL_HOST / _PORT / _USER / _PASSWORD.

use super::contract_tests::{
    check_gateway_rejects_injected_filters, check_gateway_rules_and_confirmation, check_read_only_is_reasserted_every_run,
    check_schema_graph, check_shop_catalog_and_edits, check_transactions_and_backstops, check_typed_values, key_filter,
};
use super::contract_tests::{shared_checks, Target};
use crate::contract::{testing::suites::Fixture, Category, ConnectSecrets, ConnectionSpec, NodeKind, ObjectRef};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_MYSQL_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/mysql/contract.sql")
        .split("\nGO\n")
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn spec(database: Option<&str>) -> ConnectionSpec {
    ConnectionSpec {
        engine: "mysql".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "3307").parse().unwrap_or(3307),
        user: env("USER", "root"),
        database: database.map(str::to_owned),
        ..ConnectionSpec::default()
    }
}

fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "studio-root"))
}

fn fixture() -> Fixture {
    Fixture {
        engine: "mysql",
        // The seed creates the database, so connect without one.
        spec: spec(None),
        secrets: secrets(),
        table: ObjectRef::new(Some("fosta_contract"), None, "contract_items", NodeKind::TABLE),
        child: ObjectRef::new(Some("fosta_contract"), None, "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("SELECT SLEEP(30)"),
        syntax_error: "SELEC * FORM contract_items",
        constraint_violation: "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (1, 'dup', 1, 1)",
        multi_statement: "SELECT 1; SELECT id, name FROM fosta_contract.contract_items ORDER BY id LIMIT 3",
        insert_probe: "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (5000, 'probe', 1, 1)",
        count_probe: "SELECT COUNT(*) FROM fosta_contract.contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("DELETE FROM t", Category::Destructive),
            ("CREATE TABLE x (id INT)", Category::Ddl),
            ("GRANT ALL ON *.* TO x", Category::Admin),
        ],
        language: "sql",
        gaps: &[],
    }
}

crate::contract_tests!(contract_mysql, fixture());

const MYSQL: Target = Target {
    fixture,
    spec,
    secrets,
    // max_execution_time stops SELECTs only, so the slow statement is a SELECT over two 100000-row recursive CTEs.
    slow_select: "WITH RECURSIVE s (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 100000) \
                  SELECT SUM(a.n * b.n) FROM s a, s b",
    label_prefix: "MySQL ",
    vector: false,
    read_only_var: "transaction_read_only",
    int_literal_type: "bigint",
};

shared_checks!(MYSQL, "needs dev/compose/mysql.yml";
    contract_mysql_shop_catalog_and_edits => check_shop_catalog_and_edits,
    contract_mysql_typed_values => check_typed_values,
    contract_mysql_transactions_and_backstops => check_transactions_and_backstops,
    contract_mysql_read_only_is_reasserted_every_run => check_read_only_is_reasserted_every_run,
    contract_mysql_schema_graph => check_schema_graph,
    contract_mysql_gateway_rejects_injected_filters => check_gateway_rejects_injected_filters,
    contract_mysql_gateway_rules_and_confirmation => check_gateway_rules_and_confirmation,
);
