//! Engine modules for PostgreSQL, SQLite, ClickHouse and Oracle (ADR 0001: hexagonal core with engine modules).
//!
//! Each engine is self-contained: it connects from a typed options struct, describes its catalog as a tree, browses tables with
//! structured filters, runs statements in sessions (streaming, cancel, timeout, transaction state), maps every engine type to a
//! neutral [`common::Value`], applies row changes in one transaction and classifies statements for the statement gateway. The
//! shapes they share (values, catalog nodes, browse requests, result events, classification) live in [`common`], which is the
//! seed of the core's engine ports.
//!
//! They are not wired into the app yet: the engine-module refactor will put them behind the core's ports and register them with
//! the UI. Until then only the tests use them.
#![allow(dead_code)]

pub mod clickhouse;
pub mod common;
pub mod postgres;
pub mod sqlite;
