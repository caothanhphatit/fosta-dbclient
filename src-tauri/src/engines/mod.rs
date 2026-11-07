//! Engine modules (ADR 0001): one self-contained adapter per engine, with its own connection options, typed values and
//! statement classification. They are wired into the core ports in the engine-module refactor; until then nothing outside
//! the live tests calls them.
//!
//! The SQL engines (PostgreSQL, SQLite, ClickHouse, Oracle) share [`common`]: the neutral typed value, catalog nodes and
//! descriptions, browse requests with structured filters and keyset paging, row changes, result streaming with row limits,
//! transaction state, a dialect-aware statement splitter, a SQL builder that binds every value, and statement classification.
//! Each connects from a typed options struct, describes its catalog as a tree, browses tables, runs statements in sessions
//! (streaming, cancel, timeout, transaction state) and applies row changes in one transaction where the engine has them.
#![allow(dead_code)]

pub mod clickhouse;
pub mod common;
pub mod elasticsearch;
pub mod oracle;
pub mod postgres;
pub mod redis;
pub mod sqlite;
