//! The engine ports. Each engine is one `&'static dyn Engine` (a factory registered in `runtime::registry`); connecting
//! gives an `Arc<dyn Connection>`, which offers the optional capability traits it supports.
//!
//! Every DTO is `Serialize + Deserialize` and the traits are object-safe, so a later out-of-process engine (JSON-RPC)
//! can implement them too.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    capability::{Capability, CapabilitySet},
    classify::{classify_changes, classify_create_database, classify_transaction, Classification, ClassifyInput},
    describe::{ColumnInfo, ObjectDescription},
    descriptor::{EngineDescriptor, LanguageSpec},
    errors::unsupported_capability,
    limits::CallContext,
    object::{NodeAction, ObjectRef, TreeChildren, TreeRequest},
    profile::ProfileExtras,
    schema::SchemaGraph,
    session::{OpenedSession, SessionOptions},
    spec::{ConnectRequest, ConnectionSpec, ServerInfo},
    tabular::{ApplyResult, BrowseRequest, Filter, Page, RowChangeSet},
};
use crate::error::{AppError, AppResult};

/// A connection string split into a spec and its password (which is never written to the spec's options).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedConnection {
    pub spec: ConnectionSpec,
    pub password: Option<String>,
}

#[async_trait]
pub trait Engine: Send + Sync + 'static {
    /// Built once (in a `OnceLock`) and served to the UI by `engine_descriptors`.
    fn descriptor(&self) -> &EngineDescriptor;

    fn id(&self) -> &str {
        &self.descriptor().id
    }

    /// Pure: what an operation would do, for the statement gateway. Must classify every language the engine's sessions
    /// run, every `invoke` name its connections answer (unknown names get `refused`), row changes, transactions and
    /// "New database". [`classify_common`] covers the last three.
    fn classify(&self, input: &ClassifyInput<'_>) -> Classification;

    /// Checks a browse filter before it reaches the engine (structured arity, raw language allowed). The gateway also
    /// parses raw SQL filters as one condition.
    fn validate_filter(&self, filter: &Filter) -> AppResult<()> {
        default_validate_filter(self.descriptor(), filter)
    }

    fn parse_connection_string(&self, _text: &str) -> AppResult<ParsedConnection> {
        Err(unsupported_capability("connection strings"))
    }

    fn format_connection_string(&self, _spec: &ConnectionSpec) -> Option<String> {
        None
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>>;

    /// A read in the engine's default language of at most `limit` distinct values of `column` among the first `sample`
    /// rows of `object` with how many of those rows hold each (value first, count second), identifiers quoted by the
    /// engine's dialect (`SELECT c, COUNT(*) FROM (SELECT c FROM t LIMIT sample) s GROUP BY c LIMIT limit`): the
    /// assistant's value hints, run through the gateway like any assistant read. `None` when the engine has no such
    /// statement (document, key-value and search engines).
    fn distinct_values_statement(&self, _object: &ObjectRef, _column: &ColumnInfo, _sample: u64, _limit: u64) -> Option<String> {
        None
    }
}

#[async_trait]
pub trait Connection: Send + Sync + 'static {
    fn engine_id(&self) -> &str;
    fn info(&self) -> &ServerInfo;
    /// What this server offers; a subset of the descriptor's capabilities.
    fn capabilities(&self) -> CapabilitySet;
    /// Session languages; empty means the descriptor's list.
    fn languages(&self) -> Vec<LanguageSpec> {
        Vec::new()
    }
    /// Extra entries for the connection row's menu.
    fn connection_actions(&self) -> Vec<NodeAction> {
        Vec::new()
    }
    async fn children(&self, request: &TreeRequest, ctx: &CallContext) -> AppResult<TreeChildren>;
    async fn describe(&self, object: &ObjectRef, ctx: &CallContext) -> AppResult<ObjectDescription>;
    async fn open_session(&self, options: &SessionOptions, ctx: &CallContext) -> AppResult<OpenedSession>;
    /// Engine-specific operations used by engine views (`"redis.info"`, `"vector.search"`). Every name must also be
    /// classified by `Engine::classify(Capability { .. })`; the gateway refuses unclassified names.
    async fn invoke(&self, name: &str, _args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        Err(unsupported_capability(name))
    }
    fn tabular(&self) -> Option<&dyn TabularData> {
        None
    }
    fn schema_graph(&self) -> Option<&dyn SchemaGraphSource> {
        None
    }
    fn admin(&self) -> Option<&dyn DatabaseAdmin> {
        None
    }
    /// What the engine adds to the assistant's learned profile after the structural walk (field paths, mappings, key
    /// patterns); none adds nothing.
    fn profile_extras(&self) -> Option<&dyn ProfileExtras> {
        None
    }
    async fn ping(&self) -> AppResult<()> {
        Ok(())
    }
    async fn close(&self);
}

/// Browse and edit rows or documents.
#[async_trait]
pub trait TabularData: Send + Sync {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page>;
    /// Applies all changes in one transaction where the engine has them, each update and delete guarded by its
    /// `RowChange::check`. When any change matches no row, nothing is applied and the error is `CONFLICT`
    /// (`tabular::conflict_error`) with the current values re-read.
    async fn apply(&self, changes: &RowChangeSet, ctx: &CallContext) -> AppResult<ApplyResult>;
}

#[async_trait]
pub trait SchemaGraphSource: Send + Sync {
    async fn schema_graph(&self, database: &str, ctx: &CallContext) -> AppResult<SchemaGraph>;
}

#[async_trait]
pub trait DatabaseAdmin: Send + Sync {
    /// MongoDB materialises a database only with a collection in it, so it needs `first_collection`.
    async fn create_database(&self, name: &str, first_collection: Option<&str>, ctx: &CallContext) -> AppResult<()>;
}

/// The classification of inputs every engine treats the same way: row changes, transactions and "New database".
/// `None` for statements and capability calls, which the engine classifies itself.
pub fn classify_common(input: &ClassifyInput<'_>) -> Option<Classification> {
    match input {
        ClassifyInput::Changes(set) => Some(classify_changes(set)),
        ClassifyInput::Transaction(op) => Some(classify_transaction(*op)),
        ClassifyInput::CreateDatabase { name } => Some(classify_create_database(name)),
        ClassifyInput::Statement { .. } | ClassifyInput::Capability { .. } => None,
    }
}

/// The default filter check: structured conditions need the right number of values; a raw filter must be in the
/// engine's raw filter language.
pub fn default_validate_filter(descriptor: &EngineDescriptor, filter: &Filter) -> AppResult<()> {
    match filter {
        Filter::None => Ok(()),
        Filter::Conditions { conditions, .. } => {
            if !descriptor.filter.structured {
                return Err(unsupported_capability("column filters"));
            }
            conditions.iter().try_for_each(|condition| condition.validate())
        }
        Filter::Raw { language, .. } => {
            if descriptor.filter.raw != Some(*language) || !descriptor.capabilities.contains(Capability::RawFilter) {
                return Err(unsupported_capability("this filter language"));
            }
            Ok(())
        }
    }
}

/// `UNSUPPORTED_CAPABILITY` unless `connection` advertises `capability`.
pub fn require(capabilities: CapabilitySet, capability: Capability) -> AppResult<()> {
    if capabilities.contains(capability) {
        Ok(())
    } else {
        Err(unsupported_capability(&capability.name()))
    }
}

/// Splits `host:port` with a default port (IPv6 in brackets).
pub fn split_host_port(text: &str, default_port: u16) -> AppResult<(String, u16)> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| AppError::new(format!("'{text}' is not a valid host")))?;
        let port = match after.strip_prefix(':') {
            Some(port) => port.parse().map_err(|_| AppError::new(format!("'{port}' is not a valid port")))?,
            None => default_port,
        };
        return Ok((host.to_owned(), port));
    }
    match text.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Ok((
            host.to_owned(),
            port.parse().map_err(|_| AppError::new(format!("'{port}' is not a valid port")))?,
        )),
        _ => Ok((text.to_owned(), default_port)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_port_split_with_defaults_and_ipv6() {
        assert_eq!(split_host_port("db:3307", 3306).unwrap(), ("db".into(), 3307));
        assert_eq!(split_host_port("db", 3306).unwrap(), ("db".into(), 3306));
        assert_eq!(split_host_port("[::1]:5433", 5432).unwrap(), ("::1".into(), 5433));
        assert!(split_host_port("db:x", 1).is_err());
    }
}
