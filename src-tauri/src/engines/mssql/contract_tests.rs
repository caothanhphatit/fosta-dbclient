//! Contract suites for SQL Server against `dev/compose/mssql.yml` (data from `dev/seed/mssql/contract.sql`).
//! Override the server with FOSTA_CONTRACT_MSSQL_HOST / _PORT / _USER / _PASSWORD.

use std::collections::HashMap;

use crate::contract::{testing::suites::*, Category, ConnectSecrets, ConnectionSpec, Filter, NodeKind, ObjectRef, RawFilterLang};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_MSSQL_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/mssql/contract.sql")
        .replace("\r\n", "\n")
        .split("\nGO\n")
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: format!("[id] = {key}"),
    }
}

pub fn fixture() -> Fixture {
    let options: HashMap<String, String> = [("encrypt", "true"), ("trustServerCertificate", "true")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    Fixture {
        engine: "mssql",
        spec: ConnectionSpec {
            engine: "mssql".into(),
            host: env("HOST", "127.0.0.1"),
            port: env("PORT", "1433").parse().unwrap_or(1433),
            user: env("USER", "sa"),
            // The seed creates the database, so connect to master.
            database: Some("master".into()),
            options,
            ..ConnectionSpec::default()
        },
        secrets: ConnectSecrets::password(&env("PASSWORD", "Studio#Pass2026")),
        // The legacy driver leaves out the default schema dbo; the native adapter sets the schema field.
        table: ObjectRef::new(Some("fosta_contract"), None, "contract_items", NodeKind::TABLE),
        child: ObjectRef::new(Some("fosta_contract"), None, "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("WAITFOR DELAY '00:00:30'"),
        syntax_error: "SELEC * FORM contract_items",
        constraint_violation: "INSERT INTO fosta_contract.dbo.contract_items (id, name, price, active) VALUES (1, 'dup', 1, 1)",
        multi_statement: "SELECT 1 AS one; SELECT TOP 3 id, name FROM fosta_contract.dbo.contract_items ORDER BY id",
        insert_probe: "INSERT INTO fosta_contract.dbo.contract_items (id, name, price, active) VALUES (5000, 'probe', 1, 1)",
        count_probe: "SELECT COUNT(*) AS n FROM fosta_contract.dbo.contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT TOP 5 * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("TRUNCATE TABLE t", Category::Destructive),
            ("CREATE TABLE x (id INT)", Category::Ddl),
            ("BEGIN TRANSACTION", Category::Transaction),
        ],
        language: "sql",
        gaps: &[Gap::StructuredFilter, Gap::Concurrency, Gap::Keyset, Gap::TypedValues],
    }
}

crate::contract_tests!(contract_mssql, fixture());
