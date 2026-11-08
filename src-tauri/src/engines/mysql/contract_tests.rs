//! Contract suites for MariaDB against `dev/compose/mariadb.yml` (data from `dev/seed/mariadb/contract.sql`).
//! Override the server with FOSTA_CONTRACT_MARIADB_HOST / _PORT / _USER / _PASSWORD.

use crate::contract::{testing::suites::*, Category, ConnectSecrets, ConnectionSpec, Filter, NodeKind, ObjectRef, RawFilterLang};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_MARIADB_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/mariadb/contract.sql")
        .split("\nGO\n")
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: format!("`id` = {key}"),
    }
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "mariadb",
        spec: ConnectionSpec {
            engine: "mariadb".into(),
            host: env("HOST", "127.0.0.1"),
            port: env("PORT", "3306").parse().unwrap_or(3306),
            user: env("USER", "root"),
            // The seed creates the database, so connect without one.
            database: None,
            ..ConnectionSpec::default()
        },
        secrets: ConnectSecrets::password(&env("PASSWORD", "studio-root")),
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
        gaps: &[Gap::StructuredFilter, Gap::Concurrency, Gap::Keyset, Gap::TypedValues],
    }
}

crate::contract_tests!(contract_mariadb, fixture());
