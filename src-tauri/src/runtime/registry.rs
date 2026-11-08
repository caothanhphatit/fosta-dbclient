//! The engine registry: every engine id this build knows, in connection-dialog order. Each entry is behind a Cargo
//! feature (`engine-<name>`, all on by default and in `web`). Engines that are registered but not wired yet have
//! descriptors with status `planned` and refuse to connect with `ENGINE_NOT_WIRED`; follow-up work only edits each
//! engine's own `adapter.rs` and `descriptor.rs`, never this file.

use std::sync::OnceLock;

use serde_json::json;

use crate::{
    contract::{errors::UNKNOWN_ENGINE, Engine, EngineDescriptor},
    error::{AppError, AppResult},
};

pub struct Registry {
    engines: Vec<&'static dyn Engine>,
}

pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry { engines: entries() })
}

#[allow(clippy::vec_init_then_push, unused_mut)]
fn entries() -> Vec<&'static dyn Engine> {
    let mut engines: Vec<&'static dyn Engine> = Vec::new();
    #[cfg(feature = "engine-mysql")]
    {
        engines.push(&crate::engines::mysql::MARIADB);
        engines.push(&crate::engines::mysql::MYSQL);
    }
    #[cfg(feature = "engine-mssql")]
    engines.push(&crate::engines::mssql::MSSQL);
    #[cfg(feature = "engine-mongo")]
    engines.push(&crate::engines::mongo::MONGODB);
    #[cfg(feature = "engine-postgres")]
    engines.push(&crate::engines::postgres::POSTGRES);
    #[cfg(feature = "engine-sqlite")]
    engines.push(&crate::engines::sqlite::SQLITE);
    #[cfg(feature = "engine-clickhouse")]
    engines.push(&crate::engines::clickhouse::CLICKHOUSE);
    #[cfg(feature = "engine-oracle")]
    engines.push(&crate::engines::oracle::ORACLE);
    #[cfg(feature = "engine-redis")]
    engines.push(&crate::engines::redis::REDIS);
    #[cfg(feature = "engine-elastic")]
    {
        engines.push(&crate::engines::elasticsearch::ELASTICSEARCH);
        engines.push(&crate::engines::elasticsearch::OPENSEARCH);
    }
    engines
}

impl Registry {
    /// The engine for a profile's id; unknown ids are refused (there is no fallback to MySQL).
    pub fn get(&self, id: &str) -> AppResult<&'static dyn Engine> {
        self.engines.iter().copied().find(|engine| engine.id() == id).ok_or_else(|| {
            let known: Vec<&str> = self.engines.iter().map(|engine| engine.id()).collect();
            AppError::structured(
                format!("This build of Fosta Studio does not support the engine '{id}'. Edit the connection to pick a supported one."),
                json!({ "code": UNKNOWN_ENGINE, "engine": id, "known": known }),
            )
        })
    }

    pub fn descriptors(&self) -> Vec<EngineDescriptor> {
        self.engines.iter().map(|engine| engine.descriptor().clone()).collect()
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.engines.iter().map(|engine| engine.descriptor().id.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "engines-all")]
    fn every_engine_is_registered_once_with_a_matching_descriptor() {
        let ids = registry().ids();
        for id in [
            "mariadb",
            "mysql",
            "mssql",
            "mongodb",
            "postgres",
            "sqlite",
            "clickhouse",
            "oracle",
            "redis",
            "elasticsearch",
            "opensearch",
        ] {
            assert_eq!(ids.iter().filter(|known| **known == id).count(), 1, "{id}");
            assert_eq!(registry().get(id).unwrap().id(), id);
        }
        for descriptor in registry().descriptors() {
            assert!(!descriptor.languages.is_empty(), "{} has no language", descriptor.id);
            assert!(descriptor.default_language().is_some());
        }
    }

    #[test]
    #[cfg(feature = "engines-all")]
    fn unknown_engines_are_refused_with_the_known_list() {
        let error = registry().get("postgress").err().expect("refused");
        assert_eq!(error.code(), Some(UNKNOWN_ENGINE));
        assert!(error.field("known").unwrap().as_array().unwrap().len() >= 11);
    }
}
