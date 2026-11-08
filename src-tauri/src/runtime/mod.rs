//! The application runtime around the engines: the registry of engines, the connection manager (one map of
//! connections and sessions) and crash containment for every engine call.

pub mod contain;
pub mod manager;
pub mod registry;

pub use contain::{contained, install_panic_hook, ENGINE_CRASHED};
pub use manager::{ConnectionManager, Handle};
pub use registry::registry;
