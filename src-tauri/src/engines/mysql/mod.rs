//! MariaDB and MySQL (engine ids `mariadb` and `mysql`). Until the native adapter lands, connections go through the
//! legacy driver in `drivers/mysql.rs` (see `runtime::legacy`).

#[cfg(test)]
mod contract_tests;
pub mod descriptor;
