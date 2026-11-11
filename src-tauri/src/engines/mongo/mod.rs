//! MongoDB (engine id `mongodb`) on the official driver: BSON as typed values (ObjectIds, nested documents and arrays,
//! Decimal128, dates) with type-preserving write-back, structured and raw filters, keyset paging on `_id`, edits
//! checked against what the user read (in one transaction on replica sets, compensated on standalone servers), and
//! query tabs that run database commands written as Extended JSON, reading cursors with `getMore`, and the engine
//! operations the MongoDB views invoke (`mongo.find`, `mongo.count`, `mongo.distinct`, `mongo.resolveRef`, and the bulk
//! `mongo.updatePreview`, `mongo.updateMany`, `mongo.deleteMany`, and the aggregations `mongo.aggregatePreview`,
//! `mongo.aggregate`, `mongo.schema`, the shape of a sample, and the index operations `mongo.indexes`, `mongo.createIndex`,
//! `mongo.hideIndex` / `mongo.unhideIndex` and `mongo.dropIndex`, and the validation rules `mongo.validation`,
//! `mongo.setValidation` and `mongo.validationCheck`).

pub mod adapter;
mod aggregate;
pub(crate) mod bulk;
mod catalog;
pub mod classify;
mod connect;
#[cfg(test)]
mod contract_tests;
pub mod descriptor;
mod indexes;
mod ops;
mod schema;
mod session;
mod tabular;
mod url;
mod validation;
pub mod values;

pub use adapter::{MongoAdapter, MONGODB};
