//! The tree (databases → collections and views), describe from sampled documents and `listIndexes`, and the diagram's
//! graph (collections with their sampled fields; MongoDB has no foreign keys).

use futures_util::{future::join_all, stream, StreamExt, TryStreamExt};
use mongodb::{
    bson::{doc, Bson, Document},
    results::CollectionType,
    Client, Database,
};

use super::values::{self, ID_FIELD};
use crate::{
    contract::{
        schema::MAX_DIAGRAM_TABLES, BuiltinAction, ColumnInfo, IndexInfo, KeyInfo, NodeAction, NodeKey, NodeKind, ObjectDescription,
        ObjectNode, ObjectRef, SchemaColumn, SchemaGraph, SchemaTable,
    },
    error::{AppError, AppResult},
};

/// Documents sampled to infer a collection's fields.
pub const SAMPLE_SIZE: i64 = 100;
const SYSTEM_DATABASES: [&str; 3] = ["admin", "config", "local"];
/// Collections sampled at once for the diagram.
const CONCURRENCY: usize = 8;

pub fn database_node(name: &str) -> ObjectNode {
    let object = ObjectRef::new(Some(name), None, name, NodeKind::DATABASE);
    ObjectNode {
        key: NodeKey {
            id: format!("db:{name}"),
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
            NodeAction::builtin("diagram", "Diagram", BuiltinAction::Diagram),
            NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
    }
}

/// A collection or view of a database, with its estimated document count.
pub struct CollectionEntry {
    pub name: String,
    pub view: bool,
    pub count: Option<u64>,
}

pub fn collection_node(database: &str, entry: &CollectionEntry) -> ObjectNode {
    let kind = if entry.view { NodeKind::VIEW } else { NodeKind::COLLECTION };
    let object = ObjectRef::new(Some(database), None, &entry.name, kind.clone());
    let label = if entry.view { "Drop view…" } else { "Drop collection…" };
    ObjectNode {
        key: NodeKey {
            id: format!("db:{database}/{}:{}", kind.as_str(), entry.name),
            kind: kind.clone(),
            object: Some(object.clone()),
        },
        label: entry.name.clone(),
        icon: if entry.view { "view" } else { "table" }.into(),
        object: Some(object),
        detail: entry.count.map(|count| format!("~{count} docs")),
        badges: Vec::new(),
        expandable: false,
        default_view: Some("data".into()),
        actions: vec![
            NodeAction::view("design", "Design", "design"),
            NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            NodeAction::query("drop", label, r#"{ "drop": "{name}" }"#).danger().separated(),
        ],
        kind,
    }
}

pub async fn databases(client: &Client) -> AppResult<Vec<String>> {
    Ok(client.list_database_names().await?)
}

/// Collections and views in name order, each collection with its estimated count (read concurrently).
pub async fn collections(client: &Client, database: &str) -> AppResult<Vec<CollectionEntry>> {
    let db = client.database(database);
    let mut specs: Vec<_> = db.list_collections().await?.try_collect().await?;
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    let counts = join_all(specs.iter().map(|spec| {
        let collection = db.collection::<Document>(&spec.name);
        let view = matches!(spec.collection_type, CollectionType::View);
        async move {
            if view {
                None
            } else {
                collection.estimated_document_count().await.ok()
            }
        }
    }))
    .await;
    Ok(specs
        .into_iter()
        .zip(counts)
        .map(|(spec, count)| CollectionEntry {
            view: matches!(spec.collection_type, CollectionType::View),
            name: spec.name,
            count,
        })
        .collect())
}

/// Up to [`SAMPLE_SIZE`] random documents (`$sample`); views and old servers fall back to the first documents.
pub async fn sample(db: &Database, collection: &str) -> AppResult<Vec<Document>> {
    let coll = db.collection::<Document>(collection);
    match coll.aggregate([doc! { "$sample": { "size": SAMPLE_SIZE } }]).await {
        Ok(cursor) => match cursor.try_collect().await {
            Ok(documents) => return Ok(documents),
            Err(error) => tracing::debug!(%error, "sampling failed, reading the first documents"),
        },
        Err(error) => tracing::debug!(%error, "sampling failed, reading the first documents"),
    }
    Ok(coll.find(doc! {}).limit(SAMPLE_SIZE).await?.try_collect().await?)
}

/// Columns from sampled documents: top-level fields with the kind of their most frequent type, `_id` first and
/// primary, a comment saying how many sampled documents have the field.
pub fn sampled_columns(documents: &[Document]) -> Vec<ColumnInfo> {
    let mut columns = values::columns_of(documents);
    for column in &mut columns {
        let present = documents
            .iter()
            .filter(|document| document.get(&column.name).is_some_and(|value| !matches!(value, Bson::Null)))
            .count();
        column.nullable = column.name != ID_FIELD && present < documents.len();
        column.comment = Some(format!("in {present} of {} sampled documents", documents.len()));
    }
    columns
}

pub async fn describe(client: &Client, object: &ObjectRef) -> AppResult<ObjectDescription> {
    let database = object
        .database
        .as_deref()
        .ok_or_else(|| AppError::new("The collection has no database"))?;
    let db = client.database(database);
    let coll = db.collection::<Document>(&object.name);
    let mut description = ObjectDescription::new(object.clone());
    let specs: Vec<_> = db
        .list_collections()
        .filter(doc! { "name": &object.name })
        .await?
        .try_collect()
        .await?;
    let spec = specs
        .into_iter()
        .next()
        .ok_or_else(|| AppError::new(format!("{} no longer exists. Refresh the tree.", object.display_name())))?;
    let view = matches!(spec.collection_type, CollectionType::View);
    description
        .extra
        .push(("type".into(), if view { "view" } else { "collection" }.into()));
    if let Some(source) = &spec.options.view_on {
        description.extra.push(("viewOn".into(), source.clone()));
    }
    if let Some(pipeline) = &spec.options.pipeline {
        description.extra.push((
            "pipeline".into(),
            values::extended_json(&Bson::Array(pipeline.iter().cloned().map(Bson::Document).collect())),
        ));
    }
    let documents = sample(&db, &object.name).await?;
    description.columns = sampled_columns(&documents);
    if description.column(ID_FIELD).is_none() && !view {
        let mut id = ColumnInfo::result(ID_FIELD, "objectId", crate::contract::ValueKind::ObjectId);
        id.nullable = false;
        id.hints.primary = true;
        description.columns.insert(0, id);
    }
    if description.column(ID_FIELD).is_some() {
        description.primary_key = Some(KeyInfo {
            name: (!view).then(|| "_id_".to_owned()),
            columns: vec![ID_FIELD.to_owned()],
        });
    }
    if !view {
        let indexes: Vec<_> = coll.list_indexes().await?.try_collect().await?;
        let mut specs = Vec::new();
        for index in indexes {
            let options = index.options.unwrap_or_default();
            let name = options.name.clone().unwrap_or_default();
            let columns: Vec<String> = index
                .keys
                .iter()
                .map(|(field, direction)| match direction {
                    Bson::Int32(-1) | Bson::Int64(-1) => format!("{field} DESC"),
                    _ => field.clone(),
                })
                .collect();
            let method = index.keys.values().find_map(|direction| direction.as_str().map(str::to_owned));
            let primary = name == "_id_";
            let unique = primary || options.unique.unwrap_or(false);
            if unique && !primary {
                description.unique_keys.push(KeyInfo {
                    name: Some(name.clone()),
                    columns: index.keys.keys().cloned().collect(),
                });
            }
            let mut spec = doc! { "key": index.keys.clone(), "name": &name };
            if unique && !primary {
                spec.insert("unique", true);
            }
            description.indexes.push(IndexInfo {
                definition: Some(values::document_text(&spec)),
                name,
                columns,
                unique,
                primary,
                method,
            });
            specs.push(Bson::Document(spec));
        }
        description.row_estimate = coll.estimated_document_count().await.ok().map(|count| count as i64);
        let create = doc! { "createIndexes": &object.name, "indexes": specs.into_iter().filter(|spec| {
            spec.as_document().and_then(|spec| spec.get_str("name").ok()) != Some("_id_")
        }).collect::<Vec<_>>() };
        description.ddl = Some(format!(
            "// Indexes of {}\n{}",
            object.display_name(),
            serde_json::to_string_pretty(&Bson::Document(create).into_relaxed_extjson()).unwrap_or_default()
        ));
    }
    Ok(description)
}

/// Collections with their sampled fields; no relations.
pub async fn schema_graph(client: &Client, database: &str) -> AppResult<SchemaGraph> {
    let listed = collections(client, database).await?;
    let total_tables = listed.len();
    let db = client.database(database);
    let shown: Vec<CollectionEntry> = listed.into_iter().take(MAX_DIAGRAM_TABLES).collect();
    let names: Vec<String> = shown.iter().map(|entry| entry.name.clone()).collect();
    let sampled: Vec<AppResult<Vec<Document>>> = stream::iter(names)
        .map(|name| {
            let db = db.clone();
            async move { sample(&db, &name).await }
        })
        .buffered(CONCURRENCY)
        .collect()
        .await;
    let tables = shown
        .iter()
        .zip(sampled)
        .map(|(entry, documents)| SchemaTable {
            name: entry.name.clone(),
            kind: if entry.view { "view" } else { "table" }.to_owned(),
            rows: entry.count,
            comment: String::new(),
            // A collection that cannot be sampled (permissions) still shows, without fields.
            columns: documents
                .map(|documents| {
                    sampled_columns(&documents)
                        .into_iter()
                        .map(|column| SchemaColumn {
                            primary: column.hints.primary,
                            name: column.name,
                            column_type: column.type_name,
                            nullable: column.nullable,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    Ok(SchemaGraph::assemble(database, tables, total_tables, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ActionTarget;

    #[test]
    fn nodes_have_stable_ids_actions_and_details() {
        let admin = database_node("admin");
        assert_eq!(admin.key.id, "db:admin");
        assert_eq!(admin.detail.as_deref(), Some("system"));
        assert!(database_node("shop").detail.is_none());
        let node = collection_node(
            "shop",
            &CollectionEntry {
                name: "orders".into(),
                view: false,
                count: Some(12),
            },
        );
        assert_eq!(node.key.id, "db:shop/collection:orders");
        assert_eq!(node.kind, NodeKind::COLLECTION);
        assert_eq!(node.icon, "table");
        assert_eq!(node.detail.as_deref(), Some("~12 docs"));
        assert_eq!(node.default_view.as_deref(), Some("data"));
        let drop = node.actions.iter().find(|action| action.id == "drop").unwrap();
        assert!(drop.danger);
        assert_eq!(
            drop.target,
            ActionTarget::Query {
                template: r#"{ "drop": "{name}" }"#.into()
            }
        );
        let view = collection_node(
            "shop",
            &CollectionEntry {
                name: "v".into(),
                view: true,
                count: None,
            },
        );
        assert_eq!((view.kind, view.icon.as_str()), (NodeKind::VIEW, "view"));
    }

    #[test]
    fn sampled_columns_note_how_often_a_field_appears() {
        let documents = [doc! { "_id": 1, "a": 1, "b": Bson::Null }, doc! { "_id": 2, "a": 2 }];
        let columns = sampled_columns(&documents);
        let a = columns.iter().find(|c| c.name == "a").unwrap();
        assert!(!a.nullable);
        assert_eq!(a.comment.as_deref(), Some("in 2 of 2 sampled documents"));
        assert!(columns.iter().find(|c| c.name == "b").unwrap().nullable);
        assert!(!columns[0].nullable && columns[0].hints.primary);
    }
}
