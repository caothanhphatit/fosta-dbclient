//! What MongoDB adds to the assistant's learned profile (`ProfileExtras`):
//!
//! - metadata: each collection's type, options (validator, capped, time series, collation…) and index specs, as the
//!   basis of its fingerprint, so a profile does not look changed because another sample of documents was described;
//! - reads: `mongo.schema` over a random sample of [`SAMPLE_SIZE`] documents per collection, with reference probing,
//!   within [`MAX_TIME_MS`] (never `system.*`, never views);
//! - what the answers add: every dotted path (nested documents and documents in arrays) with its types (several listed
//!   as `int | string`, arrays as `array<string>`) and the share of documents holding it, the most frequent values (or a
//!   few samples) for the caller to gate, and inferred references to other collections' `_id` with the share of probed
//!   values found.

use futures_util::{stream, StreamExt, TryStreamExt};
use mongodb::{
    bson::{self, Bson, Document},
    results::CollectionType,
    Client, IndexModel,
};
use serde_json::{json, Value as Json};

use super::values::ID_FIELD;
use crate::{
    contract::{ExtrasRead, FieldReference, FieldSamples, FieldShape, NodeKind, ObjectExtras, ObjectRef, ProfileEnrichment, ValueKind},
    error::AppResult,
};

/// Documents sampled per collection, and the server-side time bound of one read.
pub const SAMPLE_SIZE: u64 = 500;
pub const MAX_TIME_MS: u64 = 5_000;
/// Collections whose index specs are read at once.
const CONCURRENCY: usize = 8;
const SYSTEM_PREFIX: &str = "system.";

fn learned(object: &ObjectRef) -> bool {
    (object.kind == NodeKind::COLLECTION || object.kind == NodeKind::VIEW)
        && object.database.is_some()
        && !object.name.starts_with(SYSTEM_PREFIX)
}

fn extended(document: Document) -> String {
    Bson::Document(document).into_relaxed_extjson().to_string()
}

/// The fingerprint basis of one collection: its type, options and index specs (sorted by name).
pub fn basis(view: bool, options: Document, mut indexes: Vec<(String, Document)>) -> String {
    indexes.sort_by(|a, b| a.0.cmp(&b.0));
    let indexes: Vec<String> = indexes.into_iter().map(|(_, spec)| extended(spec)).collect();
    format!(
        "type {}\noptions {}\nindexes {}",
        if view { "view" } else { "collection" },
        extended(options),
        indexes.join("\n")
    )
}

fn index_spec(index: &IndexModel) -> (String, Document) {
    let name = index.options.as_ref().and_then(|options| options.name.clone()).unwrap_or_default();
    (name, bson::to_document(index).unwrap_or_default())
}

/// Collection options and index specs of the learned collections of `database`; a collection whose indexes cannot be
/// read keeps the default fingerprint.
pub async fn metadata(client: &Client, database: &str, objects: &[ObjectRef]) -> AppResult<ProfileEnrichment> {
    let wanted: Vec<&ObjectRef> = objects
        .iter()
        .filter(|object| learned(object) && object.database.as_deref() == Some(database))
        .collect();
    let mut enrichment = ProfileEnrichment::default();
    if wanted.is_empty() {
        return Ok(enrichment);
    }
    let db = client.database(database);
    let specs: Vec<_> = db.list_collections().await?.try_collect().await?;
    let found: Vec<(ObjectRef, bool, Document)> = wanted
        .into_iter()
        .filter_map(|object| {
            let spec = specs.iter().find(|spec| spec.name == object.name)?;
            let view = matches!(spec.collection_type, CollectionType::View);
            Some((object.clone(), view, bson::to_document(&spec.options).unwrap_or_default()))
        })
        .collect();
    let described: Vec<Option<ObjectExtras>> = stream::iter(found)
        .map(|(object, view, options)| {
            let db = db.clone();
            async move {
                let indexes = if view {
                    Vec::new()
                } else {
                    let listed: Vec<IndexModel> = db
                        .collection::<Document>(&object.name)
                        .list_indexes()
                        .await
                        .ok()?
                        .try_collect()
                        .await
                        .ok()?;
                    listed.iter().map(index_spec).collect()
                };
                let mut extras = ObjectExtras::new(object);
                extras.fingerprint_basis = Some(basis(view, options, indexes));
                Some(extras)
            }
        })
        .buffered(CONCURRENCY)
        .collect()
        .await;
    enrichment.objects = described.into_iter().flatten().collect();
    Ok(enrichment)
}

/// One `mongo.schema` read per collection (views and `system.*` left out).
pub fn reads(database: &str, objects: &[ObjectRef]) -> Vec<ExtrasRead> {
    objects
        .iter()
        .filter(|object| learned(object) && object.kind == NodeKind::COLLECTION && object.database.as_deref() == Some(database))
        .map(|object| ExtrasRead {
            object: Some(object.clone()),
            name: "mongo.schema".into(),
            args: json!({
                "database": database,
                "collection": object.name,
                "sampleMode": "random",
                "sampleSize": SAMPLE_SIZE,
                "references": true,
                "maxTimeMS": MAX_TIME_MS,
            }),
        })
        .collect()
}

fn kind_of(type_name: &str) -> ValueKind {
    match type_name {
        "string" | "symbol" => ValueKind::Text,
        "int" | "long" => ValueKind::Int,
        "double" => ValueKind::Float,
        "decimal" => ValueKind::Decimal,
        "bool" => ValueKind::Bool,
        "date" => ValueKind::TimestampTz,
        "objectId" => ValueKind::ObjectId,
        "object" => ValueKind::Document,
        "array" => ValueKind::Json,
        "binData" => ValueKind::Bytes,
        _ => ValueKind::Other,
    }
}

/// The non-null type names of a `types` list (`[{ type, count }]`, most frequent first).
fn type_names(types: &Json) -> Vec<String> {
    types
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["type"].as_str())
        .filter(|name| *name != "null" && *name != "undefined")
        .map(str::to_owned)
        .collect()
}

/// One path of a `mongo.schema` report as a field: types listed (arrays with their element types), presence.
pub fn field_of(path: &Json) -> Option<FieldShape> {
    let name = path["path"].as_str()?.to_owned();
    let types = type_names(&path["types"]);
    let elements = type_names(&path["array"]["elementTypes"]);
    let shown: Vec<String> = types
        .iter()
        .map(|type_name| {
            if type_name == "array" && !elements.is_empty() {
                format!("array<{}>", elements.join("|"))
            } else {
                type_name.clone()
            }
        })
        .collect();
    let has_null = path["types"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry["type"].as_str() == Some("null"));
    let percent = path["percent"].as_f64().unwrap_or(0.0) as f32;
    let primary = name == ID_FIELD;
    Some(FieldShape {
        kind: types.first().map_or(ValueKind::Other, |first| kind_of(first)),
        type_name: if shown.is_empty() { "null".to_owned() } else { shown.join(" | ") },
        nullable: !primary && (has_null || percent < 100.0),
        primary,
        presence: Some(percent),
        path: name,
    })
}

fn text_of(value: &Json) -> Option<String> {
    match value {
        Json::String(text) => Some(text.clone()),
        Json::Bool(value) => Some(value.to_string()),
        Json::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The values a path showed: its most frequent values while it has few (`more` when it has more distinct values
/// than listed), else a few samples (`more`: high cardinality).
pub fn samples_of(path: &Json) -> Option<FieldSamples> {
    let name = path["path"].as_str()?.to_owned();
    let (values, more) = match path["topValues"].as_array() {
        Some(top) => {
            let values: Vec<String> = top.iter().filter_map(|entry| text_of(&entry["value"])).collect();
            let distinct = path["distinct"].as_u64().unwrap_or(values.len() as u64);
            let more = distinct > values.len() as u64 || values.len() < top.len();
            (values, more)
        }
        None => (path["samples"].as_array().into_iter().flatten().filter_map(text_of).collect(), true),
    };
    (!values.is_empty()).then_some(FieldSamples { path: name, values, more })
}

/// What one collection's `mongo.schema` answer adds.
pub fn interpret_schema(object: &ObjectRef, answer: &Json) -> (ObjectExtras, Vec<FieldReference>) {
    let paths = answer["paths"].as_array().cloned().unwrap_or_default();
    let mut extras = ObjectExtras::new(object.clone());
    let mut fields: Vec<FieldShape> = paths.iter().filter_map(field_of).collect();
    // `_id` first, like the described columns.
    fields.sort_by_key(|field| field.path != ID_FIELD);
    extras.fields = Some(fields);
    extras.samples = paths.iter().filter_map(samples_of).collect();
    if let Some(count) = answer["sample"]["count"].as_u64() {
        extras.facts.push(("sampled documents".into(), count.to_string()));
    }
    if answer["truncated"].as_bool() == Some(true) || answer["depthCapped"].as_bool() == Some(true) {
        extras
            .facts
            .push(("fields".into(), "only some paths were analysed (caps met)".into()));
    }
    let references = paths
        .iter()
        .filter_map(|path| {
            let reference = path.get("reference").filter(|reference| reference.is_object())?;
            let probed = reference["probed"].as_u64().filter(|probed| *probed > 0)?;
            let matched = reference["matched"].as_u64().unwrap_or(0);
            Some(FieldReference {
                from: object.name.clone(),
                path: path["path"].as_str()?.to_owned(),
                to: reference["collection"].as_str()?.to_owned(),
                to_field: ID_FIELD.to_owned(),
                confidence: (matched as f32 / probed as f32).min(1.0),
            })
        })
        .collect();
    (extras, references)
}

pub fn interpret(answers: &[(ExtrasRead, Json)]) -> ProfileEnrichment {
    let mut enrichment = ProfileEnrichment::default();
    for (read, answer) in answers {
        let Some(object) = read.object.as_ref().filter(|_| read.name == "mongo.schema") else {
            continue;
        };
        let (extras, references) = interpret_schema(object, answer);
        enrichment.objects.push(extras);
        enrichment.references.extend(references);
    }
    enrichment
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;

    use super::*;

    #[test]
    fn schema_paths_become_fields_samples_and_references() {
        let answer = json!({
            "sample": { "mode": "random", "size": 500, "count": 4 },
            "paths": [
                { "path": "status", "percent": 100.0, "types": [{ "type": "string", "count": 4 }],
                  "distinct": 2, "topValues": [{ "value": "paid", "count": 3 }, { "value": "new", "count": 1 }], "samples": ["paid"] },
                { "path": "_id", "percent": 100.0, "types": [{ "type": "objectId", "count": 4 }], "topValues": null, "samples": [] },
                { "path": "age", "percent": 75.0, "types": [{ "type": "int", "count": 2 }, { "type": "string", "count": 1 }, { "type": "null", "count": 1 }],
                  "distinct": 30, "topValues": [{ "value": 1, "count": 1 }], "samples": [1] },
                { "path": "tags", "percent": 50.0, "types": [{ "type": "array", "count": 2 }],
                  "array": { "elementTypes": [{ "type": "string", "count": 3 }] }, "topValues": null, "samples": ["a", "b"] },
                { "path": "customerId", "percent": 100.0, "types": [{ "type": "objectId", "count": 4 }],
                  "reference": { "collection": "customers", "matched": 3, "probed": 4, "inferred": true } },
            ],
            "truncated": false,
            "depthCapped": false,
        });
        let object = ObjectRef::new(Some("shop"), None, "orders", NodeKind::COLLECTION);
        let (extras, references) = interpret_schema(&object, &answer);
        let fields = extras.fields.unwrap();
        assert_eq!(fields[0].path, "_id");
        assert!(fields[0].primary && !fields[0].nullable);
        let age = fields.iter().find(|field| field.path == "age").unwrap();
        assert_eq!(
            (age.type_name.as_str(), age.kind.clone(), age.nullable),
            ("int | string", ValueKind::Int, true)
        );
        let tags = fields.iter().find(|field| field.path == "tags").unwrap();
        assert_eq!((tags.type_name.as_str(), tags.presence), ("array<string>", Some(50.0)));
        let samples = |path: &str| extras.samples.iter().find(|sample| sample.path == path).cloned();
        assert_eq!(samples("status").unwrap().values, ["paid", "new"]);
        assert!(!samples("status").unwrap().more);
        assert!(samples("age").unwrap().more, "30 distinct values, one listed");
        assert!(samples("tags").unwrap().more, "samples of a high-cardinality path");
        assert!(samples("_id").is_none());
        assert_eq!(references.len(), 1);
        assert_eq!((references[0].to.as_str(), references[0].confidence), ("customers", 0.75));
        assert!(extras.facts.contains(&("sampled documents".to_owned(), "4".to_owned())));
    }

    #[test]
    fn reads_leave_out_views_and_system_collections() {
        let objects = [
            ObjectRef::new(Some("shop"), None, "orders", NodeKind::COLLECTION),
            ObjectRef::new(Some("shop"), None, "recent", NodeKind::VIEW),
            ObjectRef::new(Some("shop"), None, "system.profile", NodeKind::COLLECTION),
        ];
        let reads = reads("shop", &objects);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].args["collection"], "orders");
        assert_eq!(
            (reads[0].args["sampleMode"].as_str(), reads[0].args["references"].as_bool()),
            (Some("random"), Some(true))
        );
    }

    #[test]
    fn the_basis_ignores_index_order() {
        let a = basis(
            false,
            doc! {},
            vec![("b".into(), doc! { "key": { "b": 1 } }), ("a".into(), doc! { "key": { "a": 1 } })],
        );
        let b = basis(
            false,
            doc! {},
            vec![("a".into(), doc! { "key": { "a": 1 } }), ("b".into(), doc! { "key": { "b": 1 } })],
        );
        assert_eq!(a, b);
        assert_ne!(a, basis(false, doc! { "validator": { "a": { "$exists": true } } }, Vec::new()));
    }
}
