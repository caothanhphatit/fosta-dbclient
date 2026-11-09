//! MariaDB and MySQL (engine ids `mariadb` and `mysql`) on mysql_async: typed values from both wire protocols,
//! structured filters and keyset paging through `engines::common::sqlbuild`, edits guarded by optimistic concurrency
//! checks, query sessions with Stop, transactions and the rules' backstops, and the MariaDB vector tooling as engine
//! operations (`vector.*`).

pub mod adapter;
mod catalog;
mod connect;
#[cfg(test)]
mod contract_tests;
pub mod descriptor;
#[cfg(test)]
mod perf_tests;
mod session;
mod tabular;
mod url;
pub mod values;
pub mod vector;

pub use adapter::{MySqlAdapter, MARIADB, MYSQL};
