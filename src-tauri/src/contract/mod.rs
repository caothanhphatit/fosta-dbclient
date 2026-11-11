//! The engine contract (ADR 0001/0002): the types and traits every engine implements and the core, the commands and the
//! UI speak. A future `fosta-engine` crate; it depends only on serde, async-trait, tokio, chrono, futures and
//! `crate::error` (a source-scanning test enforces it).
//!
//! - [`value`]: the typed cell (`{ "t", "v" }` on the wire) and parsing of user input.
//! - [`object`]: structured object references and the engine-described tree.
//! - [`describe`]: columns, keys, indexes and DDL of one object.
//! - [`capability`], [`descriptor`]: what an engine and a connection can do, and how to connect to it.
//! - [`spec`]: connection specs, secrets, server info.
//! - [`tabular`]: browsing and editing rows with optimistic concurrency.
//! - [`session`], [`stream`]: query sessions, streamed results, transactions.
//! - [`classify`]: statement categories for the gateway.
//! - [`limits`]: the safety rules' backstops and the per-call context.
//! - [`errors`]: engine errors and the structured codes.
//! - [`vector`]: the `vector.*` operations, built on an engine's [`vector::VectorStore`].
//! - [`profile`]: what an engine adds to the assistant's learned profile of a database.

pub mod capability;
pub mod classify;
pub mod describe;
pub mod descriptor;
pub mod engine;
pub mod errors;
pub mod limits;
pub mod object;
pub mod profile;
pub mod schema;
pub mod session;
pub mod spec;
pub mod stream;
pub mod tabular;
pub mod value;
pub mod vector;

pub use capability::{Capability, CapabilitySet};
pub use classify::{Category, Classification, ClassifyInput, StatementInfo};
pub use describe::{ColumnHints, ColumnInfo, ForeignKeyInfo, IndexInfo, KeyInfo, ObjectDescription, ReferencingKey};
pub use descriptor::{
    AssistantHints, AssistantPrompt, AuthMethodId, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind, FieldSpec,
    FilterSupport, FormSection, LanguageSpec, RawFilterLang, SqlDialectId, TlsModeId, TlsSupport, UrlSpec,
};
pub use engine::{
    classify_common, default_validate_filter, require, Connection, DatabaseAdmin, Engine, ParsedConnection, SchemaGraphSource, TabularData,
};
pub use errors::{not_wired, unsupported_capability, EngineError, EngineResult, ErrorKind};
pub use limits::{cancelled, CallContext, CancellationToken, Limits, CANCELLED};
pub use object::{
    ActionTarget, Badge, BuiltinAction, NodeAction, NodeKey, NodeKind, ObjectNode, ObjectRef, Tone, TreeChildren, TreeRequest,
};
pub use profile::{ExtrasRead, FieldReference, FieldSamples, FieldShape, ObjectExtras, ProfileEnrichment, ProfileExtras, KEY_PATTERN};
pub use schema::{Relation, SchemaColumn, SchemaGraph, SchemaTable};
pub use session::{
    Canceller, OpenedSession, RunRequest, RunSummary, Session, SessionInfo, SessionOptions, TransactionOp, TransactionState, TxState,
};
pub use spec::{
    ConnectInfo, ConnectRequest, ConnectSecrets, ConnectionSpec, Endpoint, SecretSlot, ServerInfo, SshConfig, TlsConfig, TlsMode,
};
pub use stream::{Batcher, CollectSink, Flow, ResultSet, ResultSink, RunOptions, StreamEvent};
pub use tabular::{
    ApplyResult, BrowseRequest, Field, Filter, FilterCond, FilterOp, Page, RowChange, RowChangeSet, RowCheck, RowConflict, SortKey, Total,
};
pub use value::{parse_input, Value, ValueKind};

#[cfg(test)]
mod fixtures;
#[cfg(test)]
pub mod testing;
