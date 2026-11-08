//! The object tree and key descriptions.
//!
//! Top level: one node per logical database, `db0`..`db{N-1}` (N from `CONFIG GET databases`, 16 when the ACL forbids it),
//! with the key count from `INFO keyspace`. A database's children are `SCAN` pages: the request's `cursor` is the SCAN
//! cursor, `filter` the `MATCH` pattern, and `next_cursor` the cursor to continue with. `KEYS` is never used. Keys that are
//! not valid UTF-8 get their base64 as `name` and `binary: true` on their [`ObjectRef`].

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::{
    connection::{parse_info, parse_keyspace, RedisEngine},
    scan::{KeySummary, ScanRequest},
    types::{as_string, Blob, RedisType},
};
use crate::{
    contract::{
        Badge, BuiltinAction, NodeAction, NodeKey, NodeKind, ObjectDescription, ObjectNode, ObjectRef, Tone, TreeChildren, TreeRequest,
    },
    error::{AppError, AppResult},
};

/// Keys per tree page unless the request says otherwise.
pub const PAGE: u32 = 200;
/// What the server has when `CONFIG GET databases` is not allowed.
pub const DEFAULT_DATABASES: u32 = 16;

/// `db3` for 3.
pub fn database_label(db: i64) -> String {
    format!("db{db}")
}

pub fn database_object(db: i64) -> ObjectRef {
    ObjectRef::new(Some(&db.to_string()), None, &database_label(db), NodeKind::DATABASE)
}

/// The logical database a node or object stands for (`database` is the number as text).
pub fn db_of_object(object: &ObjectRef) -> AppResult<i64> {
    object
        .database
        .as_deref()
        .and_then(|text| text.trim_start_matches("db").parse().ok())
        .filter(|db| *db >= 0)
        .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree."))
}

pub fn db_of_parent(parent: &NodeKey) -> AppResult<i64> {
    match &parent.object {
        Some(object) => db_of_object(object),
        None => parent
            .id
            .strip_prefix("db:")
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree.")),
    }
}

/// The key an object stands for (base64 in `name` when `binary`).
pub fn key_of(object: &ObjectRef) -> AppResult<Blob> {
    if object.binary {
        STANDARD
            .decode(&object.name)
            .map(Blob)
            .map_err(|_| AppError::new("The key name is not valid base64."))
    } else {
        Ok(Blob::text(object.name.clone()))
    }
}

pub fn key_object(db: i64, key: &Blob) -> ObjectRef {
    let mut object = match key.as_str() {
        Some(text) => ObjectRef::new(Some(&db.to_string()), None, text, NodeKind::KEY),
        None => {
            let mut object = ObjectRef::new(Some(&db.to_string()), None, &STANDARD.encode(&key.0), NodeKind::KEY);
            object.binary = true;
            object
        }
    };
    object.path = Vec::new();
    object
}

pub fn database_node(db: i64, keys: Option<u64>) -> ObjectNode {
    let object = database_object(db);
    ObjectNode {
        key: NodeKey {
            id: format!("db:{db}"),
            kind: NodeKind::DATABASE,
            object: Some(object.clone()),
        },
        label: database_label(db),
        kind: NodeKind::DATABASE,
        icon: "database".into(),
        object: Some(object),
        detail: keys.map(|count| if count == 1 { "1 key".to_owned() } else { format!("{count} keys") }),
        badges: Vec::new(),
        expandable: keys != Some(0),
        default_view: Some("keys".into()),
        actions: vec![
            NodeAction::view("keys", "Browse keys", "keys"),
            NodeAction::builtin("newQuery", "New console", BuiltinAction::NewQuery),
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
            NodeAction::query("flush", "Flush database…", "FLUSHDB").danger().separated(),
        ],
    }
}

/// `59s`, `4m`, `3h`, `2d`.
pub fn format_ttl(ttl_ms: i64) -> String {
    let seconds = ttl_ms / 1000;
    match seconds {
        s if s < 1 => format!("{ttl_ms}ms"),
        s if s < 120 => format!("{s}s"),
        s if s < 7200 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

pub fn key_node(db: i64, summary: &KeySummary) -> ObjectNode {
    let object = key_object(db, &summary.key);
    let mut badges = Vec::new();
    if let Some(key_type) = &summary.key_type {
        badges.push(Badge {
            text: key_type.server_name().to_owned(),
            tone: Tone::Neutral,
        });
    }
    if let Some(ttl) = summary.ttl_ms.filter(|ttl| *ttl >= 0) {
        badges.push(Badge {
            text: format!("ttl {}", format_ttl(ttl)),
            tone: Tone::Warn,
        });
    }
    ObjectNode {
        key: NodeKey {
            id: format!("db:{db}/key:{}", object.name),
            kind: NodeKind::KEY,
            object: Some(object.clone()),
        },
        label: summary.key.display(),
        kind: NodeKind::KEY,
        icon: "key".into(),
        object: Some(object),
        detail: None,
        badges,
        expandable: false,
        default_view: Some("value".into()),
        actions: vec![NodeAction::builtin("copyName", "Copy key", BuiltinAction::CopyName)],
    }
}

/// Key counts per database from `INFO keyspace` (a database with no keys is not listed by the server).
pub async fn keyspace_counts(engine: &RedisEngine) -> HashMap<i64, u64> {
    let Ok(reply) = engine.query(redis::cmd("INFO").arg("keyspace")).await else {
        return HashMap::new();
    };
    let text = as_string(reply).unwrap_or_default();
    parse_info(&text)
        .get("keyspace")
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(db, stats)| parse_keyspace(db, stats))
                .map(|entry| (entry.db, entry.keys))
                .collect()
        })
        .unwrap_or_default()
}

/// The configured number of databases.
pub async fn database_count(engine: &RedisEngine) -> u32 {
    engine
        .config_get("databases")
        .await
        .ok()
        .flatten()
        .and_then(|text| text.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_DATABASES)
}

/// `db0`..`dbN`.
pub async fn databases(engine: &RedisEngine, count: u32) -> Vec<ObjectNode> {
    let counts = keyspace_counts(engine).await;
    (0..i64::from(count))
        .map(|db| database_node(db, Some(counts.get(&db).copied().unwrap_or(0))))
        .collect()
}

/// A pattern for `MATCH` from the tree filter box: text without glob characters finds keys that contain it.
pub fn match_pattern(filter: &str) -> Option<String> {
    let filter = filter.trim();
    if filter.is_empty() {
        None
    } else if filter.contains(['*', '?', '[']) {
        Some(filter.to_owned())
    } else {
        Some(format!("*{filter}*"))
    }
}

/// A tree cursor is the `SCAN` cursor, plus `|n` when the last `SCAN` call returned more keys than fitted the page (a small
/// database comes back whole whatever `COUNT` says): the next page repeats that call and skips the `n` keys already shown.
fn split_cursor(cursor: Option<&str>) -> (String, usize) {
    match cursor.unwrap_or_default().split_once('|') {
        Some((scan, skip)) => (scan.to_owned(), skip.parse().unwrap_or(0)),
        None => (cursor.unwrap_or_default().to_owned(), 0),
    }
}

/// One `SCAN` page of a database's keys as tree nodes: at most `limit` of them.
pub async fn keys_page(engine: &RedisEngine, db: i64, request: &TreeRequest) -> AppResult<TreeChildren> {
    let limit = request.limit.unwrap_or(PAGE).clamp(1, 2000) as usize;
    let pattern = request.filter.as_deref().and_then(match_pattern);
    let (scan_cursor, skip) = split_cursor(request.cursor.as_deref());
    let page = engine
        .scan_keys(&ScanRequest {
            cursor: scan_cursor.clone(),
            pattern: pattern.clone(),
            page_size: Some(skip + limit),
            ..Default::default()
        })
        .await?;
    let collected = page.keys.len();
    let shown: Vec<KeySummary> = page.keys.into_iter().skip(skip).take(limit).collect();
    let consumed = skip + shown.len();
    let next_cursor = if collected > consumed {
        Some(format!("{scan_cursor}|{consumed}"))
    } else {
        (!page.done).then_some(page.cursor)
    };
    let keys: Vec<Blob> = shown.iter().map(|summary| summary.key.clone()).collect();
    let summaries = engine.key_summaries(keys).await?;
    let total_hint = if pattern.is_none() { engine.db_size().await.ok() } else { None };
    Ok(TreeChildren {
        nodes: summaries.iter().map(|summary| key_node(db, summary)).collect(),
        next_cursor,
        total_hint,
    })
}

fn bytes_text(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{:.1} MiB", b as f64 / 1024.0 / 1024.0),
    }
}

/// A key's facts, or a database's key count: Redis objects have no columns.
pub async fn describe(engine: &RedisEngine, object: &ObjectRef) -> AppResult<ObjectDescription> {
    let mut description = ObjectDescription::new(object.clone());
    match object.kind.as_str() {
        "key" => {
            let key = key_of(object)?;
            let info = engine.key_info(&key).await?;
            description.row_estimate = info.length.map(|length| length as i64);
            description.extra.push(("Type".into(), info.key_type.server_name().to_owned()));
            description.extra.push((
                "TTL".into(),
                match info.ttl_ms {
                    -2 => "key does not exist".to_owned(),
                    -1 => "no expiry".to_owned(),
                    ttl => format!("{} ({ttl} ms)", format_ttl(ttl)),
                },
            ));
            if let Some(encoding) = info.encoding {
                description.extra.push(("Encoding".into(), encoding));
            }
            if let Some(length) = info.length {
                let unit = if info.key_type == RedisType::String { "bytes" } else { "elements" };
                description.extra.push(("Length".into(), format!("{length} {unit}")));
            }
            if let Some(memory) = info.memory_bytes {
                description.extra.push(("Memory".into(), bytes_text(memory)));
            }
        }
        "database" => {
            let db = db_of_object(object)?;
            description.row_estimate = keyspace_counts(engine).await.get(&db).map(|count| *count as i64);
        }
        other => return Err(AppError::new(format!("Redis has no description for a {other}."))),
    }
    Ok(description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(key: Blob, key_type: RedisType, ttl_ms: i64) -> KeySummary {
        KeySummary {
            key,
            key_type: Some(key_type),
            ttl_ms: Some(ttl_ms),
        }
    }

    #[test]
    fn database_nodes_carry_the_number_and_the_key_count() {
        let node = database_node(3, Some(12));
        assert_eq!(node.label, "db3");
        assert_eq!(node.key.id, "db:3");
        assert_eq!(node.detail.as_deref(), Some("12 keys"));
        assert_eq!(node.default_view.as_deref(), Some("keys"));
        assert!(node.expandable);
        assert!(!database_node(4, Some(0)).expandable);
        assert_eq!(db_of_object(node.object.as_ref().unwrap()).unwrap(), 3);
        assert_eq!(db_of_parent(&node.key).unwrap(), 3);
        let flush = node.actions.iter().find(|action| action.id == "flush").unwrap();
        assert!(flush.danger);
    }

    #[test]
    fn key_nodes_show_type_and_ttl_and_round_trip_binary_names() {
        let node = key_node(1, &summary(Blob::text("user:1"), RedisType::Hash, 59_000));
        assert_eq!(node.label, "user:1");
        assert_eq!(node.badges[0].text, "hash");
        assert_eq!(node.badges[1].text, "ttl 59s");
        assert_eq!(node.default_view.as_deref(), Some("value"));
        let persistent = key_node(1, &summary(Blob::text("k"), RedisType::String, -1));
        assert_eq!(persistent.badges.len(), 1);
        let binary = Blob(vec![0xff, 0x00, 0x41]);
        let node = key_node(2, &summary(binary.clone(), RedisType::String, -1));
        let object = node.object.unwrap();
        assert!(object.binary);
        assert_eq!(key_of(&object).unwrap(), binary);
        assert_eq!(key_of(&key_object(0, &Blob::text("a b"))).unwrap(), Blob::text("a b"));
    }

    #[test]
    fn tree_cursors_carry_the_keys_already_shown() {
        assert_eq!(split_cursor(None), (String::new(), 0));
        assert_eq!(split_cursor(Some("17")), ("17".into(), 0));
        assert_eq!(split_cursor(Some("17|100")), ("17".into(), 100));
        assert_eq!(split_cursor(Some("0|x")), ("0".into(), 0));
    }

    #[test]
    fn filters_become_match_patterns() {
        assert_eq!(match_pattern(""), None);
        assert_eq!(match_pattern("user"), Some("*user*".into()));
        assert_eq!(match_pattern("user:*"), Some("user:*".into()));
        assert_eq!(format_ttl(900), "900ms");
        assert_eq!(format_ttl(59_000), "59s");
        assert_eq!(format_ttl(600_000), "10m");
        assert_eq!(format_ttl(10_000_000), "2h");
        assert_eq!(format_ttl(900_000_000), "10d");
    }
}
