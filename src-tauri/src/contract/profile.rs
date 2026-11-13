//! What an engine adds to the assistant's learned profile of a database after the structural walk
//! ([`Connection::profile_extras`]): MongoDB field paths and references, Elasticsearch mappings and aliases, Redis key
//! patterns.
//!
//! The hook has two halves. [`ProfileExtras::metadata`] reads metadata only, like `describe` (mappings, collection
//! options, index specs). Reads of data (sampled documents, key names) are never run by the engine itself: it names
//! them in [`ProfileExtras::reads`] as engine operations (`invoke` names and arguments, each classified by the engine
//! like any other), the caller runs them through its own gate (the gateway, as the assistant, audited) and hands the
//! answers back to [`ProfileExtras::interpret`]. The caller decides what of the answers it keeps (sampled values only
//! where the user lets the assistant read data).
//!
//! [`Connection::profile_extras`]: super::Connection::profile_extras

use async_trait::async_trait;
use serde_json::{Map, Value as Json};

use super::{CallContext, NodeKind, ObjectRef, ValueKind};
use crate::error::AppResult;

/// One field of an object as the engine sees it: a column, a dotted document path, a mapping field.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldShape {
    /// The column name, or a dotted path (`address.city`, `items.sku`).
    pub path: String,
    /// The engine's spelling of the type; several types of one field are listed as `int | string`.
    pub type_name: String,
    pub kind: ValueKind,
    pub nullable: bool,
    pub primary: bool,
    /// The share of sampled documents holding the field, in percent (document engines).
    pub presence: Option<f32>,
}

/// Values seen in one field of a sample: the most frequent ones (or a few samples), `more` when the field had more
/// distinct values than listed. `counts` says how many sampled documents held each value (same order), empty when the
/// engine does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSamples {
    pub path: String,
    pub values: Vec<String>,
    pub more: bool,
    pub counts: Vec<u64>,
}

/// A field of one object holding another object's key (a MongoDB ObjectId field matched against `_id`s).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldReference {
    pub from: String,
    pub path: String,
    pub to: String,
    pub to_field: String,
    /// The share of probed values found in `to` (0 to 1).
    pub confidence: f32,
}

/// What the engine adds to one object. An object the structural walk did not list (a Redis key pattern) is added with
/// `object.kind` as its kind.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectExtras {
    pub object: ObjectRef,
    /// Text that identifies the object's structure from metadata only, used for its fingerprint instead of its columns
    /// (MongoDB: options, validator and index specs; the sampled fields move between builds).
    pub fingerprint_basis: Option<String>,
    /// The object's fields, replacing the described columns when set.
    pub fields: Option<Vec<FieldShape>>,
    /// Engine facts (`type`: `hash`, `targets`: two indices), shown with the object.
    pub facts: Vec<(String, String)>,
    /// Sampled values per field; the caller keeps them only where it may.
    pub samples: Vec<FieldSamples>,
    pub row_estimate: Option<i64>,
}

impl ObjectExtras {
    pub fn new(object: ObjectRef) -> Self {
        Self {
            object,
            fingerprint_basis: None,
            fields: None,
            facts: Vec::new(),
            samples: Vec::new(),
            row_estimate: None,
        }
    }
}

/// What the engine adds to a profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileEnrichment {
    pub objects: Vec<ObjectExtras>,
    pub references: Vec<FieldReference>,
    /// Database-level knowledge (aliases, data streams, key pattern counts), under one key per kind.
    pub extras: Map<String, Json>,
    /// Why something was left out (a cap met, a read that failed).
    pub note: Option<String>,
}

impl ProfileEnrichment {
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.references.is_empty() && self.extras.is_empty()
    }
}

/// A read the engine asks the caller to run: an engine operation (`invoke`) the engine classifies as a read, about one
/// object or (`object` none) the whole database.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtrasRead {
    pub object: Option<ObjectRef>,
    pub name: String,
    pub args: Json,
}

/// The engine half of profile learning; every method defaults to adding nothing.
#[async_trait]
pub trait ProfileExtras: Send + Sync {
    /// Metadata-only knowledge about `objects` of `database` (no row, document or key read), like `describe`.
    async fn metadata(&self, _database: &str, _objects: &[ObjectRef], _ctx: &CallContext) -> AppResult<ProfileEnrichment> {
        Ok(ProfileEnrichment::default())
    }

    /// The bounded reads of data the engine would learn from (sampled documents, a SCAN of key names).
    fn reads(&self, _database: &str, _objects: &[ObjectRef]) -> Vec<ExtrasRead> {
        Vec::new()
    }

    /// What the answers to [`reads`](Self::reads) add; reads that failed or were refused are not among them.
    fn interpret(&self, _database: &str, _answers: &[(ExtrasRead, Json)]) -> ProfileEnrichment {
        ProfileEnrichment::default()
    }
}

/// The kind of profile objects that stand for a family of keys (`user:{id}:profile`) rather than one object.
pub const KEY_PATTERN: NodeKind = NodeKind::of("keyPattern");
