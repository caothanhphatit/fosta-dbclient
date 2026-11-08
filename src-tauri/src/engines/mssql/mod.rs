//! SQL Server (engine id `mssql`). Until the native adapter lands, connections go through the legacy driver in
//! `drivers/mssql.rs` (see `runtime::legacy`).

#[cfg(test)]
mod contract_tests;
pub mod descriptor;
