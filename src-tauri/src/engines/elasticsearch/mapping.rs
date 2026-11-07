//! Mappings flattened into a field list with types, multi-fields and runtime fields; field capabilities across indices.
//! The field list is what types document values in the UI (dates, keywords, geo points, vectors, nested objects...).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::connection::{segment, EsEngine};
use crate::error::AppResult;

/// A coarse family of field types, for editors and cell renderers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldCategory {
    Text,
    Keyword,
    Numeric,
    Date,
    Boolean,
    Binary,
    Ip,
    Geo,
    Range,
    Object,
    Nested,
    Vector,
    Other,
}

impl FieldCategory {
    pub fn of(field_type: &str) -> Self {
        match field_type {
            "text" | "match_only_text" | "search_as_you_type" | "completion" | "semantic_text" | "annotated_text" => Self::Text,
            "keyword" | "constant_keyword" | "wildcard" | "version" | "flattened" => Self::Keyword,
            "long"
            | "integer"
            | "short"
            | "byte"
            | "double"
            | "float"
            | "half_float"
            | "scaled_float"
            | "unsigned_long"
            | "token_count"
            | "rank_feature"
            | "aggregate_metric_double"
            | "histogram" => Self::Numeric,
            "date" | "date_nanos" => Self::Date,
            "boolean" => Self::Boolean,
            "binary" => Self::Binary,
            "ip" => Self::Ip,
            "geo_point" | "geo_shape" | "point" | "shape" | "xy_point" | "xy_shape" => Self::Geo,
            "integer_range" | "float_range" | "long_range" | "double_range" | "date_range" | "ip_range" => Self::Range,
            "object" => Self::Object,
            "nested" => Self::Nested,
            "dense_vector" | "sparse_vector" | "rank_features" | "knn_vector" => Self::Vector,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    /// Dotted path (`address.city`, `name.keyword` for a multi-field).
    pub path: String,
    /// Mapping type (`keyword`, `date`, `nested`, `object` for plain objects...).
    pub field_type: String,
    pub category: FieldCategory,
    /// For multi-fields: the parent field path.
    pub parent: Option<String>,
    /// The closest enclosing `nested` path; queries on this field need a `nested` query.
    pub nested_path: Option<String>,
    /// Defined under `runtime` (computed at query time).
    pub runtime: bool,
    /// `index: false` fields cannot be searched.
    pub searchable: bool,
    /// `doc_values` / aggregatable (text fields without fielddata are not).
    pub aggregatable: bool,
    /// Date format, analyzer, vector dims and similarity, scaling factor...
    pub options: BTreeMap<String, Value>,
}

/// The mapping of one index, flattened.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexMapping {
    pub index: String,
    pub fields: Vec<FieldInfo>,
    /// `dynamic` setting at the root (`true`, `false`, `strict`, `runtime`).
    pub dynamic: Option<String>,
    /// The raw mapping, for the JSON view.
    pub raw: Value,
}

const OPTION_KEYS: &[&str] = &[
    "format",
    "analyzer",
    "search_analyzer",
    "normalizer",
    "dims",
    "similarity",
    "index_options",
    "element_type",
    "scaling_factor",
    "ignore_above",
    "null_value",
    "copy_to",
    "script",
    "path",
    "dimension",
    "method",
    "inference_id",
    "time_series_dimension",
    "time_series_metric",
];

/// Flattens `{ "properties": ..., "runtime": ... }` of one index.
pub fn flatten_mapping(mappings: &Value) -> Vec<FieldInfo> {
    let mut fields = Vec::new();
    if let Some(properties) = mappings["properties"].as_object() {
        walk(properties, "", None, &mut fields);
    }
    if let Some(runtime) = mappings["runtime"].as_object() {
        for (name, definition) in runtime {
            let field_type = definition["type"].as_str().unwrap_or("keyword").to_string();
            fields.push(FieldInfo {
                path: name.clone(),
                category: FieldCategory::of(&field_type),
                field_type,
                parent: None,
                nested_path: None,
                runtime: true,
                searchable: true,
                aggregatable: true,
                options: collect_options(definition),
            });
        }
    }
    fields
}

fn walk(properties: &serde_json::Map<String, Value>, prefix: &str, nested: Option<&str>, out: &mut Vec<FieldInfo>) {
    for (name, definition) in properties {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        // Plain objects have `properties` and no `type`.
        let field_type = definition["type"].as_str().unwrap_or("object").to_string();
        let category = FieldCategory::of(&field_type);
        let doc_values = definition["doc_values"].as_bool().unwrap_or(true);
        out.push(FieldInfo {
            path: path.clone(),
            field_type: field_type.clone(),
            category,
            parent: None,
            nested_path: nested.map(str::to_string),
            runtime: false,
            searchable: definition["index"]
                .as_bool()
                .unwrap_or(!matches!(category, FieldCategory::Object | FieldCategory::Nested)),
            aggregatable: match category {
                FieldCategory::Text => definition["fielddata"].as_bool().unwrap_or(false),
                FieldCategory::Object | FieldCategory::Nested | FieldCategory::Binary => false,
                FieldCategory::Vector => false,
                _ => doc_values,
            },
            options: collect_options(definition),
        });
        if let Some(multi) = definition["fields"].as_object() {
            for (sub_name, sub_definition) in multi {
                let sub_type = sub_definition["type"].as_str().unwrap_or("keyword").to_string();
                let sub_category = FieldCategory::of(&sub_type);
                out.push(FieldInfo {
                    path: format!("{path}.{sub_name}"),
                    category: sub_category,
                    field_type: sub_type,
                    parent: Some(path.clone()),
                    nested_path: nested.map(str::to_string),
                    runtime: false,
                    searchable: true,
                    aggregatable: sub_category != FieldCategory::Text && sub_definition["doc_values"].as_bool().unwrap_or(true),
                    options: collect_options(sub_definition),
                });
            }
        }
        if let Some(children) = definition["properties"].as_object() {
            let inner_nested = if field_type == "nested" { Some(path.as_str()) } else { nested };
            walk(children, &path, inner_nested, out);
        }
    }
}

fn collect_options(definition: &Value) -> BTreeMap<String, Value> {
    OPTION_KEYS
        .iter()
        .filter_map(|key| definition.get(*key).map(|value| (key.to_string(), value.clone())))
        .collect()
}

/// One field across the indices of a pattern (`_field_caps`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldCapability {
    pub name: String,
    /// One entry per type; more than one means the indices disagree (a mapping conflict).
    pub types: Vec<FieldTypeCapability>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldTypeCapability {
    pub field_type: String,
    pub category: FieldCategory,
    pub searchable: bool,
    pub aggregatable: bool,
    /// The indices with this type when the field has a conflict; `None` means all of them.
    pub indices: Option<Vec<String>>,
}

impl EsEngine {
    /// Mappings of every index matching `index` (a name, alias, data stream or pattern).
    pub async fn mappings(&self, index: &str) -> AppResult<Vec<IndexMapping>> {
        let body = self.client.get(&format!("/{}/_mapping", segment(index))).await?;
        let mut result = Vec::new();
        if let Some(indices) = body.as_object() {
            for (name, entry) in indices {
                let mappings = &entry["mappings"];
                result.push(IndexMapping {
                    index: name.clone(),
                    fields: flatten_mapping(mappings),
                    dynamic: match &mappings["dynamic"] {
                        Value::String(text) => Some(text.clone()),
                        Value::Bool(flag) => Some(flag.to_string()),
                        _ => None,
                    },
                    raw: mappings.clone(),
                });
            }
        }
        result.sort_by(|a, b| a.index.cmp(&b.index));
        Ok(result)
    }

    /// `_field_caps` for `fields` (default `*`) across `index`; metadata fields (`_id`, `_index`...) are left out.
    pub async fn field_caps(&self, index: &str, fields: Option<&str>) -> AppResult<Vec<FieldCapability>> {
        let path = format!("/{}/_field_caps?fields={}", segment(index), segment(fields.unwrap_or("*")));
        let body = self.client.get(&path).await?;
        let mut result: Vec<FieldCapability> = body["fields"]
            .as_object()
            .map(|fields| {
                fields
                    .iter()
                    .filter(|(name, _)| !name.starts_with('_'))
                    .map(|(name, types)| FieldCapability {
                        name: name.clone(),
                        types: types
                            .as_object()
                            .map(|types| {
                                types
                                    .iter()
                                    .map(|(field_type, caps)| FieldTypeCapability {
                                        field_type: field_type.clone(),
                                        category: FieldCategory::of(field_type),
                                        searchable: caps["searchable"].as_bool().unwrap_or(false),
                                        aggregatable: caps["aggregatable"].as_bool().unwrap_or(false),
                                        indices: caps["indices"]
                                            .as_array()
                                            .map(|indices| indices.iter().filter_map(|index| index.as_str().map(str::to_string)).collect()),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    /// Field path → category for typing document values, from `_field_caps` (works across indices and data streams;
    /// conflicting fields map to `Other`).
    pub async fn field_types(&self, index: &str) -> AppResult<BTreeMap<String, FieldCategory>> {
        Ok(self
            .field_caps(index, None)
            .await?
            .into_iter()
            .map(|field| {
                let category = match field.types.as_slice() {
                    [single] => single.category,
                    _ => FieldCategory::Other,
                };
                (field.name, category)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_mappings() {
        let mappings = json!({
            "dynamic": "strict",
            "properties": {
                "name": { "type": "text", "analyzer": "english", "fields": { "keyword": { "type": "keyword", "ignore_above": 256 } } },
                "created": { "type": "date", "format": "strict_date_optional_time" },
                "location": { "type": "geo_point" },
                "embedding": { "type": "dense_vector", "dims": 4, "similarity": "cosine" },
                "address": { "properties": { "city": { "type": "keyword" } } },
                "variants": { "type": "nested", "properties": { "sku": { "type": "keyword" }, "price": { "type": "scaled_float", "scaling_factor": 100 } } },
                "raw": { "type": "keyword", "index": false, "doc_values": false }
            },
            "runtime": { "day": { "type": "keyword", "script": { "source": "emit('x')" } } }
        });
        let fields = flatten_mapping(&mappings);
        let find = |path: &str| fields.iter().find(|field| field.path == path).unwrap_or_else(|| panic!("{path}"));
        assert_eq!(find("name").category, FieldCategory::Text);
        assert!(!find("name").aggregatable);
        assert_eq!(find("name.keyword").parent.as_deref(), Some("name"));
        assert!(find("name.keyword").aggregatable);
        assert_eq!(find("created").options["format"], "strict_date_optional_time");
        assert_eq!(find("location").category, FieldCategory::Geo);
        assert_eq!(find("embedding").category, FieldCategory::Vector);
        assert_eq!(find("embedding").options["dims"], 4);
        assert_eq!(find("address").field_type, "object");
        assert_eq!(find("address.city").nested_path, None);
        assert_eq!(find("variants").category, FieldCategory::Nested);
        assert_eq!(find("variants.sku").nested_path.as_deref(), Some("variants"));
        assert!(!find("raw").searchable && !find("raw").aggregatable);
        assert!(find("day").runtime);
        assert_eq!(fields.len(), 12);
    }
}
