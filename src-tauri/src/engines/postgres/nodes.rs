//! The tree: databases of the server, then schemas, folders and objects of a database (from `catalog::tree`), with
//! the object actions of each kind. Node ids are `database:<name>/schema:<name>/folder:<label>/<kind>:<name>` paths, so
//! they stay stable and say which database's connection answers a request.

use crate::{
    contract::{BuiltinAction, NodeAction, NodeKey, NodeKind, ObjectNode, ObjectRef, TreeChildren},
    engines::common::{adapt::catalog_children, CatalogNode, NodeKind as CoreKind},
    error::{AppError, AppResult},
};

const DATABASE_PREFIX: &str = "database:";

/// The view role the definition view of functions, sequences, types and extensions is registered under.
pub const DEFINITION_VIEW: &str = "definition";

fn encode(name: &str) -> String {
    name.replace('/', "%2F")
}

/// The database a node id belongs to (`database:shop/schema:public/…` is `shop`).
pub fn database_of(key: &NodeKey) -> AppResult<String> {
    let rest = key
        .id
        .strip_prefix(DATABASE_PREFIX)
        .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree."))?;
    let segment = rest.split('/').next().unwrap_or_default();
    Ok(segment.replace("%2F", "/"))
}

pub fn database_node(name: &str, system: bool) -> ObjectNode {
    let object = ObjectRef::new(Some(name), None, name, NodeKind::DATABASE);
    ObjectNode {
        key: NodeKey {
            id: format!("{DATABASE_PREFIX}{}", encode(name)),
            kind: NodeKind::DATABASE,
            object: Some(object.clone()),
        },
        label: name.to_owned(),
        kind: NodeKind::DATABASE,
        icon: "database".into(),
        object: Some(object),
        detail: system.then(|| "system".to_owned()),
        badges: Vec::new(),
        expandable: true,
        default_view: None,
        actions: vec![
            NodeAction::builtin("newQuery", "New query", BuiltinAction::NewQuery),
            NodeAction::builtin("diagram", "ER diagram", BuiltinAction::Diagram),
            NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
    }
}

/// Folders without objects are left out, so a schema shows only what it has.
pub fn prune(node: &mut CatalogNode) {
    for child in &mut node.children {
        prune(child);
    }
    node.children
        .retain(|child| child.kind != CoreKind::Folder || !child.children.is_empty());
}

/// Object actions per kind: query templates go through the gateway like anything typed in a query tab.
fn actions(node: &CatalogNode) -> Vec<NodeAction> {
    let copy = NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName);
    match node.kind {
        CoreKind::Schema => vec![
            NodeAction::builtin("newQuery", "New query", BuiltinAction::NewQuery),
            copy,
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
        CoreKind::Table | CoreKind::ForeignTable => {
            let mut list = vec![
                NodeAction::view("design", "Design", "design"),
                copy,
                NodeAction::query("explain", "Explain a select", "EXPLAIN SELECT * FROM {qualified}").separated(),
            ];
            if node.kind == CoreKind::Table {
                list.push(NodeAction::query("analyze", "Analyze", "ANALYZE {qualified}"));
                list.push(
                    NodeAction::query("truncate", "Truncate…", "TRUNCATE TABLE {qualified}")
                        .danger()
                        .separated(),
                );
            }
            list
        }
        CoreKind::View => vec![
            NodeAction::view("design", "Design", "design"),
            copy,
            NodeAction::query("explain", "Explain a select", "EXPLAIN SELECT * FROM {qualified}").separated(),
        ],
        CoreKind::MaterializedView => vec![
            NodeAction::view("design", "Design", "design"),
            copy,
            NodeAction::query("refresh", "Refresh data", "REFRESH MATERIALIZED VIEW {qualified}").separated(),
        ],
        CoreKind::Sequence => vec![
            copy,
            NodeAction::query("state", "Show state", "SELECT last_value, is_called FROM {qualified}").separated(),
        ],
        CoreKind::Extension => vec![
            copy,
            NodeAction::query("drop", "Drop extension…", "DROP EXTENSION {name}")
                .danger()
                .separated(),
        ],
        CoreKind::Function | CoreKind::Procedure | CoreKind::Type => vec![copy],
        _ => Vec::new(),
    }
}

/// The node icon: engine icons for the kinds the shared set has no picture for.
fn icon_of(node: &CatalogNode) -> Option<&'static str> {
    match (node.kind, node.detail.as_deref()) {
        (CoreKind::Sequence, _) => Some("engine:postgres/sequence"),
        (CoreKind::Extension, _) => Some("engine:postgres/extension"),
        (CoreKind::Type, Some("enum")) => Some("engine:postgres/enum"),
        (CoreKind::Type, _) => Some("engine:postgres/type"),
        _ => None,
    }
}

/// Children of a node inside a database; `catalog` is that database's tree (root: the database node itself).
pub fn children(catalog: &CatalogNode, parent: &NodeKey, database: &str, filter: Option<&str>) -> AppResult<TreeChildren> {
    // The catalog's root is the database node, whose path segment is `database:<name>`.
    let mut found = catalog_children(
        std::slice::from_ref(catalog),
        Some(&NodeKey {
            id: parent.id.clone(),
            kind: parent.kind.clone(),
            object: parent.object.clone(),
        }),
        Some(database),
        &actions,
    )?;
    // Icons and the definition view of the kinds without data.
    let lookup = |id: &str| -> Option<&CatalogNode> {
        let mut level = std::slice::from_ref(catalog);
        let mut found = None;
        for part in id.split('/').filter(|part| !part.is_empty()) {
            let node = level.iter().find(|node| {
                let kind: NodeKind = node.kind.into();
                format!("{}:{}", kind.as_str(), node.name.replace('/', "%2F")) == part
            })?;
            level = &node.children;
            found = Some(node);
        }
        found
    };
    for node in &mut found.nodes {
        if let Some(source) = lookup(&node.key.id) {
            if let Some(icon) = icon_of(source) {
                node.icon = icon.to_owned();
            }
            if matches!(
                source.kind,
                CoreKind::Function | CoreKind::Procedure | CoreKind::Sequence | CoreKind::Type | CoreKind::Extension
            ) {
                node.default_view = Some(DEFINITION_VIEW.to_owned());
            }
        }
    }
    if let Some(text) = filter.map(str::to_lowercase).filter(|text| !text.is_empty()) {
        found.nodes.retain(|node| node.label.to_lowercase().contains(&text));
    }
    found.total_hint = Some(found.nodes.len() as u64);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::common::ObjectRef as CoreRef;

    fn catalog() -> CatalogNode {
        let table = CatalogNode::object(CoreRef::new(Some("shop"), Some("public"), "orders", CoreKind::Table), None);
        let status = CatalogNode::object(
            CoreRef::new(Some("shop"), Some("public"), "order_status", CoreKind::Type),
            Some("enum".into()),
        );
        let mut schema = CatalogNode::container(
            CoreKind::Schema,
            "public",
            vec![
                CatalogNode::container(CoreKind::Folder, "Tables", vec![table]),
                CatalogNode::container(CoreKind::Folder, "Views", Vec::new()),
                CatalogNode::container(CoreKind::Folder, "Types", vec![status]),
            ],
        );
        schema.object = Some(CoreRef::new(Some("shop"), Some("public"), "public", CoreKind::Schema));
        let mut root = CatalogNode::container(CoreKind::Database, "shop", vec![schema]);
        root.object = Some(CoreRef::new(Some("shop"), None, "shop", CoreKind::Database));
        prune(&mut root);
        root
    }

    #[test]
    fn databases_are_roots_and_ids_name_their_database() {
        let node = database_node("my/db", false);
        assert_eq!(node.key.id, "database:my%2Fdb");
        assert_eq!(database_of(&node.key).unwrap(), "my/db");
        assert!(database_node("template1", true).detail.is_some());
        assert!(database_of(&NodeKey {
            id: "schema:public".into(),
            kind: NodeKind::SCHEMA,
            object: None
        })
        .is_err());
    }

    #[test]
    fn empty_folders_are_pruned_and_kinds_get_actions_and_icons() {
        let catalog = catalog();
        let key = database_node("shop", false).key;
        let schemas = children(&catalog, &key, "shop", None).unwrap();
        assert_eq!(schemas.nodes.len(), 1);
        assert_eq!(schemas.nodes[0].key.id, "database:shop/schema:public");
        let folders = children(&catalog, &schemas.nodes[0].key, "shop", None).unwrap();
        let labels: Vec<_> = folders.nodes.iter().map(|node| node.label.as_str()).collect();
        assert_eq!(labels, ["Tables", "Types"]);
        let tables = children(&catalog, &folders.nodes[0].key, "shop", None).unwrap();
        let orders = &tables.nodes[0];
        assert_eq!(orders.default_view.as_deref(), Some("data"));
        assert!(orders.actions.iter().any(|action| action.id == "truncate" && action.danger));
        assert_eq!(orders.object.as_ref().unwrap().schema.as_deref(), Some("public"));
        assert_eq!(orders.object.as_ref().unwrap().database.as_deref(), Some("shop"));
        let types = children(&catalog, &folders.nodes[1].key, "shop", None).unwrap();
        assert_eq!(types.nodes[0].icon, "engine:postgres/enum");
        assert_eq!(types.nodes[0].default_view.as_deref(), Some(DEFINITION_VIEW));
        let filtered = children(&catalog, &schemas.nodes[0].key, "shop", Some("TAB")).unwrap();
        assert_eq!(filtered.nodes.len(), 1);
    }
}
