//! Objects and the engine-described tree (ADR 0001): structured object references, node kinds as open strings, tree
//! nodes with their actions, and paged children.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// What a node is. Common kinds have constants; engine-specific kinds are namespaced (`"redis.stream"`,
/// `"es.componentTemplate"`, `"clickhouse.part"`). The UI picks views and actions per kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeKind(pub Cow<'static, str>);

impl NodeKind {
    pub const CONNECTION: Self = Self::of("connection");
    pub const DATABASE: Self = Self::of("database");
    pub const SCHEMA: Self = Self::of("schema");
    /// A grouping node such as "Tables" or "Functions".
    pub const FOLDER: Self = Self::of("folder");
    pub const TABLE: Self = Self::of("table");
    pub const VIEW: Self = Self::of("view");
    pub const MATERIALIZED_VIEW: Self = Self::of("materializedView");
    pub const FOREIGN_TABLE: Self = Self::of("foreignTable");
    pub const DICTIONARY: Self = Self::of("dictionary");
    pub const FUNCTION: Self = Self::of("function");
    pub const PROCEDURE: Self = Self::of("procedure");
    pub const PACKAGE: Self = Self::of("package");
    pub const SEQUENCE: Self = Self::of("sequence");
    pub const TYPE: Self = Self::of("type");
    pub const EXTENSION: Self = Self::of("extension");
    pub const INDEX: Self = Self::of("index");
    pub const TRIGGER: Self = Self::of("trigger");
    pub const SYNONYM: Self = Self::of("synonym");
    pub const COLLECTION: Self = Self::of("collection");
    pub const KEY_PREFIX: Self = Self::of("keyPrefix");
    pub const KEY: Self = Self::of("key");
    pub const SEARCH_INDEX: Self = Self::of("searchIndex");
    pub const DATA_STREAM: Self = Self::of("dataStream");
    pub const ALIAS: Self = Self::of("alias");
    pub const TEMPLATE: Self = Self::of("template");
    pub const PIPELINE: Self = Self::of("pipeline");
    pub const POLICY: Self = Self::of("policy");

    pub const fn of(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    /// A kind read from the wire or built at run time.
    pub fn named(name: impl Into<String>) -> Self {
        Self(Cow::Owned(name.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether rows (or documents) can be browsed with a `BrowseRequest`.
    pub fn is_relation(&self) -> bool {
        matches!(
            self.as_str(),
            "table" | "view" | "materializedView" | "foreignTable" | "dictionary" | "collection"
        )
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured reference to an object; never a dotted string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    /// Extra qualifiers (a package member, an index field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// `name` is base64 (non-UTF-8 Redis keys).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

impl ObjectRef {
    pub fn new(database: Option<&str>, schema: Option<&str>, name: &str, kind: NodeKind) -> Self {
        Self {
            database: database.map(str::to_owned),
            schema: schema.map(str::to_owned),
            name: name.to_owned(),
            kind,
            path: Vec::new(),
            binary: false,
        }
    }

    pub fn table(database: &str, schema: Option<&str>, name: &str) -> Self {
        Self::new(Some(database), schema, name, NodeKind::TABLE)
    }

    /// `database.schema.name` for messages and logs (never parsed back).
    pub fn display_name(&self) -> String {
        [self.database.as_deref(), self.schema.as_deref(), Some(self.name.as_str())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// A node's identity in the tree. `id` is engine-opaque and stable across calls (`"db:shop/schema:public/folder:tables"`);
/// the UI only stores and sends it back.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeKey {
    pub id: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub object: Option<ObjectRef>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    #[default]
    Neutral,
    Ok,
    Warn,
    Danger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Badge {
    pub text: String,
    #[serde(default)]
    pub tone: Tone,
}

/// Actions every engine can attach without engine code in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuiltinAction {
    Refresh,
    CopyName,
    NewQuery,
    Diagram,
    CreateDatabase,
    Import,
    Export,
    Design,
}

/// What a node action does when picked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActionTarget {
    /// Opens the registered view with this role for the node's object.
    View {
        role: String,
    },
    /// Opens a query tab pre-filled from the template (`{qualified}`, `{name}`, `{database}`, `{schema}` are filled in).
    Query {
        template: String,
    },
    /// Calls `invoke_capability` (through the gateway) with these arguments plus `object`.
    Capability {
        name: String,
        args: serde_json::Value,
    },
    Builtin {
        name: BuiltinAction,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeAction {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub target: ActionTarget,
    #[serde(default)]
    pub danger: bool,
    #[serde(default)]
    pub separator_before: bool,
}

impl NodeAction {
    pub fn new(id: &str, label: &str, target: ActionTarget) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            icon: None,
            target,
            danger: false,
            separator_before: false,
        }
    }

    pub fn view(id: &str, label: &str, role: &str) -> Self {
        Self::new(id, label, ActionTarget::View { role: role.to_owned() })
    }

    pub fn builtin(id: &str, label: &str, name: BuiltinAction) -> Self {
        Self::new(id, label, ActionTarget::Builtin { name })
    }

    pub fn query(id: &str, label: &str, template: &str) -> Self {
        Self::new(
            id,
            label,
            ActionTarget::Query {
                template: template.to_owned(),
            },
        )
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn separated(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

/// One row of the object tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectNode {
    pub key: NodeKey,
    pub label: String,
    pub kind: NodeKind,
    /// An `IconName` from `src/core/dom.ts`, or `"engine:<id>/<name>"` served by the engine's UI module.
    pub icon: String,
    #[serde(default)]
    pub object: Option<ObjectRef>,
    /// Short note shown dimmed after the label (`~12k rows`, a function signature, `system`).
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub badges: Vec<Badge>,
    pub expandable: bool,
    /// The view role opened by a double-click: `"data"`, `"design"`, `"overview"`, `"value"`, …
    #[serde(default)]
    pub default_view: Option<String>,
    #[serde(default)]
    pub actions: Vec<NodeAction>,
}

/// Children of a node. `parent: None` asks for the connection's top level (databases for SQL, `db0…` for Redis,
/// sections for Elasticsearch).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeRequest {
    #[serde(default)]
    pub parent: Option<NodeKey>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// A name filter typed in the tree's search box; engines may ignore it.
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeChildren {
    pub nodes: Vec<ObjectNode>,
    /// Pass back as `cursor` to get the next page ("Load more").
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub total_hint: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_plain_strings_on_the_wire() {
        assert_eq!(serde_json::to_string(&NodeKind::MATERIALIZED_VIEW).unwrap(), "\"materializedView\"");
        let kind: NodeKind = serde_json::from_str("\"redis.stream\"").unwrap();
        assert_eq!(kind, NodeKind::named("redis.stream"));
        assert!(NodeKind::COLLECTION.is_relation());
        assert!(!NodeKind::FUNCTION.is_relation());
    }

    #[test]
    fn object_refs_skip_empty_extras() {
        let object = ObjectRef::table("shop", None, "orders");
        let json = serde_json::to_value(&object).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "database": "shop", "schema": null, "name": "orders", "kind": "table" })
        );
        let back: ObjectRef = serde_json::from_value(serde_json::json!({ "name": "k", "kind": "key", "binary": true })).unwrap();
        assert!(back.binary);
        assert_eq!(object.display_name(), "shop.orders");
    }

    #[test]
    fn action_targets_are_tagged() {
        let action = NodeAction::query("truncate", "Truncate…", "TRUNCATE TABLE {qualified}").danger();
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["target"]["kind"], "query");
        assert_eq!(json["danger"], true);
        let builtin: ActionTarget = serde_json::from_value(serde_json::json!({ "kind": "builtin", "name": "copyName" })).unwrap();
        assert_eq!(
            builtin,
            ActionTarget::Builtin {
                name: BuiltinAction::CopyName
            }
        );
    }
}
