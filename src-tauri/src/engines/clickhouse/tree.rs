//! The object tree, loaded lazily one level at a time: databases, then the folders Tables, Views, Materialized views and
//! Dictionaries of a database (empty ones are left out), then the objects. A table shows its engine, rows and size from
//! `system.parts` (active parts only).

use super::{edit::supports_mutations, ChEngine};
use crate::{
    contract::{Badge, BuiltinAction, NodeAction, NodeKey, NodeKind, ObjectNode, ObjectRef, Tone, TreeChildren, TreeRequest},
    engines::common::Result,
    error::{AppError, AppResult},
};

/// Databases that belong to the server; they sort last and carry a "system" note.
const SYSTEM_DATABASES: [&str; 3] = ["system", "INFORMATION_SCHEMA", "information_schema"];

/// The text of the badge that marks a table the grid must not offer edits for. The ClickHouse UI module reads it.
pub const READ_ONLY_BADGE: &str = "read-only";

/// One table-like object of a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEntry {
    pub name: String,
    pub engine: String,
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
}

impl TableEntry {
    fn kind(&self) -> NodeKind {
        match self.engine.as_str() {
            "View" | "LiveView" | "WindowView" => NodeKind::VIEW,
            "MaterializedView" => NodeKind::MATERIALIZED_VIEW,
            "Dictionary" => NodeKind::DICTIONARY,
            _ => NodeKind::TABLE,
        }
    }
}

/// The folders of a database in tree order: id, label and the kind they hold.
fn folders() -> [(&'static str, &'static str, NodeKind); 4] {
    [
        ("tables", "Tables", NodeKind::TABLE),
        ("views", "Views", NodeKind::VIEW),
        ("materialized", "Materialized views", NodeKind::MATERIALIZED_VIEW),
        ("dictionaries", "Dictionaries", NodeKind::DICTIONARY),
    ]
}

fn encode(name: &str) -> String {
    name.replace('%', "%25").replace('/', "%2F")
}

fn decode(name: &str) -> String {
    name.replace("%2F", "/").replace("%25", "%")
}

/// `1234567` as `1.2M`, for the rows note.
fn short_count(value: u64) -> String {
    match value {
        0..=9_999 => value.to_string(),
        10_000..=999_999 => format!("{:.1}K", value as f64 / 1_000.0),
        1_000_000..=999_999_999 => format!("{:.1}M", value as f64 / 1_000_000.0),
        _ => format!("{:.1}B", value as f64 / 1_000_000_000.0),
    }
    .replace(".0", "")
}

/// `52_428_800` as `50 MiB`.
pub fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit]).replace(".0 ", " ")
    }
}

/// `MergeTree · 1.2M rows · 50 MiB`.
fn table_detail(entry: &TableEntry) -> String {
    let mut parts = vec![entry.engine.clone()];
    if supports_mutations(&entry.engine) || entry.rows.is_some_and(|rows| rows > 0) {
        if let Some(rows) = entry.rows {
            parts.push(format!("{} rows", short_count(rows)));
        }
        if let Some(bytes) = entry.bytes {
            parts.push(human_bytes(bytes));
        }
    }
    parts.join(" · ")
}

fn database_node(name: &str) -> ObjectNode {
    let object = ObjectRef::new(Some(name), None, name, NodeKind::DATABASE);
    ObjectNode {
        key: NodeKey {
            id: format!("db:{}", encode(name)),
            kind: NodeKind::DATABASE,
            object: Some(object.clone()),
        },
        label: name.to_owned(),
        kind: NodeKind::DATABASE,
        icon: "database".into(),
        object: Some(object),
        detail: SYSTEM_DATABASES.contains(&name).then(|| "system".to_owned()),
        badges: Vec::new(),
        expandable: true,
        default_view: None,
        actions: vec![
            NodeAction::builtin("newQuery", "New query", BuiltinAction::NewQuery),
            NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
    }
}

fn folder_node(database: &str, id: &str, label: &str, count: usize) -> ObjectNode {
    ObjectNode {
        key: NodeKey {
            id: format!("db:{}/folder:{id}", encode(database)),
            kind: NodeKind::FOLDER,
            object: None,
        },
        label: label.to_owned(),
        kind: NodeKind::FOLDER,
        icon: "layers".into(),
        object: None,
        detail: Some(count.to_string()),
        badges: Vec::new(),
        expandable: true,
        default_view: None,
        actions: Vec::new(),
    }
}

fn object_node(database: &str, folder: &str, entry: &TableEntry) -> ObjectNode {
    let kind = entry.kind();
    let object = ObjectRef::new(Some(database), None, &entry.name, kind.clone());
    let table = kind == NodeKind::TABLE;
    let mut actions = vec![
        NodeAction::view("design", "Design", "design"),
        NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
    ];
    if table {
        actions.push(NodeAction::view("parts", "Parts and mutations", "parts"));
        actions.push(
            NodeAction::query("truncate", "Truncate…", "TRUNCATE TABLE {qualified}")
                .danger()
                .separated(),
        );
    }
    let badges = if table && !supports_mutations(&entry.engine) {
        vec![Badge {
            text: READ_ONLY_BADGE.to_owned(),
            tone: Tone::Neutral,
        }]
    } else {
        Vec::new()
    };
    ObjectNode {
        key: NodeKey {
            id: format!("db:{}/folder:{folder}/{}:{}", encode(database), kind.as_str(), encode(&entry.name)),
            kind: kind.clone(),
            object: Some(object.clone()),
        },
        label: entry.name.clone(),
        icon: if kind == NodeKind::VIEW || kind == NodeKind::MATERIALIZED_VIEW {
            "view"
        } else {
            "table"
        }
        .into(),
        object: Some(object),
        detail: table.then(|| table_detail(entry)),
        badges,
        expandable: false,
        default_view: Some("data".into()),
        actions,
        kind,
    }
}

impl ChEngine {
    /// Database names, the server's own last.
    pub async fn database_names(&self) -> Result<Vec<String>> {
        let rows = self
            .strings(
                "SELECT name FROM system.databases ORDER BY name IN ('system', 'INFORMATION_SCHEMA', 'information_schema'), name",
                &[],
            )
            .await?;
        Ok(rows.into_iter().filter_map(|row| row.into_iter().next()).collect())
    }

    /// The tables, views, materialized views and dictionaries of a database, with rows and bytes of the active parts.
    pub async fn table_entries(&self, database: &str) -> Result<Vec<TableEntry>> {
        let with_parts = self
            .strings(
                "SELECT t.name, t.engine, toString(p.rows), toString(p.bytes) FROM system.tables AS t \
                 LEFT JOIN (SELECT `table` AS name, sum(rows) AS rows, sum(bytes_on_disk) AS bytes FROM system.parts \
                            WHERE database = {db:String} AND active GROUP BY `table`) AS p ON p.name = t.name \
                 WHERE t.database = {db:String} AND NOT t.is_temporary ORDER BY t.name",
                &[("db", database)],
            )
            .await;
        let rows = match with_parts {
            Ok(rows) => rows,
            // No access to system.parts: the table's own statistics, without sizes.
            Err(_) => {
                self.strings(
                    "SELECT name, engine, toString(total_rows), '' FROM system.tables WHERE database = {db:String} \
                     AND NOT is_temporary ORDER BY name",
                    &[("db", database)],
                )
                .await?
            }
        };
        Ok(rows
            .into_iter()
            .filter(|row| row.len() == 4)
            .map(|row| {
                let engine = row[1].clone();
                let sized = supports_mutations(&engine) || row[2].parse::<u64>().is_ok_and(|rows| rows > 0);
                TableEntry {
                    name: row[0].clone(),
                    rows: row[2].parse().ok().filter(|_| sized),
                    bytes: row[3].parse().ok().filter(|_| sized),
                    engine,
                }
            })
            .collect())
    }

    /// One level of the tree.
    pub async fn tree_children(&self, request: &TreeRequest) -> AppResult<TreeChildren> {
        let mut nodes: Vec<ObjectNode> = match &request.parent {
            None => self.database_names().await?.iter().map(|name| database_node(name)).collect(),
            Some(parent) => {
                let id = parent.id.as_str();
                let mut parts = id.split('/');
                let database = parts
                    .next()
                    .and_then(|part| part.strip_prefix("db:"))
                    .map(decode)
                    .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree."))?;
                let folder = parts.next().and_then(|part| part.strip_prefix("folder:")).map(str::to_owned);
                let entries = self.table_entries(&database).await?;
                match folder {
                    None => folders()
                        .into_iter()
                        .filter_map(|(folder, label, kind)| {
                            let count = entries.iter().filter(|entry| entry.kind() == kind).count();
                            (count > 0).then(|| folder_node(&database, folder, label, count))
                        })
                        .collect(),
                    Some(folder) => {
                        let Some((_, _, kind)) = folders().into_iter().find(|(id, _, _)| *id == folder) else {
                            return Err(AppError::new("This item no longer exists. Refresh the tree."));
                        };
                        entries
                            .iter()
                            .filter(|entry| entry.kind() == kind)
                            .map(|entry| object_node(&database, &folder, entry))
                            .collect()
                    }
                }
            }
        };
        if let Some(filter) = request.filter.as_deref().map(str::to_lowercase).filter(|text| !text.is_empty()) {
            nodes.retain(|node| node.label.to_lowercase().contains(&filter));
        }
        Ok(TreeChildren {
            total_hint: Some(nodes.len() as u64),
            nodes,
            next_cursor: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, engine: &str, rows: Option<u64>, bytes: Option<u64>) -> TableEntry {
        TableEntry {
            name: name.into(),
            engine: engine.into(),
            rows,
            bytes,
        }
    }

    #[test]
    fn details_show_engine_rows_and_size() {
        assert_eq!(
            table_detail(&entry("t", "MergeTree", Some(1_234_567), Some(52_428_800))),
            "MergeTree · 1.2M rows · 50 MiB"
        );
        assert_eq!(
            table_detail(&entry("t", "ReplacingMergeTree", Some(0), Some(0))),
            "ReplacingMergeTree · 0 rows · 0 B"
        );
        assert_eq!(table_detail(&entry("t", "Log", None, None)), "Log");
        assert_eq!(short_count(999), "999");
        assert_eq!(short_count(12_000), "12K");
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn nodes_have_stable_path_ids_and_edit_badges() {
        let node = object_node("my/db", "tables", &entry("events", "Log", None, None));
        assert_eq!(node.key.id, "db:my%2Fdb/folder:tables/table:events");
        assert_eq!(decode("my%2Fdb"), "my/db");
        assert_eq!(node.badges[0].text, READ_ONLY_BADGE);
        assert!(object_node("d", "tables", &entry("e", "MergeTree", None, None)).badges.is_empty());
        let view = object_node("d", "views", &entry("v", "View", None, None));
        assert_eq!(view.kind, NodeKind::VIEW);
        assert!(view.badges.is_empty() && view.detail.is_none());
        assert_eq!(database_node("system").detail.as_deref(), Some("system"));
    }
}
