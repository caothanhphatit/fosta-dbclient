//! Engine modules (ADR 0001): one self-contained adapter per engine, with its own connection options, typed values and
//! statement classification, registered in `runtime::registry`. [`mysql`] is a native adapter; the others are wired
//! into the contract one by one (planned engines refuse to connect until then).
//!
//! The SQL engines (PostgreSQL, SQLite, ClickHouse, Oracle) share [`common`]: the neutral typed value, catalog nodes and
//! descriptions, browse requests with structured filters and keyset paging, row changes, result streaming with row limits,
//! transaction state, a dialect-aware statement splitter, a SQL builder that binds every value, and statement classification.
//! Each connects from a typed options struct, describes its catalog as a tree, browses tables, runs statements in sessions
//! (streaming, cancel, timeout, transaction state) and applies row changes in one transaction where the engine has them.
#![allow(dead_code)]

#[cfg(feature = "engine-clickhouse")]
pub mod clickhouse;
pub mod common;
#[cfg(feature = "engine-elastic")]
pub mod elasticsearch;
pub mod mongo;
pub mod mssql;
pub mod mysql;
#[cfg(feature = "engine-oracle")]
pub mod oracle;
#[cfg(feature = "engine-postgres")]
pub mod postgres;
#[cfg(feature = "engine-redis")]
pub mod redis;
#[cfg(feature = "engine-sqlite")]
pub mod sqlite;
