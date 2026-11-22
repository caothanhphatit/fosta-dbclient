//! Schema compare: the engine-neutral diff of two [`SchemaSnapshot`]s and the planner that orders a sync script.
//! Pure: it reads no database and runs nothing. Engines take part only through the contract
//! ([`crate::contract::SchemaDialect`]), never by name.
//!
//! - [`options`]: the compare options resolved against the two snapshots (case folding, exclusions).
//! - [`diff`]: matching tables and views, then their columns, keys, checks, indexes and foreign keys.
//! - [`plan`]: the selected differences as ordered migration steps, with the dependencies they pull in.
//! - [`script`]: the steps rendered by the target dialect into a reviewed, never-run SQL script.

pub mod diff;
pub mod options;
pub mod plan;
pub mod script;
#[cfg(test)]
mod tests;

pub use diff::diff;
pub use options::{glob_match, ResolvedOptions};
pub use plan::{plan, Plan, PlannedStep};
pub use script::sync_script;

use crate::{
    contract::SchemaSnapshot,
    error::{AppError, AppResult},
};

/// MySQL and MariaDB share one catalog module, so they may be compared (the script uses the target's dialect).
const COMPATIBLE_PAIRS: [(&str, &str); 1] = [("mysql", "mariadb")];

/// Whether two engines may be compared: the same engine, or a compatible pair.
pub fn engines_compatible(source: &str, target: &str) -> bool {
    source == target
        || COMPATIBLE_PAIRS
            .iter()
            .any(|(a, b)| (source == *a && target == *b) || (source == *b && target == *a))
}

/// Refuses snapshots of engines that cannot be compared.
pub fn check_compatible(source: &SchemaSnapshot, target: &SchemaSnapshot) -> AppResult<()> {
    if engines_compatible(&source.engine, &target.engine) {
        Ok(())
    } else {
        Err(AppError::new(format!(
            "Compare needs two connections of the same engine ({} and {} differ)",
            source.engine, target.engine
        )))
    }
}
