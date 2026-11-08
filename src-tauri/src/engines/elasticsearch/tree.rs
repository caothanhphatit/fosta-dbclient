//! The object tree and `describe`.
//!
//! The top level has three folders: Indices, Data streams (when the cluster has them) and Aliases. Indices, data
//! streams and aliases are `collection` nodes, so the shared document grid and design view open them; each shows its
//! document count and size, and health or state as a badge. Node ids are stable: `folder:<name>`, `index:<name>`,
//! `stream:<name>`, `alias:<name>`. Long lists page with a cursor (the offset) and honour the tree's name filter.
//!
//! `describe` returns the fields of the mapping as columns (the declared key as primary key), the document count and
//! the mapping itself as the DDL, plus settings, aliases and size as extras.

use std::collections::BTreeMap;

use serde_json::{json, Value as Json};

use super::{
    catalog::IndexSummary,
    connection::{segment, EsEngine},
    descriptor::Flavor,
    grid::{self, ID},
};
use crate::{
    contract::{
        Badge, BuiltinAction, KeyInfo, NodeAction, NodeKey, NodeKind, ObjectDescription, ObjectNode, ObjectRef, Tone, TreeChildren,
        TreeRequest,
    },
    error::{AppError, AppResult},
};

const PAGE: usize = 500;
const INDICES: &str = "folder:indices";
const STREAMS: &str = "folder:streams";
const ALIASES: &str = "folder:aliases";

/// Human-readable bytes (`12.3 MB`).
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn docs(count: u64) -> String {
    if count == 1 {
        "1 doc".to_owned()
    } else {
        format!("{count} docs")
    }
}

fn folder(id: &str, label: &str, icon: &str, detail: Option<String>) -> ObjectNode {
    ObjectNode {
        key: NodeKey {
            id: id.to_owned(),
            kind: NodeKind::FOLDER,
            object: None,
        },
        label: label.to_owned(),
        kind: NodeKind::FOLDER,
        icon: icon.to_owned(),
        object: None,
        detail,
        badges: Vec::new(),
        expandable: true,
        default_view: None,
        actions: vec![NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh)],
    }
}

/// A browsable node (index, data stream or alias): kind `collection`, so the grid and the design view open it.
fn collection(prefix: &str, name: &str, icon: &str, detail: Option<String>, badges: Vec<Badge>, kind_label: &str) -> ObjectNode {
    let object = ObjectRef {
        path: vec![kind_label.to_owned()],
        ..ObjectRef::new(None, None, name, NodeKind::COLLECTION)
    };
    let search = format!(
        "GET /{}/_search\n{{\n  \"query\": {{ \"match_all\": {{}} }},\n  \"size\": 20\n}}",
        segment(name)
    );
    let mut actions = vec![
        NodeAction::view("design", "Mapping", "design"),
        NodeAction::query("search", "Search in console", &search),
        NodeAction::query("mapping", "Get mapping", &format!("GET /{}/_mapping", segment(name))),
        NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
    ];
    if kind_label == "index" {
        actions.push(
            NodeAction::query("delete", "Delete index…", &format!("DELETE /{}", segment(name)))
                .danger()
                .separated(),
        );
    } else if kind_label == "dataStream" {
        actions.push(
            NodeAction::query("delete", "Delete data stream…", &format!("DELETE /_data_stream/{}", segment(name)))
                .danger()
                .separated(),
        );
    }
    ObjectNode {
        key: NodeKey {
            id: format!("{prefix}:{name}"),
            kind: NodeKind::COLLECTION,
            object: Some(object.clone()),
        },
        label: name.to_owned(),
        kind: NodeKind::COLLECTION,
        icon: icon.to_owned(),
        object: Some(object),
        detail,
        badges,
        expandable: false,
        default_view: Some("data".into()),
        actions,
    }
}

fn index_node(index: &IndexSummary) -> ObjectNode {
    let detail = match (index.docs_count, index.store_bytes) {
        (Some(count), Some(size)) => Some(format!("{} · {}", docs(count), bytes(size))),
        (Some(count), None) => Some(docs(count)),
        _ => None,
    };
    let mut badges = Vec::new();
    if index.status == "close" {
        badges.push(Badge {
            text: "closed".into(),
            tone: Tone::Warn,
        });
    }
    match index.health.as_deref() {
        Some("yellow") => badges.push(Badge {
            text: "yellow".into(),
            tone: Tone::Warn,
        }),
        Some("red") => badges.push(Badge {
            text: "red".into(),
            tone: Tone::Danger,
        }),
        _ => {}
    }
    collection("index", &index.name, "table", detail, badges, "index")
}

/// One page of `nodes` after the name filter, with a cursor for the rest.
fn paged(nodes: Vec<ObjectNode>, request: &TreeRequest) -> TreeChildren {
    let filter = request.filter.as_deref().map(str::to_lowercase).filter(|text| !text.is_empty());
    let nodes: Vec<ObjectNode> = match filter {
        Some(filter) => nodes
            .into_iter()
            .filter(|node| node.label.to_lowercase().contains(&filter))
            .collect(),
        None => nodes,
    };
    let total = nodes.len();
    let start = request
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = request.limit.map_or(PAGE, |limit| (limit as usize).clamp(1, 5000));
    let end = (start + limit).min(total);
    TreeChildren {
        nodes: nodes.into_iter().skip(start).take(limit).collect(),
        next_cursor: (end < total).then(|| end.to_string()),
        total_hint: Some(total as u64),
    }
}

pub async fn children(engine: &EsEngine, request: &TreeRequest) -> AppResult<TreeChildren> {
    let Some(parent) = &request.parent else {
        let mut nodes = vec![folder(INDICES, "Indices", "layers", None)];
        if engine.features().data_streams {
            nodes.push(folder(STREAMS, "Data streams", "logs", None));
        }
        nodes.push(folder(ALIASES, "Aliases", "bookmark", None));
        return Ok(TreeChildren {
            total_hint: Some(nodes.len() as u64),
            nodes,
            next_cursor: None,
        });
    };
    let nodes = match parent.id.as_str() {
        INDICES => engine.list_indices(false).await?.iter().map(index_node).collect(),
        STREAMS => {
            let (streams, indices) = futures_util::join!(engine.list_data_streams(false), engine.list_indices(true));
            let indices: BTreeMap<String, IndexSummary> = indices
                .unwrap_or_default()
                .into_iter()
                .map(|index| (index.name.clone(), index))
                .collect();
            streams?
                .iter()
                .map(|stream| {
                    let backing: Vec<&IndexSummary> = stream.backing_indices.iter().filter_map(|name| indices.get(name)).collect();
                    let count: u64 = backing.iter().filter_map(|index| index.docs_count).sum();
                    let size: u64 = backing.iter().filter_map(|index| index.store_bytes).sum();
                    let detail = format!("{} · {} · {} backing", docs(count), bytes(size), stream.backing_indices.len());
                    let badges = match stream.status.as_deref().map(str::to_ascii_lowercase).as_deref() {
                        Some("yellow") => vec![Badge {
                            text: "yellow".into(),
                            tone: Tone::Warn,
                        }],
                        Some("red") => vec![Badge {
                            text: "red".into(),
                            tone: Tone::Danger,
                        }],
                        _ => Vec::new(),
                    };
                    collection("stream", &stream.name, "logs", Some(detail), badges, "dataStream")
                })
                .collect()
        }
        ALIASES => {
            let (aliases, indices) = futures_util::join!(engine.list_aliases(false), engine.list_indices(true));
            let indices: BTreeMap<String, IndexSummary> = indices
                .unwrap_or_default()
                .into_iter()
                .map(|index| (index.name.clone(), index))
                .collect();
            let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for alias in aliases? {
                grouped.entry(alias.alias).or_default().push(alias.index);
            }
            grouped
                .iter()
                .map(|(alias, targets)| {
                    let count: u64 = targets.iter().filter_map(|name| indices.get(name)?.docs_count).sum();
                    let size: u64 = targets.iter().filter_map(|name| indices.get(name)?.store_bytes).sum();
                    let target = if targets.len() == 1 {
                        format!("→ {}", targets[0])
                    } else {
                        format!("→ {} indices", targets.len())
                    };
                    let detail = format!("{target} · {} · {}", docs(count), bytes(size));
                    collection("alias", alias, "bookmark", Some(detail), Vec::new(), "alias")
                })
                .collect()
        }
        _ => Vec::new(),
    };
    Ok(paged(nodes, request))
}

/// The fields of the mapping as columns, the key, the count and the mapping as the DDL.
pub async fn describe(engine: &EsEngine, flavor: Flavor, object: &ObjectRef) -> AppResult<ObjectDescription> {
    let target = grid::resolve(engine, &object.name).await?;
    let name = segment(&object.name);
    let mapping_path = format!("/{name}/_mapping");
    let settings_path = format!("/{name}/_settings?flat_settings=true");
    let (mapping, settings, count) = futures_util::join!(
        engine.client().get(&mapping_path),
        engine.client().get(&settings_path),
        engine.count(&object.name, None),
    );
    let mapping = mapping?;
    let mut description = ObjectDescription::new(object.clone());
    description.columns = target.column_infos();
    let key = target.key.as_ref().map_or_else(|| ID.to_owned(), |key| key.name.clone());
    description.primary_key = Some(KeyInfo {
        name: None,
        columns: target.key_columns(),
    });
    description.row_estimate = count.ok().and_then(|count| i64::try_from(count).ok());
    // One concrete index shows its mapping as is; an alias or pattern shows each index's.
    let ddl = if target.concrete {
        mapping[&object.name]["mappings"].clone()
    } else {
        Json::Object(
            mapping
                .as_object()
                .into_iter()
                .flatten()
                .map(|(index, entry)| (index.clone(), entry["mappings"].clone()))
                .collect(),
        )
    };
    description.ddl = Some(serde_json::to_string_pretty(&json!({ "mappings": ddl })).unwrap_or_default());
    let mut extra = vec![
        (
            "Kind".to_owned(),
            object
                .path
                .first()
                .cloned()
                .unwrap_or_else(|| if target.concrete { "index" } else { "alias or pattern" }.into()),
        ),
        ("Key".to_owned(), key),
        ("Engine".to_owned(), flavor.label().to_owned()),
    ];
    if !target.concrete {
        extra.push(("Indices".to_owned(), target.indices.join(", ")));
    }
    if let Ok(settings) = settings {
        if let Some(first) = settings.as_object().and_then(|indices| indices.values().next()) {
            for (label, key) in [
                ("Shards", "index.number_of_shards"),
                ("Replicas", "index.number_of_replicas"),
                ("Created", "index.creation_date"),
                ("Lifecycle policy", "index.lifecycle.name"),
                ("Default pipeline", "index.default_pipeline"),
            ] {
                if let Some(value) = first["settings"][key].as_str() {
                    extra.push((label.to_owned(), value.to_owned()));
                }
            }
        }
    }
    description.extra = extra;
    if description.columns.len() <= 1 && !target.concrete && target.indices.is_empty() {
        return Err(AppError::new(format!("{} matches no index.", object.name)));
    }
    Ok(description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_counts_read_well() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(docs(1), "1 doc");
        assert_eq!(docs(0), "0 docs");
    }

    #[test]
    fn index_nodes_are_browsable_collections_with_stable_keys() {
        let summary = IndexSummary {
            name: "products".into(),
            uuid: None,
            health: Some("yellow".into()),
            status: "open".into(),
            primaries: Some(1),
            replicas: Some(1),
            docs_count: Some(300),
            docs_deleted: Some(0),
            store_bytes: Some(2048),
            primary_store_bytes: Some(2048),
            creation_date: None,
            hidden: false,
            data_stream: None,
        };
        let node = index_node(&summary);
        assert!(node.kind.is_relation());
        assert_eq!(node.key.id, "index:products");
        assert_eq!(node.default_view.as_deref(), Some("data"));
        assert_eq!(node.detail.as_deref(), Some("300 docs · 2.0 KB"));
        assert_eq!(node.badges[0].tone, Tone::Warn);
        assert!(node.actions.iter().any(|action| action.danger));
        assert_eq!(node, index_node(&summary));
    }

    #[test]
    fn pages_follow_the_cursor_and_filter() {
        let nodes: Vec<ObjectNode> = (0..7)
            .map(|i| collection("index", &format!("logs-{i}"), "table", None, Vec::new(), "index"))
            .collect();
        let first = paged(
            nodes.clone(),
            &TreeRequest {
                limit: Some(3),
                ..TreeRequest::default()
            },
        );
        assert_eq!(
            (first.nodes.len(), first.next_cursor.as_deref(), first.total_hint),
            (3, Some("3"), Some(7))
        );
        let last = paged(
            nodes.clone(),
            &TreeRequest {
                limit: Some(3),
                cursor: Some("6".into()),
                ..TreeRequest::default()
            },
        );
        assert_eq!((last.nodes.len(), last.next_cursor), (1, None));
        let filtered = paged(
            nodes,
            &TreeRequest {
                filter: Some("LOGS-5".into()),
                ..TreeRequest::default()
            },
        );
        assert_eq!(filtered.nodes.len(), 1);
    }
}
