//! The stored shape of a database profile (version 1): what the assistant learned about one database of one connection.
//! Every field the later phases fill (value hints, the summary, descriptions written by the model, engine extras) is
//! already here, empty until then, so a version 1 file stays readable as they arrive.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::contract::{NodeKind, ValueKind};

/// The version written to new profiles; a file of another version is set aside like a damaged one.
pub const PROFILE_VERSION: u32 = 1;

/// What the assistant knows about one database (`connection_id` + `database`; an empty `database` for engines without
/// databases, such as Elasticsearch).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseProfile {
    pub version: u32,
    /// The engine id (`postgres`, `mongo`, …).
    pub engine: String,
    pub connection_id: String,
    pub database: String,
    /// Hash of every learned object's fingerprint, sorted: changes when any object's name, kind or columns change.
    pub fingerprint: String,
    /// The cheap fingerprints the stale check recomputes without describing every object.
    #[serde(default)]
    pub checks: StructureChecks,
    /// When the first build ran, and when the latest build or refresh ran (milliseconds since the Unix epoch).
    pub first_learned_at: i64,
    pub learned_at: i64,
    /// Whether any row or document values were read while learning (value hints, P4). Structure alone reads none.
    #[serde(default)]
    pub data_read: bool,
    /// The model that wrote the summary and descriptions, and the tokens they took (P5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Tables, views, collections and search indexes, in the engine's order.
    pub objects: Vec<ObjectProfile>,
    /// Objects the database had when listed (more than `objects.len()` when the listing was cut).
    pub total_objects: usize,
    /// Objects described in detail: at most [`super::MAX_DETAILED_OBJECTS`]; the others keep a name and a kind.
    pub detailed_objects: usize,
    /// The listing or the detail was cut (too many objects, or the size cap of the file).
    #[serde(default)]
    pub truncated: bool,
    /// System and credential objects left out on purpose.
    #[serde(default)]
    pub excluded_objects: usize,
    pub relations: Vec<RelationProfile>,
    pub routines: Vec<RoutineProfile>,
    /// The model's summary of the database (P5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ProfileSummary>,
    /// The user's own knowledge (glossary, rules); never overwritten by a build or a refresh.
    #[serde(default)]
    pub notes: Vec<ProfileNote>,
    /// Engine-specific knowledge (MongoDB field references, Redis key patterns, Elasticsearch mappings; P6).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extras: Map<String, Value>,
}

impl DatabaseProfile {
    /// The object called `name` (`table` or `schema.table`), matched exactly first, then ignoring case.
    pub fn object(&self, name: &str) -> Option<&ObjectProfile> {
        self.objects
            .iter()
            .find(|object| object.matches(name, false))
            .or_else(|| self.objects.iter().find(|object| object.matches(name, true)))
    }

    pub fn object_mut(&mut self, name: &str) -> Option<&mut ObjectProfile> {
        let index = self
            .objects
            .iter()
            .position(|object| object.matches(name, false))
            .or_else(|| self.objects.iter().position(|object| object.matches(name, true)))?;
        self.objects.get_mut(index)
    }
}

/// Fingerprints that need no `describe` per object: the listing (names and kinds) and, where the engine draws schema
/// diagrams, its schema graph (tables and columns, one call).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureChecks {
    pub listing: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    /// Hash of the kind, the name and the columns (name, type, nullability, primary key), in column-name order.
    pub fingerprint: String,
    /// Described: columns, keys and indexes are known. Objects past the detail cap have a name and a kind only.
    pub detailed: bool,
    /// The engine's comment on the object (untrusted text, cut and scrubbed of secret-looking values).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default)]
    pub columns: Vec<ColumnProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_key: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unique_keys: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexProfile>,
    /// From the engine's statistics; never a count of rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_estimate: Option<i64>,
    /// Engine facts (ClickHouse table engine, SQLite `WITHOUT ROWID`), scrubbed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<(String, String)>,
    /// Sample-derived hints on column values (P4; only where the user lets the assistant read data).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_hints: Vec<ValueHint>,
    /// What the object is for, written by the model (P5) or the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
}

impl ObjectProfile {
    /// `schema.name`, or `name` without a schema.
    pub fn qualified(&self) -> String {
        qualified(self.schema.as_deref(), &self.name)
    }

    fn matches(&self, name: &str, loose: bool) -> bool {
        let same = |a: &str, b: &str| if loose { a.eq_ignore_ascii_case(b) } else { a == b };
        same(&self.name, name) || same(&self.qualified(), name)
    }

    pub fn column(&self, name: &str) -> Option<&ColumnProfile> {
        self.columns.iter().find(|column| column.name == name)
    }
}

/// `schema.name`, or `name` without a schema.
pub fn qualified(schema: Option<&str>, name: &str) -> String {
    match schema.filter(|schema| !schema.is_empty()) {
        Some(schema) => format!("{schema}.{name}"),
        None => name.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnProfile {
    pub name: String,
    /// The engine's spelling of the type.
    pub type_name: String,
    pub kind: ValueKind,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The name looks like a credential or personal data (`password`, `api_key`, `ssn`…): its comment and default are
    /// dropped and its values are never sampled.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProfile {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// Where a relation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationSource {
    /// A declared foreign key.
    Fk,
    /// A column named after another object (`customer_id` → `customers.id`).
    InferredName,
    /// A MongoDB field holding another collection's `_id` (P6).
    MongoRef,
}

/// `from.columns` → `to.to_columns`; objects as [`ObjectProfile::qualified`] names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub from: String,
    pub columns: Vec<String>,
    pub to: String,
    pub to_columns: Vec<String>,
    /// Set when the referenced object is in another database (MySQL cross-database keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_database: Option<String>,
    pub source: RelationSource,
    /// 1.0 for declared keys; below for inferred ones.
    pub confidence: f32,
}

/// A function or procedure: its name and the signature the tree shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Values a column takes (at most 20 of 64 characters) or the format they follow (P4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueHint {
    pub column: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DescriptionSource {
    Llm,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Description {
    pub text: String,
    pub source: DescriptionSource,
    pub updated_at: i64,
    /// The object's fingerprint when the model wrote the description (none for the user's): a summary run with
    /// `onlyChanged` describes again the objects whose fingerprint moved on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub text: String,
    #[serde(default)]
    pub model: String,
    pub created_at: i64,
}

/// One entry of the user's glossary: free text, optionally about one object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileNote {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}
