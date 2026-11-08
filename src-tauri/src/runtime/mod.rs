//! The application runtime around the engines: crash containment today; the engine registry and the connection
//! manager join it in the engine-contract refactor (ADR 0001/0002).

pub mod contain;

pub use contain::{contained, install_panic_hook, ENGINE_CRASHED};
