//! The structural build of a profile, through the engine contract only (`Handle::call_cancellable`): the objects of a
//! database from `children` (looking inside schemas and folders, leaving system and credential objects out), one
//! `describe` per object up to [`MAX_DETAILED_OBJECTS`], declared and name-inferred relations, and the cheap structure
//! checks (listing, and the schema graph where the engine has one). Every engine gets a profile without engine code
//! here; nothing reads rows.

use std::{collections::HashSet, future::Future, pin::Pin, time::Duration};

use serde::Serialize;

use super::{
    fingerprint::{self, ColumnShape},
    infer::infer_relations,
    model::{
        qualified, ColumnProfile, DatabaseProfile, IndexProfile, ObjectProfile, RelationProfile, RelationSource, RoutineProfile,
        StructureChecks, PROFILE_VERSION,
    },
    redact, MAX_DETAILED_OBJECTS,
};
use crate::{
    audit::now_millis,
    contract::{
        cancelled, CallContext, CancellationToken, Capability, EngineFamily, NodeKey, NodeKind, ObjectDescription, ObjectNode, TreeRequest,
        CANCELLED,
    },
    error::{AppError, AppResult},
    runtime::Handle,
};

/// Children asked for per call, and the most pages followed under one node.
const WALK_PAGE: u32 = 500;
const WALK_MAX_PAGES: usize = 40;
/// Containers looked inside below the database node (schemas, folders such as "Tables").
const MAX_DEPTH: usize = 3;
/// Objects listed per database; past this the listing is cut and the profile says so.
pub const MAX_LISTED_OBJECTS: usize = 20_000;
/// Routines (functions, procedures) kept per database.
const MAX_ROUTINES: usize = 2_000;
/// Engine facts kept per object.
const MAX_EXTRA: usize = 20;
/// How long a cancelled engine call may take to stop before its task is aborted.
const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// Where a build is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    Listing,
    Describing,
    Relations,
    /// Reading value hints (only where the assistant may read data and the caller asked for them).
    Values,
    Saving,
    /// The model writes the database summary and the object descriptions (`profile_summarize`, a run of its own).
    Summary,
}

/// One progress report: `done` of `total` in `step` (0 of 0 while the total is unknown), and the object being read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub step: Step,
    pub done: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

impl Progress {
    pub fn new(step: Step, done: usize, total: usize) -> Self {
        Self {
            step,
            done,
            total,
            object: None,
        }
    }
}

pub type ProgressFn<'a> = &'a (dyn Fn(Progress) + Send + Sync);

/// What the walk found below a database.
#[derive(Default)]
struct Listed {
    objects: Vec<ObjectNode>,
    routines: Vec<ObjectNode>,
    excluded: usize,
    truncated: bool,
}

/// What a profile learns about: tables, views, collections and search indexes.
fn is_learned(kind: &NodeKind) -> bool {
    kind.is_relation() || *kind == NodeKind::SEARCH_INDEX
}

fn is_routine(kind: &NodeKind) -> bool {
    *kind == NodeKind::FUNCTION || *kind == NodeKind::PROCEDURE
}

fn cancelled_error(error: &AppError, cancel: &CancellationToken) -> bool {
    cancel.is_cancelled() || error.code() == Some(CANCELLED)
}

/// Up to `max` children of `parent`, following `next_cursor` (bounded); stops when `cancel` fires.
async fn children(handle: &Handle, ctx: &CallContext, parent: Option<NodeKey>, max: usize) -> AppResult<Vec<ObjectNode>> {
    let mut nodes = Vec::new();
    let mut cursor = None;
    for _ in 0..WALK_MAX_PAGES {
        if ctx.cancel.is_cancelled() {
            return Err(cancelled());
        }
        let room = max.saturating_sub(nodes.len()).max(1);
        let request = TreeRequest {
            parent: parent.clone(),
            cursor: cursor.take(),
            limit: Some(WALK_PAGE.min(u32::try_from(room).unwrap_or(WALK_PAGE))),
            filter: None,
        };
        let call_ctx = ctx.clone();
        let page = handle
            .call_cancellable("listed objects", ctx.cancel.clone(), CANCEL_GRACE, |conn| async move {
                conn.children(&request, &call_ctx).await
            })
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

/// The objects and routines of `database`. An engine without databases (Elasticsearch) ignores `database`. A system
/// database is refused.
async fn list(handle: &Handle, ctx: &CallContext, database: &str) -> AppResult<Listed> {
    let roots = children(handle, ctx, None, MAX_LISTED_OBJECTS).await?;
    let mut listed = Listed::default();
    if roots.iter().any(|node| node.kind == NodeKind::DATABASE) {
        if database.is_empty() {
            return Err(AppError::new("Choose a database to learn"));
        }
        let root = roots
            .into_iter()
            .find(|node| node.kind == NodeKind::DATABASE && database_name(node) == database)
            .ok_or_else(|| AppError::new(format!("There is no database {database} on this connection")))?;
        if redact::is_system_database(&root) {
            return Err(AppError::new(format!(
                "{database} is a system database; the assistant does not learn system databases"
            )));
        }
        collect(handle, ctx, root.key, 0, &mut listed).await?;
    } else {
        visit(handle, ctx, roots, 0, &mut listed).await?;
    }
    Ok(listed)
}

type Walk<'a> = Pin<Box<dyn Future<Output = AppResult<()>> + Send + 'a>>;

fn collect<'a>(handle: &'a Handle, ctx: &'a CallContext, parent: NodeKey, depth: usize, listed: &'a mut Listed) -> Walk<'a> {
    Box::pin(async move {
        let room = MAX_LISTED_OBJECTS.saturating_sub(listed.objects.len()).max(1);
        let nodes = children(handle, ctx, Some(parent), room).await?;
        visit(handle, ctx, nodes, depth, listed).await
    })
}

fn visit<'a>(handle: &'a Handle, ctx: &'a CallContext, nodes: Vec<ObjectNode>, depth: usize, listed: &'a mut Listed) -> Walk<'a> {
    Box::pin(async move {
        for node in nodes {
            if listed.objects.len() >= MAX_LISTED_OBJECTS {
                listed.truncated = true;
                break;
            }
            if is_learned(&node.kind) {
                if redact::is_excluded_object(&node) {
                    listed.excluded += 1;
                } else {
                    listed.objects.push(node);
                }
            } else if is_routine(&node.kind) {
                if !redact::is_excluded_object(&node) && listed.routines.len() < MAX_ROUTINES {
                    listed.routines.push(node);
                }
            } else if node.expandable && depth < MAX_DEPTH && matches!(node.kind.as_str(), "schema" | "folder") {
                if redact::is_excluded_container(&node) {
                    listed.excluded += 1;
                } else {
                    collect(handle, ctx, node.key, depth + 1, listed).await?;
                }
            }
        }
        Ok(())
    })
}

fn node_schema(node: &ObjectNode) -> Option<String> {
    node.object.as_ref().and_then(|object| object.schema.clone())
}

fn node_name(node: &ObjectNode) -> String {
    node.object
        .as_ref()
        .map_or_else(|| node.label.clone(), |object| object.name.clone())
}

fn listing_fingerprint(objects: &[ObjectNode]) -> String {
    fingerprint::listing_fingerprint(
        objects
            .iter()
            .map(|node| (qualified(node_schema(node).as_deref(), &node_name(node)), node.kind.as_str())),
    )
}

/// An object known by name and kind only (past the detail cap, or its description failed).
fn undetailed(node: &ObjectNode) -> ObjectProfile {
    let (schema, name) = (node_schema(node), node_name(node));
    ObjectProfile {
        fingerprint: fingerprint::object_fingerprint(node.kind.as_str(), &qualified(schema.as_deref(), &name), []),
        schema,
        name,
        kind: node.kind.clone(),
        detailed: false,
        comment: None,
        columns: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
        indexes: Vec::new(),
        row_estimate: None,
        extra: Vec::new(),
        value_hints: Vec::new(),
        description: None,
    }
}

/// The profile of a described object: columns with sensitive ones redacted, keys and indexes; no DDL.
fn described(node: &ObjectNode, description: &ObjectDescription) -> ObjectProfile {
    let primary_key: Vec<String> = description.primary_key.as_ref().map(|key| key.columns.clone()).unwrap_or_default();
    let columns: Vec<ColumnProfile> = description
        .columns
        .iter()
        .map(|column| {
            let sensitive = redact::is_sensitive_name(&column.name);
            ColumnProfile {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                kind: column.kind.clone(),
                nullable: column.nullable,
                primary: column.hints.primary || primary_key.contains(&column.name),
                comment: if sensitive {
                    None
                } else {
                    redact::clean(column.comment.as_deref())
                },
                default: if sensitive {
                    None
                } else {
                    redact::clean(column.default.as_deref())
                },
                sensitive,
                presence: None,
            }
        })
        .collect();
    let mut object = undetailed(node);
    object.detailed = true;
    object.fingerprint = fingerprint::object_fingerprint(
        node.kind.as_str(),
        &object.qualified(),
        columns.iter().map(|column| ColumnShape {
            name: &column.name,
            type_name: &column.type_name,
            nullable: column.nullable,
            primary: column.primary,
        }),
    );
    object.comment = redact::clean(description.comment.as_deref());
    object.columns = columns;
    object.primary_key = primary_key;
    object.unique_keys = description.unique_keys.iter().map(|key| key.columns.clone()).collect();
    object.indexes = description
        .indexes
        .iter()
        .map(|index| IndexProfile {
            name: index.name.clone(),
            columns: index.columns.iter().map(|column| redact::scrub(column)).collect(),
            unique: index.unique,
            primary: index.primary,
            method: index.method.clone(),
        })
        .collect();
    object.row_estimate = description.row_estimate;
    object.extra = description
        .extra
        .iter()
        .take(MAX_EXTRA)
        .map(|(key, value)| (key.clone(), redact::scrub(value)))
        .collect();
    object
}

/// The declared keys of a described object, both directions, as relations within `database`.
fn declared_relations(database: &str, object: &ObjectProfile, description: &ObjectDescription) -> Vec<RelationProfile> {
    let other_database = |name: &Option<String>| name.clone().filter(|name| !name.is_empty() && name != database);
    let outgoing = description.foreign_keys.iter().map(|key| RelationProfile {
        name: key.name.clone(),
        from: object.qualified(),
        columns: key.columns.clone(),
        to: qualified(key.ref_schema.as_deref().or(object.schema.as_deref()), &key.ref_table),
        to_columns: key.ref_columns.clone(),
        to_database: other_database(&key.ref_database),
        source: RelationSource::Fk,
        confidence: 1.0,
    });
    let incoming = description
        .referenced_by
        .iter()
        .filter(|key| other_database(&key.database).is_none())
        .map(|key| RelationProfile {
            name: key.name.clone(),
            from: qualified(key.schema.as_deref().or(object.schema.as_deref()), &key.table),
            columns: key.columns.clone(),
            to: object.qualified(),
            to_columns: key.ref_columns.clone(),
            to_database: None,
            source: RelationSource::Fk,
            confidence: 1.0,
        });
    outgoing.chain(incoming).collect()
}

/// The schema graph's fingerprint, where a SQL engine has one (a failure leaves the check out). Document engines are
/// left out: their graph comes from sampled documents, so it reads rows and moves between calls.
async fn graph_check(handle: &Handle, ctx: &CallContext, database: &str) -> Option<String> {
    if handle.engine.descriptor().family != EngineFamily::Sql || !handle.conn.capabilities().contains(Capability::SchemaGraph) {
        return None;
    }
    let (call_ctx, name) = (ctx.clone(), database.to_owned());
    let graph = handle
        .call_cancellable("read the schema", ctx.cancel.clone(), CANCEL_GRACE, |conn| async move {
            match conn.schema_graph() {
                Some(source) => source.schema_graph(&name, &call_ctx).await,
                None => Err(crate::contract::unsupported_capability("schema diagrams")),
            }
        })
        .await
        .ok()?;
    Some(fingerprint::graph_fingerprint(&graph))
}

/// The cheap structure checks of `database` now: the listing (children calls only) and the schema graph.
pub async fn checks(handle: &Handle, database: &str, cancel: &CancellationToken) -> AppResult<StructureChecks> {
    let ctx = CallContext::default().with_cancel(cancel.clone());
    let listed = list(handle, &ctx, database).await?;
    Ok(StructureChecks {
        listing: listing_fingerprint(&listed.objects),
        graph: graph_check(handle, &ctx, database).await,
    })
}

/// Builds the structural profile of `database` on `connection_id`. `cancel` stops it between and during engine calls
/// (the error is `CANCELLED`); `progress` hears each step.
pub async fn build(
    handle: &Handle,
    connection_id: &str,
    database: &str,
    cancel: &CancellationToken,
    progress: ProgressFn<'_>,
) -> AppResult<DatabaseProfile> {
    let ctx = CallContext::default().with_cancel(cancel.clone());
    progress(Progress::new(Step::Listing, 0, 0));
    let listed = list(handle, &ctx, database).await?;
    let detail_total = listed.objects.len().min(MAX_DETAILED_OBJECTS);
    let mut objects = Vec::with_capacity(listed.objects.len());
    let mut relations = Vec::new();
    for (index, node) in listed.objects.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let Some(reference) = node.object.clone().filter(|_| index < MAX_DETAILED_OBJECTS) else {
            objects.push(undetailed(node));
            continue;
        };
        progress(Progress {
            object: Some(qualified(reference.schema.as_deref(), &reference.name)),
            ..Progress::new(Step::Describing, index, detail_total)
        });
        let call_ctx = ctx.clone();
        let outcome = handle
            .call_cancellable("described an object", cancel.clone(), CANCEL_GRACE, |conn| async move {
                conn.describe(&reference, &call_ctx).await
            })
            .await;
        match outcome {
            Ok(description) => {
                let object = described(node, &description);
                relations.extend(declared_relations(database, &object, &description));
                objects.push(object);
            }
            Err(error) if cancelled_error(&error, cancel) => return Err(cancelled()),
            // One object the user cannot describe (permissions, a broken view) does not stop the build.
            Err(error) => {
                tracing::debug!(%error, "a profile kept an object it could not describe by name only");
                objects.push(undetailed(node));
            }
        }
    }
    progress(Progress::new(Step::Describing, detail_total, detail_total));
    progress(Progress::new(Step::Relations, 0, 1));
    let mut seen = HashSet::new();
    relations.retain(|relation| seen.insert((relation.from.clone(), relation.columns.clone(), relation.to.clone())));
    let inferred = infer_relations(&objects, &relations);
    relations.extend(inferred);
    let checks = StructureChecks {
        listing: listing_fingerprint(&listed.objects),
        graph: graph_check(handle, &ctx, database).await,
    };
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    progress(Progress::new(Step::Relations, 1, 1));
    let routines = listed
        .routines
        .iter()
        .map(|node| RoutineProfile {
            schema: node_schema(node),
            name: node_name(node),
            kind: node.kind.clone(),
            signature: redact::clean(node.detail.as_deref()),
        })
        .collect();
    let now = now_millis();
    let detailed_objects = objects.iter().filter(|object| object.detailed).count();
    Ok(DatabaseProfile {
        version: PROFILE_VERSION,
        engine: handle.engine.id().to_owned(),
        connection_id: connection_id.to_owned(),
        database: database.to_owned(),
        fingerprint: fingerprint::database_fingerprint(&objects),
        checks,
        first_learned_at: now,
        learned_at: now,
        data_read: false,
        model: None,
        input_tokens: 0,
        output_tokens: 0,
        total_objects: objects.len(),
        detailed_objects,
        truncated: listed.truncated || objects.len() > MAX_DETAILED_OBJECTS,
        excluded_objects: listed.excluded,
        objects,
        relations,
        routines,
        summary: None,
        notes: Vec::new(),
        extras: serde_json::Map::new(),
    })
}
