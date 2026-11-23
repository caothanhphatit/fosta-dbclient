//! SQL Server (engine id `mssql`) on tiberius: typed values, structured filters and keyset paging through
//! `engines::common::sqlbuild` with a T-SQL dialect, edits guarded by the table's `rowversion` (or the original values),
//! and query sessions with `GO` batches, per-statement row counts, transactions, Stop and the statement timeout.

pub mod adapter;
pub mod batch;
mod catalog;
mod connect;
#[cfg(test)]
mod contract_tests;
pub mod descriptor;
mod schema_snapshot;
pub mod schema_sync;
#[cfg(test)]
mod schema_sync_tests;
mod session;
mod tabular;
mod url;
pub mod values;

pub use adapter::{MssqlAdapter, MSSQL};
