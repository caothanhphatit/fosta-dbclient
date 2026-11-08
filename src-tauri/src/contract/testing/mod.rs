//! Test support for the contract: an in-memory engine (and, with the contract suites, the per-capability tests every
//! engine must pass).

pub mod fake;

pub use fake::FakeEngine;
