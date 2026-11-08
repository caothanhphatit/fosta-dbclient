//! MongoDB (engine id `mongodb`). Until the native adapter lands, connections go through the legacy driver in
//! `drivers/mongo.rs` (see `runtime::legacy`).

#[cfg(test)]
mod contract_tests;
pub mod descriptor;
