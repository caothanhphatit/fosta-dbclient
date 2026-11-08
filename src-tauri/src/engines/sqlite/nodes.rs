//! The tree: the attached databases (`main`, `temp`, `ATTACH`ed files) with folders of tables, views, indexes and triggers,
//! and the object actions of each kind. SQLite calls an attached database a "schema"; the contract calls it the
//! `database` of an object, so references are translated at this boundary.

use crate::{
    contract::{self, BuiltinAction, NodeAction, NodeKey, TreeChildren},
    engines::common::{adapt::catalog_children, CatalogNode, NodeKind as CoreKind, ObjectRef as CoreRef},
    error::AppResult,
};

/// The view role the definition view of indexes and triggers is registered under.
pub const DEFINITION_VIEW: &str = "definition";

/// The core's reference (attached database in `schema`) as a contract reference (`database`).
pub fn to_contract(object: &CoreRef) -> contract::ObjectRef {
    contract::ObjectRef::new(object.schema.as_deref(), None, &object.name, object.kind.into())
}

/// The core reference of a contract reference: the database is the attached database's name (`main` when absent).
pub fn to_core(object: &contract::ObjectRef) -> AppResult<CoreRef> {
    let kind = (&object.kind).try_into()?;
    Ok(CoreRef::new(
        None,
        Some(object.database.as_deref().or(object.schema.as_deref()).unwrap_or("main")),
        &object.name,
        kind,
    ))
}

/// Folders without objects are left out.
pub fn prune(nodes: &mut [CatalogNode]) {
    for node in nodes.iter_mut() {
        prune(&mut node.children);
        node.children
            .retain(|child| child.kind != CoreKind::Folder || !child.children.is_empty());
    }
}

/// The catalog with every object reference moved to the contract's `database` field, so the references the tree hands
/// out are the ones `describe` and `browse` take back.
fn remap(nodes: &mut [CatalogNode]) {
    for node in nodes.iter_mut() {
        if let Some(object) = &mut node.object {
            object.database = object.schema.take();
        }
        remap(&mut node.children);
    }
}

fn actions(node: &CatalogNode) -> Vec<NodeAction> {
    let copy = NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName);
    match node.kind {
        CoreKind::Database => vec![
            NodeAction::builtin("newQuery", "New query", BuiltinAction::NewQuery),
            NodeAction::builtin("diagram", "ER diagram", BuiltinAction::Diagram),
            copy,
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
        CoreKind::Table => vec![
            NodeAction::view("design", "Design", "design"),
            copy,
            NodeAction::query("explain", "Explain a select", "EXPLAIN QUERY PLAN SELECT * FROM {qualified}").separated(),
            NodeAction::query("analyze", "Analyze", "ANALYZE {qualified}"),
            NodeAction::query("clear", "Delete all rows…", "DELETE FROM {qualified}")
                .danger()
                .separated(),
        ],
        CoreKind::View => vec![
            NodeAction::view("design", "Design", "design"),
            copy,
            NodeAction::query("explain", "Explain a select", "EXPLAIN QUERY PLAN SELECT * FROM {qualified}").separated(),
        ],
        CoreKind::Index => vec![copy, NodeAction::query("reindex", "Reindex", "REINDEX {qualified}").separated()],
        CoreKind::Trigger => vec![
            copy,
            NodeAction::query("drop", "Drop trigger…", "DROP TRIGGER {qualified}")
                .danger()
                .separated(),
        ],
        _ => Vec::new(),
    }
}

/// Children of `parent` (the databases when `None`); `catalog` is the core's tree.
pub fn children(catalog: &[CatalogNode], parent: Option<&NodeKey>, filter: Option<&str>) -> AppResult<TreeChildren> {
    let mut roots = catalog.to_vec();
    prune(&mut roots);
    remap(&mut roots);
    let mut found = catalog_children(&roots, parent, None, &actions)?;
    // Indexes and triggers have no rows: they open their definition.
    for node in &mut found.nodes {
        match node.kind.as_str() {
            "index" | "trigger" => node.default_view = Some(DEFINITION_VIEW.to_owned()),
            _ => {}
        }
        if node.kind.as_str() == "trigger" {
            node.icon = "engine:sqlite/trigger".to_owned();
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

    fn catalog() -> Vec<CatalogNode> {
        let table = CatalogNode::object(CoreRef::new(None, Some("main"), "orders", CoreKind::Table), None);
        let index = CatalogNode::object(
            CoreRef::new(None, Some("main"), "orders_idx", CoreKind::Index),
            Some("on orders".into()),
        );
        let mut main = CatalogNode::container(
            CoreKind::Database,
            "main",
            vec![
                CatalogNode::container(CoreKind::Folder, "Tables", vec![table]),
                CatalogNode::container(CoreKind::Folder, "Views", Vec::new()),
                CatalogNode::container(CoreKind::Folder, "Indexes", vec![index]),
            ],
        );
        main.object = Some(CoreRef::new(None, Some("main"), "main", CoreKind::Database));
        vec![main]
    }

    #[test]
    fn databases_hold_pruned_folders_and_references_use_the_database_field() {
        let catalog = catalog();
        let top = children(&catalog, None, None).unwrap();
        assert_eq!(top.nodes[0].key.id, "database:main");
        assert_eq!(top.nodes[0].object.as_ref().unwrap().database.as_deref(), Some("main"));
        let folders = children(&catalog, Some(&top.nodes[0].key), None).unwrap();
        let labels: Vec<_> = folders.nodes.iter().map(|node| node.label.as_str()).collect();
        assert_eq!(labels, ["Tables", "Indexes"]);
        let tables = children(&catalog, Some(&folders.nodes[0].key), None).unwrap();
        let orders = &tables.nodes[0];
        assert_eq!(orders.default_view.as_deref(), Some("data"));
        let object = orders.object.as_ref().unwrap();
        assert_eq!((object.database.as_deref(), object.schema.as_deref()), (Some("main"), None));
        assert!(orders.actions.iter().any(|action| action.id == "clear" && action.danger));
        assert_eq!(
            to_core(object).unwrap(),
            CoreRef::new(None, Some("main"), "orders", CoreKind::Table)
        );
        let indexes = children(&catalog, Some(&folders.nodes[1].key), None).unwrap();
        assert_eq!(indexes.nodes[0].default_view.as_deref(), Some(DEFINITION_VIEW));
        let filtered = children(&catalog, Some(&top.nodes[0].key), Some("IND")).unwrap();
        assert_eq!(filtered.nodes.len(), 1);
    }

    #[test]
    fn a_reference_without_a_database_means_main() {
        let object = contract::ObjectRef::new(None, None, "t", contract::NodeKind::TABLE);
        assert_eq!(to_core(&object).unwrap().schema.as_deref(), Some("main"));
        assert!(to_core(&contract::ObjectRef::new(None, None, "k", contract::NodeKind::KEY)).is_err());
    }
}
