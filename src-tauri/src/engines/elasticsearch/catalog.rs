//! The object tree: indices, data streams, aliases, templates, ingest pipelines, lifecycle policies (ILM on
//! Elasticsearch, ISM on OpenSearch) and snapshot repositories, plus the details of one index.
//!
//! Sections are fetched concurrently and fail independently: a user without `manage_ilm` still sees the indices, with
//! an error on the policies section.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    connection::{segment, EsEngine, LifecycleApi},
    mapping::IndexMapping,
};
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section<T> {
    pub items: Vec<T>,
    /// Why the section could not be read (permissions, API missing on this distribution).
    pub error: Option<String>,
}

impl<T> Section<T> {
    fn from(result: AppResult<Vec<T>>) -> Self {
        match result {
            Ok(items) => Self { items, error: None },
            Err(error) => Self {
                items: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogOptions {
    /// Include hidden and system indices (`.kibana`, `.ds-*` backing indices...).
    pub include_hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub indices: Section<IndexSummary>,
    pub data_streams: Section<DataStreamInfo>,
    pub aliases: Section<AliasInfo>,
    pub index_templates: Section<IndexTemplate>,
    pub component_templates: Section<ComponentTemplate>,
    pub legacy_templates: Section<LegacyTemplate>,
    pub pipelines: Section<PipelineInfo>,
    pub lifecycle_policies: Section<LifecyclePolicy>,
    pub snapshot_repositories: Section<SnapshotRepository>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSummary {
    pub name: String,
    pub uuid: Option<String>,
    /// green / yellow / red; `None` for closed indices.
    pub health: Option<String>,
    /// open / close.
    pub status: String,
    pub primaries: Option<u32>,
    pub replicas: Option<u32>,
    /// Lucene documents, as `_cat/indices` reports them: nested objects count as documents of their own, so an index
    /// with nested fields shows more than `_count`. [`IndexDetails`] has the exact count.
    pub docs_count: Option<u64>,
    pub docs_deleted: Option<u64>,
    pub store_bytes: Option<u64>,
    pub primary_store_bytes: Option<u64>,
    pub creation_date: Option<String>,
    pub hidden: bool,
    /// The data stream this is a backing index of.
    pub data_stream: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataStreamInfo {
    pub name: String,
    pub timestamp_field: String,
    pub backing_indices: Vec<String>,
    pub generation: u64,
    pub status: Option<String>,
    pub template: Option<String>,
    pub ilm_policy: Option<String>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasInfo {
    pub alias: String,
    pub index: String,
    pub is_write_index: Option<bool>,
    pub has_filter: bool,
    pub routing: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexTemplate {
    pub name: String,
    pub index_patterns: Vec<String>,
    pub priority: Option<i64>,
    pub composed_of: Vec<String>,
    pub data_stream: bool,
    pub version: Option<i64>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentTemplate {
    pub name: String,
    pub version: Option<i64>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyTemplate {
    pub name: String,
    pub index_patterns: Vec<String>,
    pub order: Option<i64>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineInfo {
    pub id: String,
    pub description: Option<String>,
    /// Processor types in order (`set`, `rename`, `date`...).
    pub processors: Vec<String>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecyclePolicy {
    pub name: String,
    /// `ilm` (Elasticsearch phases) or `ism` (OpenSearch states).
    pub kind: LifecycleApi,
    /// ILM phases (`hot`, `warm`, `delete`...) or ISM states.
    pub stages: Vec<String>,
    /// ILM: indices using the policy (from `in_use_by`). ISM: index patterns from `ism_template`.
    pub used_by: Vec<String>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRepository {
    pub name: String,
    pub repository_type: String,
    pub settings: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    pub snapshot: String,
    pub state: Option<String>,
    pub indices: Vec<String>,
    pub start_time: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Everything the index tab shows.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDetails {
    pub name: String,
    pub mapping: Option<IndexMapping>,
    /// Flat settings (`index.number_of_shards`...), including defaults when asked.
    pub settings: BTreeMap<String, Value>,
    pub aliases: Vec<String>,
    pub stats: Option<IndexStats>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    /// Top-level documents (`_count`), nested objects not included.
    pub docs_count: u64,
    /// Lucene documents in the primaries, nested objects included.
    pub lucene_docs: u64,
    pub docs_deleted: u64,
    pub store_bytes: u64,
    pub primary_store_bytes: u64,
    pub segments: u64,
    pub indexing_total: u64,
    pub search_query_total: u64,
    pub search_query_time_ms: u64,
}

fn number<T: std::str::FromStr>(value: &Value) -> Option<T> {
    match value {
        Value::String(text) => text.parse().ok(),
        Value::Number(number) => number.to_string().parse().ok(),
        _ => None,
    }
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect(),
        Value::String(text) => vec![text.clone()],
        _ => Vec::new(),
    }
}

impl EsEngine {
    /// The whole catalog, sections fetched concurrently.
    pub async fn catalog(&self, options: &CatalogOptions) -> Catalog {
        let (indices, data_streams, aliases, index_templates, component_templates, legacy_templates, pipelines, policies, repositories) = futures_util::join!(
            self.list_indices(options.include_hidden),
            self.list_data_streams(options.include_hidden),
            self.list_aliases(options.include_hidden),
            self.list_index_templates(),
            self.list_component_templates(),
            self.list_legacy_templates(),
            self.list_pipelines(),
            self.list_lifecycle_policies(),
            self.list_snapshot_repositories(),
        );
        let mut indices = Section::from(indices);
        // Mark backing indices with their data stream.
        if let Ok(streams) = &data_streams {
            let owner: BTreeMap<&str, &str> = streams
                .iter()
                .flat_map(|stream| {
                    stream
                        .backing_indices
                        .iter()
                        .map(move |index| (index.as_str(), stream.name.as_str()))
                })
                .collect();
            for index in &mut indices.items {
                index.data_stream = owner.get(index.name.as_str()).map(|name| name.to_string());
            }
        }
        Catalog {
            indices,
            data_streams: Section::from(data_streams),
            aliases: Section::from(aliases),
            index_templates: Section::from(index_templates),
            component_templates: Section::from(component_templates),
            legacy_templates: Section::from(legacy_templates),
            pipelines: Section::from(pipelines),
            lifecycle_policies: Section::from(policies),
            snapshot_repositories: Section::from(repositories),
        }
    }

    pub async fn list_indices(&self, include_hidden: bool) -> AppResult<Vec<IndexSummary>> {
        let wildcards = if include_hidden { "all" } else { "open,closed" };
        let path = format!(
            "/_cat/indices?format=json&bytes=b&expand_wildcards={wildcards}&h=health,status,index,uuid,pri,rep,docs.count,docs.deleted,store.size,pri.store.size,creation.date.string"
        );
        let rows = self.client.get(&path).await?;
        let hidden: Vec<String> = if include_hidden {
            let settings = self
                .client
                .get("/_all/_settings/index.hidden?expand_wildcards=all&flat_settings=true")
                .await
                .unwrap_or(Value::Null);
            settings
                .as_object()
                .map(|indices| {
                    indices
                        .iter()
                        .filter(|(_, entry)| entry["settings"]["index.hidden"].as_str() == Some("true"))
                        .map(|(name, _)| name.clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut indices: Vec<IndexSummary> = rows
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        let name = row["index"].as_str().unwrap_or_default().to_string();
                        IndexSummary {
                            hidden: hidden.contains(&name) || name.starts_with('.'),
                            uuid: row["uuid"].as_str().map(str::to_string),
                            health: row["health"].as_str().map(str::to_string),
                            status: row["status"].as_str().unwrap_or("open").to_string(),
                            primaries: number(&row["pri"]),
                            replicas: number(&row["rep"]),
                            docs_count: number(&row["docs.count"]),
                            docs_deleted: number(&row["docs.deleted"]),
                            store_bytes: number(&row["store.size"]),
                            primary_store_bytes: number(&row["pri.store.size"]),
                            creation_date: row["creation.date.string"].as_str().map(str::to_string),
                            data_stream: None,
                            name,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !include_hidden {
            // `_cat/indices` still lists the `.ds-*` backing indices of visible data streams; they belong under their
            // data stream, and other dot indices are system ones.
            indices.retain(|index| !index.hidden);
        }
        indices.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(indices)
    }

    pub async fn list_data_streams(&self, include_hidden: bool) -> AppResult<Vec<DataStreamInfo>> {
        // OpenSearch rejects `expand_wildcards` here (and has no hidden data streams).
        let path = if self.info.is_opensearch() {
            "/_data_stream".to_string()
        } else {
            let wildcards = if include_hidden { "all" } else { "open,closed" };
            format!("/_data_stream?expand_wildcards={wildcards}")
        };
        let body = self.client.get(&path).await?;
        Ok(body["data_streams"]
            .as_array()
            .map(|streams| {
                streams
                    .iter()
                    .map(|stream| DataStreamInfo {
                        name: stream["name"].as_str().unwrap_or_default().to_string(),
                        timestamp_field: stream["timestamp_field"]["name"].as_str().unwrap_or("@timestamp").to_string(),
                        backing_indices: stream["indices"]
                            .as_array()
                            .map(|indices| {
                                indices
                                    .iter()
                                    .filter_map(|index| index["index_name"].as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        generation: stream["generation"].as_u64().unwrap_or(0),
                        status: stream["status"].as_str().map(str::to_string),
                        template: stream["template"].as_str().map(str::to_string),
                        ilm_policy: stream["ilm_policy"].as_str().map(str::to_string),
                        hidden: stream["hidden"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn list_aliases(&self, include_hidden: bool) -> AppResult<Vec<AliasInfo>> {
        let wildcards = if include_hidden { "all" } else { "open,closed" };
        let body = self.client.get(&format!("/_alias?expand_wildcards={wildcards}")).await?;
        let mut aliases = Vec::new();
        for (index, entry) in body.as_object().into_iter().flatten() {
            for (alias, definition) in entry["aliases"].as_object().into_iter().flatten() {
                aliases.push(AliasInfo {
                    alias: alias.clone(),
                    index: index.clone(),
                    is_write_index: definition["is_write_index"].as_bool(),
                    has_filter: definition.get("filter").is_some(),
                    routing: definition["index_routing"]
                        .as_str()
                        .or(definition["routing"].as_str())
                        .map(str::to_string),
                });
            }
        }
        aliases.sort_by(|a, b| (&a.alias, &a.index).cmp(&(&b.alias, &b.index)));
        Ok(aliases)
    }

    pub async fn list_index_templates(&self) -> AppResult<Vec<IndexTemplate>> {
        let body = self.client.get("/_index_template").await?;
        let mut templates: Vec<IndexTemplate> = body["index_templates"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let template = &item["index_template"];
                        IndexTemplate {
                            name: item["name"].as_str().unwrap_or_default().to_string(),
                            index_patterns: strings(&template["index_patterns"]),
                            priority: template["priority"].as_i64(),
                            composed_of: strings(&template["composed_of"]),
                            data_stream: template.get("data_stream").is_some(),
                            version: template["version"].as_i64(),
                            raw: template.clone(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    pub async fn list_component_templates(&self) -> AppResult<Vec<ComponentTemplate>> {
        let body = self.client.get("/_component_template").await?;
        let mut templates: Vec<ComponentTemplate> = body["component_templates"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| ComponentTemplate {
                        name: item["name"].as_str().unwrap_or_default().to_string(),
                        version: item["component_template"]["version"].as_i64(),
                        raw: item["component_template"].clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    pub async fn list_legacy_templates(&self) -> AppResult<Vec<LegacyTemplate>> {
        let body = self.client.get("/_template").await?;
        let mut templates: Vec<LegacyTemplate> = body
            .as_object()
            .map(|items| {
                items
                    .iter()
                    .map(|(name, template)| LegacyTemplate {
                        name: name.clone(),
                        index_patterns: strings(&template["index_patterns"]),
                        order: template["order"].as_i64(),
                        raw: template.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    pub async fn list_pipelines(&self) -> AppResult<Vec<PipelineInfo>> {
        let body = self.client.get("/_ingest/pipeline").await?;
        let mut pipelines: Vec<PipelineInfo> = body
            .as_object()
            .map(|items| {
                items
                    .iter()
                    .map(|(id, pipeline)| PipelineInfo {
                        id: id.clone(),
                        description: pipeline["description"].as_str().map(str::to_string),
                        processors: pipeline["processors"]
                            .as_array()
                            .map(|processors| {
                                processors
                                    .iter()
                                    .filter_map(|processor| processor.as_object()?.keys().next().cloned())
                                    .collect()
                            })
                            .unwrap_or_default(),
                        raw: pipeline.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        pipelines.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(pipelines)
    }

    /// ILM policies (Elasticsearch) or ISM policies (OpenSearch) in one shape.
    pub async fn list_lifecycle_policies(&self) -> AppResult<Vec<LifecyclePolicy>> {
        let mut policies: Vec<LifecyclePolicy> = match self.features().lifecycle {
            LifecycleApi::Ilm => {
                let body = self.client.get("/_ilm/policy").await?;
                body.as_object()
                    .map(|items| {
                        items
                            .iter()
                            .map(|(name, entry)| LifecyclePolicy {
                                name: name.clone(),
                                kind: LifecycleApi::Ilm,
                                stages: entry["policy"]["phases"]
                                    .as_object()
                                    .map(|phases| phases.keys().cloned().collect())
                                    .unwrap_or_default(),
                                used_by: strings(&entry["in_use_by"]["indices"])
                                    .into_iter()
                                    .chain(strings(&entry["in_use_by"]["data_streams"]))
                                    .collect(),
                                raw: entry.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            LifecycleApi::Ism => {
                let body = self.client.get("/_plugins/_ism/policies?size=1000").await?;
                body["policies"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| {
                                let policy = &item["policy"];
                                LifecyclePolicy {
                                    name: item["_id"]
                                        .as_str()
                                        .or(policy["policy_id"].as_str())
                                        .unwrap_or_default()
                                        .to_string(),
                                    kind: LifecycleApi::Ism,
                                    stages: policy["states"]
                                        .as_array()
                                        .map(|states| {
                                            states
                                                .iter()
                                                .filter_map(|state| state["name"].as_str().map(str::to_string))
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                    used_by: policy["ism_template"]
                                        .as_array()
                                        .map(|templates| {
                                            templates.iter().flat_map(|template| strings(&template["index_patterns"])).collect()
                                        })
                                        .unwrap_or_default(),
                                    raw: item.clone(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            LifecycleApi::None => return Err(AppError::new("This cluster has no lifecycle policy API.")),
        };
        policies.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(policies)
    }

    pub async fn list_snapshot_repositories(&self) -> AppResult<Vec<SnapshotRepository>> {
        let body = self.client.get("/_snapshot").await?;
        let mut repositories: Vec<SnapshotRepository> = body
            .as_object()
            .map(|items| {
                items
                    .iter()
                    .map(|(name, repository)| SnapshotRepository {
                        name: name.clone(),
                        repository_type: repository["type"].as_str().unwrap_or_default().to_string(),
                        settings: repository["settings"].clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        repositories.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(repositories)
    }

    pub async fn list_snapshots(&self, repository: &str) -> AppResult<Vec<SnapshotInfo>> {
        let body = self.client.get(&format!("/_snapshot/{}/_all", segment(repository))).await?;
        Ok(body["snapshots"]
            .as_array()
            .map(|snapshots| {
                snapshots
                    .iter()
                    .map(|snapshot| SnapshotInfo {
                        snapshot: snapshot["snapshot"].as_str().unwrap_or_default().to_string(),
                        state: snapshot["state"].as_str().map(str::to_string),
                        indices: strings(&snapshot["indices"]),
                        start_time: snapshot["start_time"].as_str().map(str::to_string),
                        duration_ms: snapshot["duration_in_millis"].as_u64(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Mapping, settings, aliases and stats of one index (or the write index of an alias / data stream when `name`
    /// resolves to several, the first one).
    pub async fn index_details(&self, name: &str, include_defaults: bool) -> AppResult<IndexDetails> {
        let target = segment(name);
        let settings_path = format!(
            "/{target}/_settings?flat_settings=true{}",
            if include_defaults { "&include_defaults=true" } else { "" }
        );
        let alias_path = format!("/{target}/_alias");
        let stats_path = format!("/{target}/_stats/docs,store,segments,indexing,search");
        let (mappings, settings, aliases, stats) = futures_util::join!(
            self.mappings(name),
            self.client.get(&settings_path),
            self.client.get(&alias_path),
            self.client.get(&stats_path),
        );
        let settings = settings?;
        let resolved = settings
            .as_object()
            .and_then(|indices| indices.keys().next().cloned())
            .unwrap_or_else(|| name.to_string());
        let mut flat = BTreeMap::new();
        for key in ["settings", "defaults"] {
            if let Some(entries) = settings[&resolved][key].as_object() {
                for (setting, value) in entries {
                    flat.entry(setting.clone()).or_insert_with(|| value.clone());
                }
            }
        }
        let aliases = aliases
            .ok()
            .and_then(|body| {
                body[&resolved]["aliases"]
                    .as_object()
                    .map(|aliases| aliases.keys().cloned().collect())
            })
            .unwrap_or_default();
        let count = self.count(&resolved, None).await.ok();
        let stats = stats.ok().map(|body| {
            let total = &body["indices"][&resolved]["total"];
            let primaries = &body["indices"][&resolved]["primaries"];
            let get = |value: &Value| value.as_u64().unwrap_or(0);
            IndexStats {
                docs_count: count.unwrap_or_else(|| get(&primaries["docs"]["count"])),
                lucene_docs: get(&primaries["docs"]["count"]),
                docs_deleted: get(&primaries["docs"]["deleted"]),
                store_bytes: get(&total["store"]["size_in_bytes"]),
                primary_store_bytes: get(&primaries["store"]["size_in_bytes"]),
                segments: get(&total["segments"]["count"]),
                indexing_total: get(&total["indexing"]["index_total"]),
                search_query_total: get(&total["search"]["query_total"]),
                search_query_time_ms: get(&total["search"]["query_time_in_millis"]),
            }
        });
        let mapping = mappings
            .ok()
            .and_then(|mappings| mappings.into_iter().find(|mapping| mapping.index == resolved));
        Ok(IndexDetails {
            name: resolved,
            mapping,
            settings: flat,
            aliases,
            stats,
        })
    }
}
