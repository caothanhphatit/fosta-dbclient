//! Test support for the contract: an in-memory engine for core tests, and the per-capability suites every engine must
//! pass against a live server (see [`suites`]).

pub mod fake;
pub mod suites;
pub mod vector;

pub use fake::FakeEngine;
