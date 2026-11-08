//! Language support for the query editor: a lossless, dialect-aware SQL lexer, a statement splitter with
//! "statement at cursor", a SQL formatter that only ever changes whitespace and keyword case, and the
//! [`CompletionSchema`] the editor's completion source is driven by.
//!
//! The module depends on nothing else in the crate, so commands (`completion_schema`, `format_sql`) and engines can
//! call it without cycles. See `docs/engines/language.md` for the API.
//!
//! Dialects use the same camelCase names as the engine contract's `SqlDialectId` (`mariaDb`, `mySql`, `msSql`,
//! `postgres`, `sqlite`, `clickHouse`, `oracle`, `generic`).

pub mod format;
pub mod keywords;
pub mod lexer;
pub mod schema;
pub mod split;

#[cfg(test)]
mod format_tests;

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

pub use format::{format_sql, try_format, FormatError, FormatOptions, KeywordCase};
pub use lexer::{tokenize, Token, TokenKind};
pub use schema::{CompletionColumn, CompletionSchema, CompletionTable, ForeignRef};
pub use split::{split, statement_at, Statement};

/// A SQL dialect as the editor knows it. Serialized like the contract's `SqlDialectId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dialect {
    #[serde(alias = "mariadb")]
    MariaDb,
    #[serde(alias = "mysql")]
    MySql,
    #[serde(alias = "mssql", alias = "tsql")]
    MsSql,
    #[serde(alias = "postgresql", alias = "pg")]
    Postgres,
    Sqlite,
    #[serde(alias = "clickhouse")]
    ClickHouse,
    Oracle,
    #[serde(alias = "ansi")]
    Generic,
}

impl Dialect {
    pub const ALL: [Dialect; 8] = [
        Dialect::MariaDb,
        Dialect::MySql,
        Dialect::MsSql,
        Dialect::Postgres,
        Dialect::Sqlite,
        Dialect::ClickHouse,
        Dialect::Oracle,
        Dialect::Generic,
    ];

    /// MySQL and MariaDB share their lexical rules.
    pub fn is_mysql(self) -> bool {
        matches!(self, Dialect::MySql | Dialect::MariaDb)
    }

    pub fn id(self) -> &'static str {
        match self {
            Dialect::MariaDb => "mariaDb",
            Dialect::MySql => "mySql",
            Dialect::MsSql => "msSql",
            Dialect::Postgres => "postgres",
            Dialect::Sqlite => "sqlite",
            Dialect::ClickHouse => "clickHouse",
            Dialect::Oracle => "oracle",
            Dialect::Generic => "generic",
        }
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Dialect {
    type Err = String;

    /// Accepts the camelCase ids and the usual aliases (`mysql`, `mssql`, `tsql`, `postgresql`, `pg`, ...).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.trim().to_ascii_lowercase();
        Ok(match lower.as_str() {
            "mariadb" => Dialect::MariaDb,
            "mysql" => Dialect::MySql,
            "mssql" | "tsql" | "sqlserver" => Dialect::MsSql,
            "postgres" | "postgresql" | "pg" => Dialect::Postgres,
            "sqlite" => Dialect::Sqlite,
            "clickhouse" => Dialect::ClickHouse,
            "oracle" | "plsql" => Dialect::Oracle,
            "generic" | "ansi" | "sql" => Dialect::Generic,
            _ => return Err(format!("Unknown SQL dialect `{s}`.")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_ids_round_trip_through_serde_and_from_str() {
        for d in Dialect::ALL {
            let json = serde_json::to_string(&d).unwrap();
            assert_eq!(json, format!("\"{}\"", d.id()));
            assert_eq!(serde_json::from_str::<Dialect>(&json).unwrap(), d);
            assert_eq!(d.id().parse::<Dialect>().unwrap(), d);
        }
        assert_eq!(serde_json::from_str::<Dialect>("\"mssql\"").unwrap(), Dialect::MsSql);
        assert_eq!("PostgreSQL".parse::<Dialect>().unwrap(), Dialect::Postgres);
        assert!("cobol".parse::<Dialect>().is_err());
    }
}
