//! What the assistant reads about a connection, through the engine contract only (`Handle::call`, contained): database
//! names and the objects of a database from `children`, descriptions from `describe`, and the schema overview from
//! `schema_graph` (or per-object descriptions when the engine has no schema graph). Every engine gets these read-only
//! metadata tools without engine code here. Key-value engines (Redis) list at most [`MAX_KEYS`] keys per database, so a
//! question never scans a large keyspace; the listing says so.

use crate::{
    contract::{
        CallContext, Capability, EngineFamily, NodeKey, NodeKind, ObjectDescription, ObjectNode, ObjectRef, Relation, SchemaColumn,
        SchemaGraph, SchemaTable, TreeRequest,
    },
    error::{AppError, AppResult},
    runtime::Handle,
};

/// Children asked for per call, and the most pages followed under one node.
const WALK_PAGE: u32 = 500;
const WALK_MAX_PAGES: usize = 40;
/// Containers looked inside below the database node (schemas, folders such as "Tables").
const MAX_DEPTH: usize = 3;
/// Objects collected per database; a huge catalog is cut here.
const MAX_OBJECTS: usize = 5000;
/// Keys collected per database of a key-value engine (every key is an object there).
pub const MAX_KEYS: usize = 200;
/// Objects described one by one when the engine has no schema graph.
const MAX_DESCRIBED: usize = 100;

/// Up to `max` children of `parent`, following `next_cursor` (bounded). `filter` is the tree's name filter, which an
/// engine may ignore.
async fn children(
    handle: &Handle,
    ctx: &CallContext,
    parent: Option<NodeKey>,
    max: usize,
    filter: Option<&str>,
) -> AppResult<Vec<ObjectNode>> {
    let mut nodes = Vec::new();
    let mut cursor = None;
    for _ in 0..WALK_MAX_PAGES {
        let room = max.saturating_sub(nodes.len()).max(1);
        let request = TreeRequest {
            parent: parent.clone(),
            cursor: cursor.take(),
            limit: Some(WALK_PAGE.min(u32::try_from(room).unwrap_or(WALK_PAGE))),
            filter: filter.map(str::to_owned),
        };
        let ctx = ctx.clone();
        let page = handle
            .call("listed objects", |conn| async move { conn.children(&request, &ctx).await })
            .await?;
        nodes.extend(page.nodes);
        match page.next_cursor {
            Some(next) if nodes.len() < max => cursor = Some(next),
            _ => break,
        }
    }
    nodes.truncate(max);
    Ok(nodes)
}

/// The name a database node stands for.
fn database_name(node: &ObjectNode) -> String {
    node.object
        .as_ref()
        .and_then(|object| object.database.clone().or_else(|| Some(object.name.clone())))
        .unwrap_or_else(|| node.label.clone())
}

/// The database nodes at the top of the tree.
async fn database_nodes(handle: &Handle, ctx: &CallContext) -> AppResult<Vec<ObjectNode>> {
    Ok(children(handle, ctx, None, MAX_OBJECTS, None)
        .await?
        .into_iter()
        .filter(|node| node.kind == NodeKind::DATABASE)
        .collect())
}

pub async fn databases(handle: &Handle, ctx: &CallContext) -> AppResult<Vec<String>> {
    Ok(database_nodes(handle, ctx).await?.iter().map(database_name).collect())
}

/// What the assistant lists as the objects of a database: tables, views and collections, search indexes and keys.
fn is_listed(kind: &NodeKind) -> bool {
    kind.is_relation() || *kind == NodeKind::SEARCH_INDEX || *kind == NodeKind::KEY
}

/// The objects of one database, and a note for the model when the listing was cut.
pub struct Listing {
    pub objects: Vec<ObjectNode>,
    pub note: Option<String>,
}

/// The objects of `database`: the database node's children, looking inside schema and folder nodes on the way, in the
/// engine's order; at most [`MAX_KEYS`] for a key-value engine and [`MAX_OBJECTS`] for the others. An engine without
/// databases (Elasticsearch: indices, data streams and aliases in top-level folders) ignores `database`.
pub async fn objects(handle: &Handle, ctx: &CallContext, database: &str) -> AppResult<Listing> {
    let key_value = handle.engine.descriptor().family == EngineFamily::KeyValue;
    let max = if key_value { MAX_KEYS } else { MAX_OBJECTS };
    let found = walk(handle, ctx, database, max, None).await?;
    let note = (found.len() >= max).then(|| {
        if key_value {
            format!("only the first {max} keys were listed; the database may hold many more. Ask for a key pattern instead of listing")
        } else {
            format!("only the first {max} objects were listed")
        }
    });
    Ok(Listing { objects: found, note })
}

/// The reference of the object called `name` in `database`. A cut listing (a Redis database with more keys than
/// [`MAX_KEYS`]: `SCAN` returns them in an order that changes with every server start) does not settle it, so the walk is
/// repeated with `name` as the tree filter, which a key-value engine turns into a key pattern; an engine that ignores the
/// filter answers the same as before.
pub async fn find(handle: &Handle, ctx: &CallContext, database: &str, name: &str) -> AppResult<ObjectRef> {
    let listing = objects(handle, ctx, database).await?;
    match find_object(&listing.objects, database, name) {
        Err(error) if listing.note.is_some() => {
            let matching = walk(handle, ctx, database, MAX_KEYS, Some(name)).await?;
            find_object(&matching, database, name).map_err(|_| error)
        }
        outcome => outcome,
    }
}

/// At most `max` listed objects of `database`, those matching `filter` when the engine applies it.
async fn walk(handle: &Handle, ctx: &CallContext, database: &str, max: usize, filter: Option<&str>) -> AppResult<Vec<ObjectNode>> {
    let roots = children(handle, ctx, None, MAX_OBJECTS, None).await?;
    let mut found = Vec::new();
    if roots.iter().any(|node| node.kind == NodeKind::DATABASE) {
        if database.is_empty() {
            return Err(AppError::new("No database selected; pass `database`."));
        }
        let root = roots
            .into_iter()
            .find(|node| node.kind == NodeKind::DATABASE && database_name(node) == database)
            .ok_or_else(|| AppError::new(format!("There is no database {database} on this connection")))?;
        collect(handle, ctx, root.key, 0, max, filter, &mut found).await?;
    } else {
        visit(handle, ctx, roots, 0, max, filter, &mut found).await?;
    }
    Ok(found)
}

/// Adds the listed objects under `parent` to `found`, visiting containers where they appear.
fn collect<'a>(
    handle: &'a Handle,
    ctx: &'a CallContext,
    parent: NodeKey,
    depth: usize,
    max: usize,
    filter: Option<&'a str>,
    found: &'a mut Vec<ObjectNode>,
) -> Walk<'a> {
    Box::pin(async move {
        let room = max.saturating_sub(found.len()).max(1);
        let nodes = children(handle, ctx, Some(parent), room, filter).await?;
        visit(handle, ctx, nodes, depth, max, filter, found).await
    })
}

type Walk<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>>;

/// Adds the listed objects among `nodes` to `found` and looks inside schema and folder nodes (boxed: it recurses).
fn visit<'a>(
    handle: &'a Handle,
    ctx: &'a CallContext,
    nodes: Vec<ObjectNode>,
    depth: usize,
    max: usize,
    filter: Option<&'a str>,
    found: &'a mut Vec<ObjectNode>,
) -> Walk<'a> {
    Box::pin(async move {
        for node in nodes {
            if found.len() >= max {
                break;
            }
            if is_listed(&node.kind) {
                found.push(node);
            } else if node.expandable && depth < MAX_DEPTH && matches!(node.kind.as_str(), "schema" | "folder") {
                collect(handle, ctx, node.key, depth + 1, max, filter, found).await?;
            }
        }
        Ok(())
    })
}

/// The reference of the object called `name` (`table`, or `schema.table`), matched exactly first, then ignoring case.
pub fn find_object(nodes: &[ObjectNode], database: &str, name: &str) -> AppResult<ObjectRef> {
    let names = |node: &ObjectNode| -> Vec<String> {
        let mut names = vec![node.label.clone()];
        if let Some(object) = &node.object {
            names.push(object.name.clone());
            if let Some(schema) = &object.schema {
                names.push(format!("{schema}.{}", object.name));
            }
        }
        names
    };
    let exact = nodes.iter().find(|node| names(node).iter().any(|candidate| candidate == name));
    let loose = || {
        nodes
            .iter()
            .find(|node| names(node).iter().any(|candidate| candidate.eq_ignore_ascii_case(name)))
    };
    exact
        .or_else(loose)
        .and_then(|node| node.object.clone())
        .ok_or_else(|| AppError::new(format!("There is no table, view or collection {name} in {database}")))
}

pub async fn describe(handle: &Handle, ctx: &CallContext, object: ObjectRef) -> AppResult<ObjectDescription> {
    let ctx = ctx.clone();
    handle
        .call("described an object", |conn| async move { conn.describe(&object, &ctx).await })
        .await
}

/// Tables, columns and foreign keys of `database`: the engine's schema graph, or one description per object (capped).
pub async fn schema_overview(handle: &Handle, ctx: &CallContext, database: &str) -> AppResult<SchemaGraph> {
    if handle.conn.capabilities().contains(Capability::SchemaGraph) {
        let (ctx, name) = (ctx.clone(), database.to_owned());
        return handle
            .call("read the schema", |conn| async move {
                match conn.schema_graph() {
                    Some(source) => source.schema_graph(&name, &ctx).await,
                    None => Err(crate::contract::unsupported_capability("schema diagrams")),
                }
            })
            .await;
    }
    let nodes: Vec<ObjectNode> = objects(handle, ctx, database)
        .await?
        .objects
        .into_iter()
        .filter(|node| node.kind.is_relation())
        .collect();
    let mut tables = Vec::new();
    let mut relations = Vec::new();
    for node in nodes.iter().take(MAX_DESCRIBED) {
        let Some(object) = node.object.clone() else { continue };
        let description = describe(handle, ctx, object).await?;
        relations.extend(relations_of(database, &description));
        tables.push(table_of(&description));
    }
    Ok(SchemaGraph::assemble(database, tables, nodes.len(), relations))
}

fn table_of(description: &ObjectDescription) -> SchemaTable {
    let primary: Vec<&str> = description
        .primary_key
        .iter()
        .flat_map(|key| key.columns.iter().map(String::as_str))
        .collect();
    SchemaTable {
        name: description.object.name.clone(),
        kind: if description.object.kind == NodeKind::VIEW {
            "view"
        } else {
            "table"
        }
        .to_owned(),
        rows: description.row_estimate.and_then(|rows| u64::try_from(rows).ok()),
        comment: description.comment.clone().unwrap_or_default(),
        columns: description
            .columns
            .iter()
            .map(|column| SchemaColumn {
                name: column.name.clone(),
                column_type: column.type_name.clone(),
                nullable: column.nullable,
                primary: column.hints.primary || primary.contains(&column.name.as_str()),
            })
            .collect(),
    }
}

fn relations_of(database: &str, description: &ObjectDescription) -> Vec<Relation> {
    description
        .foreign_keys
        .iter()
        .map(|key| Relation {
            constraint: key
                .name
                .clone()
                .unwrap_or_else(|| format!("{}({})", description.object.name, key.columns.join(","))),
            database: database.to_owned(),
            table: description.object.name.clone(),
            columns: key.columns.clone(),
            referenced_database: key.ref_database.clone().unwrap_or_default(),
            referenced_table: key.ref_table.clone(),
            referenced_columns: key.ref_columns.clone(),
        })
        .collect()
}
