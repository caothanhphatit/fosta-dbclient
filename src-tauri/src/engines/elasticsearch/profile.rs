//! What Elasticsearch and OpenSearch add to the assistant's learned profile (`ProfileExtras`), from metadata only (no
//! document is read): every mapped field of an index, alias or data stream as a dotted path with its type (objects and
//! nested fields, multi-fields such as `title.keyword`, runtime fields; types that differ across the indices behind an
//! alias listed together), the indices behind aliases and data streams, and the aliases and data streams of the cluster
//! (hidden and system ones, such as `.security` or `.kibana`, left out).

use std::collections::BTreeMap;

use futures_util::{stream, StreamExt};
use serde_json::{json, Map, Value as Json};

use super::{
    connection::EsEngine,
    mapping::{FieldInfo, IndexMapping},
    values::kind_of,
};
use crate::{
    contract::{FieldShape, NodeKind, ObjectExtras, ObjectRef, ProfileEnrichment, ValueKind},
    error::AppResult,
};

/// Objects whose mappings are read at once, and the most fields kept per object.
const CONCURRENCY: usize = 8;
const MAX_FIELDS: usize = 500;
/// Indices named per alias or data stream.
const MAX_NAMED: usize = 20;
const ID: &str = "_id";
const INDEX: &str = "_index";

fn key_field(path: &str) -> FieldShape {
    FieldShape {
        path: path.to_owned(),
        type_name: path.to_owned(),
        kind: ValueKind::Text,
        nullable: false,
        primary: true,
        presence: None,
    }
}

fn shown_type(field: &FieldInfo) -> String {
    let mut text = field.field_type.clone();
    if field.runtime {
        text.push_str(" (runtime)");
    }
    if let Some(nested) = field.nested_path.as_deref().filter(|nested| *nested != field.path) {
        text.push_str(&format!(" (in nested {nested})"));
    }
    text
}

/// The fields of one index, alias or data stream: `_id` (and `_index` when several indices answer) first, then every
/// mapped path in mapping order, types that differ between indices listed together.
pub fn fields_of(mappings: &[IndexMapping], concrete: bool) -> Vec<FieldShape> {
    let mut fields = Vec::new();
    if !concrete {
        fields.push(key_field(INDEX));
    }
    fields.push(key_field(ID));
    let mut order: Vec<String> = Vec::new();
    let mut types: BTreeMap<String, (Vec<String>, ValueKind)> = BTreeMap::new();
    for mapping in mappings {
        for field in &mapping.fields {
            let entry = types.entry(field.path.clone()).or_insert_with(|| {
                order.push(field.path.clone());
                (Vec::new(), kind_of(&field.field_type))
            });
            let shown = shown_type(field);
            if !entry.0.contains(&shown) {
                entry.0.push(shown);
            }
        }
    }
    for path in order.into_iter().take(MAX_FIELDS) {
        let (names, kind) = types.remove(&path).unwrap_or((Vec::new(), ValueKind::Other));
        fields.push(FieldShape {
            path,
            type_name: names.join(" | "),
            kind,
            nullable: true,
            primary: false,
            presence: None,
        });
    }
    fields
}

fn named(names: &[String]) -> String {
    let mut shown: Vec<&str> = names.iter().take(MAX_NAMED).map(String::as_str).collect();
    let more = names.len().saturating_sub(MAX_NAMED);
    let more_text = format!("+{more} more");
    if more > 0 {
        shown.push(&more_text);
    }
    shown.join(", ")
}

/// A hidden or system name (`.security-7`, `.kibana`); the backing indices of a visible data stream (`.ds-logs-…`)
/// are not hidden.
fn hidden_name(name: &str) -> bool {
    match name.strip_prefix(".ds-") {
        Some(stream) => stream.starts_with('.'),
        None => name.starts_with('.'),
    }
}

/// Aliases with their indices, leaving out hidden aliases and aliases whose indices are all hidden or system ones
/// (`.security`, `.kibana_7.17`), and the hidden indices of the others.
pub fn visible_aliases(aliases: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, Vec<String>> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (alias, index) in aliases {
        if !hidden_name(&alias) && !hidden_name(&index) {
            grouped.entry(alias).or_default().push(index);
        }
    }
    grouped
}

/// The mappings of every learned object, and the cluster's aliases and data streams.
pub async fn metadata(engine: &EsEngine, objects: &[ObjectRef]) -> AppResult<ProfileEnrichment> {
    let wanted: Vec<ObjectRef> = objects
        .iter()
        .filter(|object| object.kind == NodeKind::COLLECTION || object.kind == NodeKind::SEARCH_INDEX)
        .cloned()
        .collect();
    let described: Vec<Option<ObjectExtras>> = stream::iter(wanted)
        .map(|object| async move {
            let mappings = engine.mappings(&object.name).await.ok()?;
            let concrete = mappings.len() == 1 && mappings[0].index == object.name;
            let mut extras = ObjectExtras::new(object);
            extras.fields = Some(fields_of(&mappings, concrete));
            if !concrete {
                let indices: Vec<String> = mappings.iter().map(|mapping| mapping.index.clone()).collect();
                extras.facts.push(("Indices".into(), named(&indices)));
            }
            let dynamic: Vec<&str> = mappings.iter().filter_map(|mapping| mapping.dynamic.as_deref()).collect();
            if let Some(first) = dynamic.first() {
                extras.facts.push(("Dynamic mapping".into(), (*first).to_owned()));
            }
            Some(extras)
        })
        .buffered(CONCURRENCY)
        .collect()
        .await;
    let mut enrichment = ProfileEnrichment {
        objects: described.into_iter().flatten().collect(),
        ..ProfileEnrichment::default()
    };
    let (aliases, streams) = futures_util::join!(engine.list_aliases(false), engine.list_data_streams(false));
    let grouped = visible_aliases(aliases.unwrap_or_default().into_iter().map(|alias| (alias.alias, alias.index)));
    if !grouped.is_empty() {
        let aliases: Map<String, Json> = grouped.into_iter().map(|(alias, indices)| (alias, json!(indices))).collect();
        enrichment.extras.insert("aliases".into(), Json::Object(aliases));
    }
    let streams: Map<String, Json> = streams
        .unwrap_or_default()
        .into_iter()
        .filter(|stream| !stream.hidden && !hidden_name(&stream.name))
        .map(|stream| {
            (
                stream.name,
                json!({
                    "timestampField": stream.timestamp_field,
                    "backingIndices": stream.backing_indices.len(),
                    "template": stream.template,
                }),
            )
        })
        .collect();
    if !streams.is_empty() {
        enrichment.extras.insert("dataStreams".into(), Json::Object(streams));
    }
    Ok(enrichment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::elasticsearch::mapping::flatten_mapping;

    fn mapping(index: &str, mappings: Json) -> IndexMapping {
        IndexMapping {
            index: index.into(),
            fields: flatten_mapping(&mappings),
            dynamic: None,
            raw: mappings,
        }
    }

    #[test]
    fn mappings_flatten_into_dotted_fields() {
        let logs = mapping(
            "logs-1",
            json!({
                "properties": {
                    "title": { "type": "text", "fields": { "keyword": { "type": "keyword" } } },
                    "user": { "properties": { "name": { "type": "keyword" }, "age": { "type": "integer" } } },
                    "items": { "type": "nested", "properties": { "sku": { "type": "keyword" } } },
                    "code": { "type": "keyword" },
                },
                "runtime": { "day": { "type": "keyword" } },
            }),
        );
        let fields = fields_of(std::slice::from_ref(&logs), true);
        let shown: Vec<(&str, &str)> = fields.iter().map(|field| (field.path.as_str(), field.type_name.as_str())).collect();
        assert_eq!(
            shown,
            [
                ("_id", "_id"),
                ("title", "text"),
                ("title.keyword", "keyword"),
                ("user", "object"),
                ("user.name", "keyword"),
                ("user.age", "integer"),
                ("items", "nested"),
                ("items.sku", "keyword (in nested items)"),
                ("code", "keyword"),
                ("day", "keyword (runtime)"),
            ]
        );
        assert_eq!(fields.iter().find(|field| field.path == "user.age").unwrap().kind, ValueKind::Int);
        assert!(fields[0].primary && !fields[1].primary);
        // Behind an alias: `_index` first, and types that differ between indices listed together.
        let other = mapping(
            "logs-2",
            json!({ "properties": { "code": { "type": "long" }, "extra": { "type": "boolean" } } }),
        );
        let merged = fields_of(&[logs, other], false);
        assert_eq!((merged[0].path.as_str(), merged[1].path.as_str()), ("_index", "_id"));
        let code = merged.iter().find(|field| field.path == "code").unwrap();
        assert_eq!(code.type_name, "keyword | long");
        assert!(merged.iter().any(|field| field.path == "extra"));
    }

    #[test]
    fn hidden_aliases_and_system_indices_are_left_out() {
        let pairs = [
            (".security", ".security-7"),
            (".kibana", ".kibana_7.17.0_001"),
            ("audit", ".internal-audit"),
            ("logs", "logs-1"),
            ("logs", ".hidden-logs"),
            ("events", ".ds-events-2026.11.12-000001"),
            ("secret-stream", ".ds-.hidden-stream-000001"),
        ];
        let visible = visible_aliases(pairs.iter().map(|(alias, index)| ((*alias).to_owned(), (*index).to_owned())));
        let shown: Vec<(&str, Vec<&str>)> = visible
            .iter()
            .map(|(alias, indices)| (alias.as_str(), indices.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(shown, [("events", vec![".ds-events-2026.11.12-000001"]), ("logs", vec!["logs-1"])]);
        assert!(hidden_name(".geoip_databases") && !hidden_name("orders"));
    }

    #[test]
    fn long_lists_of_indices_are_cut() {
        let names: Vec<String> = (0..25).map(|index| format!("i{index}")).collect();
        let text = named(&names);
        assert!(text.ends_with("+5 more") && text.starts_with("i0, i1"));
    }
}
