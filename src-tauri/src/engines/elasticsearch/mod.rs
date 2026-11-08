//! Elasticsearch / OpenSearch engine core (search and document capabilities in ADR 0001): a "Kibana-lite" over the
//! REST API with plain `reqwest`.
//!
//! - [`config`] and [`client`]: node URLs or Cloud ID, basic / API key / bearer auth, TLS (CA, insecure, SHA-256
//!   fingerprint pinning), timeouts, failover, error bodies as sentences.
//! - [`connection`]: [`connection::EsEngine::connect`], distribution and version detection, feature flags, health.
//! - [`catalog`]: indices, data streams, aliases, templates, ingest pipelines, ILM / ISM policies, snapshots, and the
//!   details of one index. [`mapping`]: flattened mappings and field capabilities that type document values.
//! - [`search`]: Query DSL search with from/size, search_after and point-in-time paging, highlight, aggregation trees,
//!   profile, count and validation. [`query_lang`]: ES|QL and SQL as typed tables with cursors.
//! - [`docs`]: typed document operations and by-query operations. [`admin`]: analyze, ingest simulate, `_cat`, cluster
//!   stats, tasks, index operations and reindex.
//! - [`console`]: the Dev Tools console (Kibana request format). [`classify`]: request classes for the gateway.
//!
//! OpenSearch differences handled here: PIT (`_search/point_in_time`, `pit_id`), SQL (`_plugins/_sql`, JDBC format),
//! ISM policies (`_plugins/_ism/policies`, states instead of phases) instead of ILM, no ES|QL (PPL is deferred).

pub mod adapter;
pub mod admin;
pub mod catalog;
pub mod classify;
pub mod client;
pub mod config;
pub mod connection;
pub mod console;
pub mod descriptor;
pub mod docs;
pub mod mapping;
pub mod query_lang;
pub mod search;

#[cfg(test)]
mod live_tests;

pub use adapter::{ElasticAdapter, ELASTICSEARCH, OPENSEARCH};
