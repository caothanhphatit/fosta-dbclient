//! MongoDB (engine id `mongodb`) on the official driver: BSON as typed values (ObjectIds, nested documents and arrays,
//! Decimal128, dates) with type-preserving write-back, structured and raw filters, keyset paging on `_id`, edits
//! checked against what the user read (in one transaction on replica sets, compensated on standalone servers), and
//! query tabs that run database commands written as Extended JSON, reading cursors with `getMore`, and the engine
//! operations the MongoDB views invoke (`mongo.find`, `mongo.count`, `mongo.distinct`, `mongo.resolveRef`).

pub mod adapter;
mod catalog;
pub mod classify;
mod connect;
#[cfg(test)]
mod contract_tests;
pub mod descriptor;
mod ops;
mod session;
mod tabular;
mod url;
pub mod values;

pub use adapter::{MongoAdapter, MONGODB};
