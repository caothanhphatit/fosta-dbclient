//! Fingerprints of a database's structure, from metadata only (no row is read): one per object (kind, name, columns),
//! one for the database (every object's), and the cheap checks the stale test recomputes (the listing, the schema
//! graph). SHA-256 over a canonical text, as lower-case hex.

use std::collections::BTreeMap;

use serde::Serialize;

use super::model::{DatabaseProfile, ObjectProfile};
use crate::contract::SchemaGraph;

pub fn sha256_hex(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One column as the fingerprint sees it.
pub struct ColumnShape<'a> {
    pub name: &'a str,
    pub type_name: &'a str,
    pub nullable: bool,
    pub primary: bool,
}

/// The fingerprint of one object: its kind, qualified name and columns, in column-name order (a reorder is no change).
pub fn object_fingerprint<'a>(kind: &str, qualified_name: &str, columns: impl IntoIterator<Item = ColumnShape<'a>>) -> String {
    let mut lines: Vec<String> = columns
        .into_iter()
        .map(|column| {
            format!(
                "{}\t{}\t{}\t{}",
                column.name,
                column.type_name,
                u8::from(column.nullable),
                u8::from(column.primary)
            )
        })
        .collect();
    lines.sort_unstable();
    sha256_hex(&format!("{kind}\n{qualified_name}\n{}", lines.join("\n")))
}

/// The fingerprint of an object whose engine names a metadata-only basis for it (MongoDB: options, validator and index
/// specs), instead of columns that come from sampled documents and move between builds.
pub fn basis_fingerprint(kind: &str, qualified_name: &str, basis: &str) -> String {
    sha256_hex(&format!("{kind}\n{qualified_name}\nbasis\n{basis}"))
}

pub fn of_object(object: &ObjectProfile) -> String {
    object_fingerprint(
        object.kind.as_str(),
        &object.qualified(),
        object.columns.iter().map(|column| ColumnShape {
            name: &column.name,
            type_name: &column.type_name,
            nullable: column.nullable,
            primary: column.primary,
        }),
    )
}

/// The fingerprint of a database: its objects' fingerprints by qualified name and kind.
pub fn database_fingerprint(objects: &[ObjectProfile]) -> String {
    let mut lines: Vec<String> = objects
        .iter()
        .map(|object| format!("{}\t{}\t{}", object.qualified(), object.kind, object.fingerprint))
        .collect();
    lines.sort_unstable();
    sha256_hex(&lines.join("\n"))
}

/// The fingerprint of a listing: the objects' qualified names and kinds.
pub fn listing_fingerprint<'a>(objects: impl IntoIterator<Item = (String, &'a str)>) -> String {
    let mut lines: Vec<String> = objects.into_iter().map(|(name, kind)| format!("{name}\t{kind}")).collect();
    lines.sort_unstable();
    sha256_hex(&lines.join("\n"))
}

/// The fingerprint of a schema graph: tables with their columns (row counts left out: they move all the time).
pub fn graph_fingerprint(graph: &SchemaGraph) -> String {
    let mut lines: Vec<String> = graph
        .tables
        .iter()
        .map(|table| {
            let mut columns: Vec<String> = table
                .columns
                .iter()
                .map(|column| {
                    format!(
                        "{}:{}:{}:{}",
                        column.name,
                        column.column_type,
                        u8::from(column.nullable),
                        u8::from(column.primary)
                    )
                })
                .collect();
            columns.sort_unstable();
            format!("{}\t{}\t{}", table.name, table.kind, columns.join(","))
        })
        .collect();
    lines.sort_unstable();
    lines.push(format!("total {}", graph.total_tables));
    sha256_hex(&lines.join("\n"))
}

/// How the objects of two builds of one database compare, by qualified name and kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDiff {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

impl ObjectDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Per-object comparison of `before` and `after` by fingerprint.
pub fn diff(before: &DatabaseProfile, after: &DatabaseProfile) -> ObjectDiff {
    let index = |profile: &DatabaseProfile| -> BTreeMap<(String, String), String> {
        profile
            .objects
            .iter()
            .map(|object| ((object.qualified(), object.kind.to_string()), object.fingerprint.clone()))
            .collect()
    };
    let (old, new) = (index(before), index(after));
    let mut outcome = ObjectDiff::default();
    for (key, fingerprint) in &new {
        match old.get(key) {
            None => outcome.added.push(key.0.clone()),
            Some(previous) if previous != fingerprint => outcome.changed.push(key.0.clone()),
            Some(_) => outcome.unchanged += 1,
        }
    }
    outcome.removed = old.keys().filter(|key| !new.contains_key(*key)).map(|key| key.0.clone()).collect();
    outcome
}
