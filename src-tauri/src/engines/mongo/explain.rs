//! The query plan of a find or a pipeline, for the collection workspace's Explain section.
//!
//! - `mongo.explain { database, collection, kind: "find" | "aggregate", filter?, projection?, sort?, skip?, limit?,
//!   collation?, hint?, pipeline?, verbosity?, maxTimeMS? }` → `{ explain, plan, verbosity, statement, elapsedMs }`
//!   (reads). `kind` "find" takes the arguments of `mongo.find` (no default limit: the plan is the query's), "aggregate"
//!   a pipeline (`$out` / `$merge` are refused: an explain only reads). `verbosity` is "queryPlanner" (the default: the
//!   plan only), "executionStats" or "allPlansExecution", which run the query: still a read, bounded by `maxTimeMS` (the
//!   workspace's timeout at most) on the server and by a little more on the client.
//!
//! `explain` is the server's answer as relaxed Extended JSON (without `$clusterTime` / `operationTime`); `plan` is
//! [`normalise`]d from it, or null for a shape this module does not know (the raw answer still shows it):
//! `{ shape: "find" | "aggregate" | "sharded", explainVersion, winningPlan, rejectedPlans: [{ shard, plan }], summary }`.
//! Plan nodes are `{ stage, indexName?, keyPattern?, direction?, bounds?, isMultiKey?, filter?, details?, shard?,
//! nReturned?, executionTimeMillisEstimate?, totalKeysExamined?, totalDocsExamined?, children }`, the root the stage
//! that returns the documents and `children` its inputs; pipeline stages that do not run in the query layer are nodes
//! (`stage` the operator, `details` its specification) above the `$cursor`'s plan. The summary is `{ nReturned,
//! keysExamined, docsExamined, executionTimeMillis, isCollScan, usedIndexes, sortInMemory, fetchRatio }`, the counters
//! null without execution statistics. Classic plans (explainVersion 1), slot-based plans (explainVersion 2: the
//! `queryPlan` with the SBE stages' counters joined by `planNodeId`), sharded finds (`SHARD_MERGE` and per-shard plans)
//! and sharded aggregations (`shards`, `splitPipeline`) are understood.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mongodb::{
    bson::{doc, Bson, Document},
    Client, Collection,
};
use serde_json::{json, Map, Value as Json};

use super::{
    aggregate,
    ops::{document_arg, elapsed_ms, name_json, u64_arg},
    values,
};
use crate::error::{AppError, AppResult};

/// The verbosities of `explain`; the first is the default.
pub const VERBOSITIES: [&str; 3] = ["queryPlanner", "executionStats", "allPlansExecution"];
/// What the client waits beyond the server's time bound before giving up.
const CLIENT_GRACE: Duration = Duration::from_secs(5);
/// Stages that read an index (they carry `indexName` / `keyPattern`).
const INDEX_STAGES: [&str; 7] = [
    "IXSCAN",
    "COUNT_SCAN",
    "DISTINCT_SCAN",
    "EXPRESS_IXSCAN",
    "EXPRESS_CLUSTERED_IXSCAN",
    "TEXT_MATCH",
    "GEO_NEAR_2DSPHERE",
];
/// Stages that read documents (their `docsExamined`).
const DOCUMENT_STAGES: [&str; 5] = ["FETCH", "COLLSCAN", "IDHACK", "EXPRESS_IXSCAN", "CLUSTERED_IXSCAN"];
/// Keys of a plan node holding its inputs.
const INPUT_KEYS: [&str; 6] = ["inputStage", "inputStages", "innerStage", "outerStage", "thenStage", "elseStage"];

/// The `kind` argument: "find" (the default) or "aggregate".
pub fn explain_kind(args: &Json) -> AppResult<&str> {
    match args.get("kind").and_then(Json::as_str).map(str::trim) {
        None | Some("" | "find") => Ok("find"),
        Some("aggregate") => Ok("aggregate"),
        Some(other) => Err(AppError::new(format!("kind is \"find\" or \"aggregate\", not \"{other}\""))),
    }
}

/// The `verbosity` argument, checked; "queryPlanner" when absent.
pub fn verbosity(args: &Json) -> AppResult<&'static str> {
    match args.get("verbosity").and_then(Json::as_str).map(str::trim) {
        None | Some("") => Ok(VERBOSITIES[0]),
        Some(wanted) => VERBOSITIES.iter().find(|known| **known == wanted).copied().ok_or_else(|| {
            AppError::new(format!(
                "verbosity is \"queryPlanner\", \"executionStats\" or \"allPlansExecution\", not \"{wanted}\""
            ))
        }),
    }
}

/// A hint as the command takes it: an index name or a key document.
fn hint_bson(args: &Json) -> AppResult<Option<Bson>> {
    match args.get("hint") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(name)) if !name.trim_start().starts_with('{') => {
            Ok((!name.trim().is_empty()).then(|| Bson::String(name.trim().to_owned())))
        }
        Some(_) => Ok(document_arg(args, "hint")?.map(Bson::Document)),
    }
}

fn max_time_ms(max_time: Duration) -> i64 {
    i64::try_from(max_time.as_millis()).unwrap_or(i64::MAX)
}

/// The command `explain` wraps, and its mongosh text (`db.getCollection(…).explain(…).find(…)`).
pub fn explained_command(collection: &str, args: &Json, verbosity: &str, max_time: Duration) -> AppResult<(Document, String)> {
    let mut shell = format!("db.getCollection({}).explain({})", name_json(collection), name_json(verbosity));
    let mut command = Document::new();
    if explain_kind(args)? == "aggregate" {
        let stages = aggregate::pipeline_arg(args)?.ok_or_else(|| AppError::new("pipeline is required"))?;
        if let Some(stage) = aggregate::write_stage(&stages) {
            return Err(AppError::new(format!(
                "{stage} writes into a collection; explain only reads, so leave it out"
            )));
        }
        let pipeline = stages
            .into_iter()
            .enumerate()
            .map(|(index, stage)| match Bson::try_from(stage) {
                Ok(Bson::Document(stage)) => Ok(stage),
                Ok(_) => Err(AppError::new(format!("Stage {} must be a document", index + 1))),
                Err(error) => Err(AppError::new(format!("Stage {} is not valid Extended JSON ({error})", index + 1))),
            })
            .collect::<AppResult<Vec<_>>>()?;
        shell.push_str(&format!(
            ".aggregate([{}])",
            pipeline.iter().map(values::document_text).collect::<Vec<_>>().join(", ")
        ));
        command.insert("aggregate", collection);
        command.insert("pipeline", pipeline);
        command.insert("cursor", Document::new());
    } else {
        let filter = document_arg(args, "filter")?.unwrap_or_default();
        let projection = document_arg(args, "projection")?.filter(|projection| !projection.is_empty());
        shell.push_str(&format!(".find({}", values::document_text(&filter)));
        if let Some(projection) = &projection {
            shell.push_str(&format!(", {}", values::document_text(projection)));
        }
        shell.push(')');
        command.insert("find", collection);
        command.insert("filter", filter);
        if let Some(projection) = projection {
            command.insert("projection", projection);
        }
        if let Some(sort) = document_arg(args, "sort")?.filter(|sort| !sort.is_empty()) {
            shell.push_str(&format!(".sort({})", values::document_text(&sort)));
            command.insert("sort", sort);
        }
        if let Some(skip) = u64_arg(args, "skip")?.filter(|skip| *skip > 0) {
            shell.push_str(&format!(".skip({skip})"));
            command.insert("skip", i64::try_from(skip).unwrap_or(i64::MAX));
        }
        if let Some(limit) = u64_arg(args, "limit")?.filter(|limit| *limit > 0) {
            shell.push_str(&format!(".limit({limit})"));
            command.insert("limit", i64::try_from(limit).unwrap_or(i64::MAX));
        }
    }
    if let Some(collation) = document_arg(args, "collation")?.filter(|collation| !collation.is_empty()) {
        command.insert("collation", collation);
    }
    if let Some(hint) = hint_bson(args)? {
        command.insert("hint", hint);
    }
    command.insert("maxTimeMS", max_time_ms(max_time));
    Ok((command, shell))
}

/// Runs `explain` on the command of `args`.
pub async fn explain(client: &Client, coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let verbosity = verbosity(args)?;
    let (command, statement) = explained_command(coll.name(), args, verbosity, max_time)?;
    let started = Instant::now();
    let database = client.database(&coll.namespace().db);
    let run = database.run_command(doc! { "explain": command, "verbosity": verbosity });
    let mut reply = match tokio::time::timeout(max_time + CLIENT_GRACE, run).await {
        Ok(reply) => reply?,
        Err(_) => {
            return Err(AppError::new(format!(
                "The explain did not finish within {} ms",
                max_time.as_millis()
            )))
        }
    };
    let elapsed_ms = elapsed_ms(started);
    for noise in ["$clusterTime", "operationTime", "$configTime", "$topologyTime"] {
        reply.remove(noise);
    }
    let explain = Bson::Document(reply).into_relaxed_extjson();
    let plan = normalise(&explain);
    Ok(json!({
        "explain": explain,
        "plan": plan,
        "verbosity": verbosity,
        "statement": statement,
        "elapsedMs": elapsed_ms,
    }))
}

// --- Normalising -----------------------------------------------------------------------------------------------------

/// Counters of a whole plan (null without execution statistics).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Totals {
    returned: Option<i64>,
    keys: Option<i64>,
    docs: Option<i64>,
    millis: Option<i64>,
}

impl Totals {
    fn of(stats: Option<&Json>) -> Totals {
        let Some(stats) = stats else {
            return Totals::default();
        };
        Totals {
            returned: int(stats.get("nReturned")),
            keys: int(stats.get("totalKeysExamined")),
            docs: int(stats.get("totalDocsExamined")),
            millis: int(stats.get("executionTimeMillis")),
        }
    }

    /// Shards run side by side: counters add up, the time is the slowest shard's.
    fn add(self, other: Totals) -> Totals {
        let sum = |a: Option<i64>, b: Option<i64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        Totals {
            returned: sum(self.returned, other.returned),
            keys: sum(self.keys, other.keys),
            docs: sum(self.docs, other.docs),
            millis: self.millis.max(other.millis),
        }
    }
}

struct Plan {
    winning: Json,
    rejected: Vec<Json>,
    totals: Totals,
    sharded: bool,
}

/// A whole number in a relaxed Extended JSON value (`5`, `5.0`, `{ "$numberLong": "5" }`).
fn int(value: Option<&Json>) -> Option<i64> {
    match value? {
        Json::Number(number) => number.as_i64().or_else(|| number.as_f64().map(|n| n as i64)),
        Json::Object(object) => object
            .get("$numberLong")
            .or_else(|| object.get("$numberInt"))
            .and_then(Json::as_str)
            .and_then(|text| text.parse().ok()),
        _ => None,
    }
}

fn text<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Json::as_str)
}

/// The classic tree of a plan entry: `queryPlan` in slot-based explains, the entry itself otherwise.
fn plan_root(entry: &Json) -> &Json {
    entry.get("queryPlan").unwrap_or(entry)
}

/// The inputs of a raw plan node.
fn inputs(raw: &Json) -> Vec<&Json> {
    let mut found = Vec::new();
    for key in INPUT_KEYS {
        match raw.get(key) {
            Some(Json::Array(items)) => found.extend(items.iter().filter(|item| item.is_object())),
            Some(item @ Json::Object(_)) => found.push(item),
            _ => {}
        }
    }
    found
}

/// One normalised node of a raw classic plan or execution tree.
fn node(raw: &Json) -> Json {
    let mut out = Map::new();
    out.insert("stage".into(), json!(text(raw, "stage").unwrap_or("?")));
    for key in ["indexName", "keyPattern", "direction", "isMultiKey", "filter", "planNodeId"] {
        if let Some(value) = raw.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(bounds) = raw.get("indexBounds") {
        out.insert("bounds".into(), bounds.clone());
    }
    if text(raw, "stage") == Some("IDHACK") {
        out.insert("indexName".into(), json!("_id_"));
        out.insert("keyPattern".into(), json!({ "_id": 1 }));
    }
    let metric = |names: &[&str]| names.iter().find_map(|name| int(raw.get(*name)));
    let metrics = [
        ("nReturned", metric(&["nReturned"])),
        (
            "executionTimeMillisEstimate",
            metric(&["executionTimeMillisEstimate", "executionTimeMillis"]),
        ),
        ("totalKeysExamined", metric(&["keysExamined", "totalKeysExamined"])),
        ("totalDocsExamined", metric(&["docsExamined", "totalDocsExamined"])),
    ];
    for (key, value) in metrics {
        if let Some(value) = value {
            out.insert(key.into(), json!(value));
        }
    }
    out.insert("children".into(), Json::Array(inputs(raw).into_iter().map(node).collect()));
    Json::Object(out)
}

fn children_mut(node: &mut Json) -> &mut Vec<Json> {
    if !node.get("children").is_some_and(Json::is_array) {
        node["children"] = json!([]);
    }
    match node.get_mut("children") {
        Some(Json::Array(children)) => children,
        _ => unreachable!("children was just set to an array"),
    }
}

/// Pre-order (stage, index) pairs: two trees of the same plan have the same signature.
fn signature(raw: &Json, out: &mut Vec<String>) {
    out.push(format!(
        "{}:{}",
        text(raw, "stage").unwrap_or("?"),
        text(raw, "indexName").unwrap_or("")
    ));
    for input in inputs(raw) {
        signature(input, out);
    }
}

fn signature_of(raw: &Json) -> Vec<String> {
    let mut out = Vec::new();
    signature(raw, &mut out);
    out
}

/// The counters of a slot-based execution tree by `planNodeId`: the topmost SBE stage of a plan node gives its returned
/// count and time, keys and documents examined add up over its stages.
fn sbe_metrics(raw: &Json, out: &mut HashMap<i64, Map<String, Json>>) {
    fn walk(value: &Json, out: &mut HashMap<i64, Map<String, Json>>) {
        let Json::Object(object) = value else {
            return;
        };
        if object.contains_key("stage") {
            if let Some(id) = int(object.get("planNodeId")) {
                let entry = out.entry(id).or_default();
                for key in ["nReturned", "executionTimeMillisEstimate"] {
                    if !entry.contains_key(key) {
                        if let Some(n) = int(object.get(key)) {
                            entry.insert(key.into(), json!(n));
                        }
                    }
                }
                for (from, to) in [("keysExamined", "totalKeysExamined"), ("docsExamined", "totalDocsExamined")] {
                    if let Some(n) = int(object.get(from)) {
                        let sum = int(entry.get(to)).unwrap_or(0) + n;
                        entry.insert(to.into(), json!(sum));
                    }
                }
            }
        }
        for item in object.values() {
            match item {
                Json::Object(_) => walk(item, out),
                Json::Array(items) => items.iter().for_each(|item| walk(item, out)),
                _ => {}
            }
        }
    }
    walk(raw, out);
}

fn attach_sbe(node: &mut Json, metrics: &HashMap<i64, Map<String, Json>>) {
    if let Some(found) = int(node.get("planNodeId")).and_then(|id| metrics.get(&id)) {
        for (key, value) in found {
            if node.get(key).is_none() {
                node[key.as_str()] = value.clone();
            }
        }
    }
    for child in children_mut(node) {
        attach_sbe(child, metrics);
    }
}

fn collect_stages<'a>(node: &'a Json, wanted: &[&str], out: &mut Vec<&'a Json>) {
    if text(node, "stage").is_some_and(|stage| wanted.contains(&stage)) {
        out.push(node);
    }
    for child in node.get("children").and_then(Json::as_array).into_iter().flatten() {
        collect_stages(child, wanted, out);
    }
}

/// Fills the plan's totals into the one stage they belong to when the stages lack them (slot-based trees, queryPlanner
/// answers of a sharded find): keys into the only index scan, documents into the only fetch or scan, the returned count
/// into the root.
fn fill_from_totals(root: &mut Json, totals: Totals) {
    fn only<'a>(node: &'a mut Json, wanted: &[&str], key: &str) -> Option<&'a mut Json> {
        let mut found = Vec::new();
        collect_stages(node, wanted, &mut found);
        if found.len() != 1 || found[0].get(key).is_some() {
            return None;
        }
        fn find<'b>(node: &'b mut Json, wanted: &[&str]) -> Option<&'b mut Json> {
            if text(node, "stage").is_some_and(|stage| wanted.contains(&stage)) {
                return Some(node);
            }
            children_mut(node).iter_mut().find_map(|child| find(child, wanted))
        }
        find(node, wanted)
    }
    if let Some(keys) = totals.keys {
        if let Some(scan) = only(root, &INDEX_STAGES, "totalKeysExamined") {
            scan["totalKeysExamined"] = json!(keys);
        }
    }
    if let Some(docs) = totals.docs {
        if let Some(fetch) = only(root, &DOCUMENT_STAGES, "totalDocsExamined") {
            fetch["totalDocsExamined"] = json!(docs);
        }
    }
    if let (Some(returned), None) = (totals.returned, root.get("nReturned")) {
        root["nReturned"] = json!(returned);
    }
}

/// The plan of a find explain (`queryPlanner`, `executionStats`), sharded or not.
fn find_plan(explain: &Json) -> Option<Plan> {
    let planner = explain.get("queryPlanner")?;
    let winning_raw = planner.get("winningPlan")?;
    let stats = explain.get("executionStats");
    let totals = Totals::of(stats);
    if let Some(shards) = winning_raw.get("shards").and_then(Json::as_array) {
        let execution = stats.and_then(|stats| stats.get("executionStages"));
        let shard_stats = execution.and_then(|stages| stages.get("shards")).and_then(Json::as_array);
        let mut root = json!({ "stage": text(winning_raw, "stage").unwrap_or("SHARD_MERGE"), "children": [] });
        if let Some(execution) = execution {
            for (from, to) in [
                ("nReturned", "nReturned"),
                ("executionTimeMillis", "executionTimeMillisEstimate"),
                ("totalKeysExamined", "totalKeysExamined"),
                ("totalDocsExamined", "totalDocsExamined"),
            ] {
                if let Some(n) = int(execution.get(from)) {
                    root[to] = json!(n);
                }
            }
        }
        let mut rejected = Vec::new();
        for shard in shards {
            let name = text(shard, "shardName").unwrap_or("?");
            let shard_execution = shard_stats.and_then(|all| all.iter().find(|entry| text(entry, "shardName") == Some(name)));
            let sub = json!({ "queryPlanner": shard, "executionStats": shard_execution.cloned().unwrap_or(Json::Null) });
            let Some(plan) = find_plan(&sub) else {
                continue;
            };
            let mut shard_node = json!({ "stage": "SHARD", "shard": name, "children": [plan.winning] });
            for (key, value) in [
                ("nReturned", plan.totals.returned),
                ("executionTimeMillisEstimate", plan.totals.millis),
                ("totalKeysExamined", plan.totals.keys),
                ("totalDocsExamined", plan.totals.docs),
            ] {
                if let Some(value) = value {
                    shard_node[key] = json!(value);
                }
            }
            children_mut(&mut root).push(shard_node);
            rejected.extend(plan.rejected.into_iter().map(|mut entry| {
                entry["shard"] = json!(name);
                entry
            }));
        }
        return Some(Plan {
            winning: root,
            rejected,
            totals,
            sharded: true,
        });
    }
    let slot_based = winning_raw.get("queryPlan").is_some();
    let execution = stats
        .and_then(|stats| stats.get("executionStages"))
        .filter(|stages| stages.is_object());
    let mut winning = match execution {
        Some(execution) if !slot_based => node(execution),
        _ => node(plan_root(winning_raw)),
    };
    if let (true, Some(execution)) = (slot_based, execution) {
        let mut metrics = HashMap::new();
        sbe_metrics(execution, &mut metrics);
        attach_sbe(&mut winning, &metrics);
    }
    fill_from_totals(&mut winning, totals);
    let all_plans = stats
        .and_then(|stats| stats.get("allPlansExecution"))
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let rejected = planner
        .get("rejectedPlans")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .map(|entry| {
            let tree = plan_root(entry);
            let wanted = signature_of(tree);
            let executed = all_plans
                .iter()
                .filter_map(|run| run.get("executionStages"))
                .find(|stages| entry.get("queryPlan").is_none() && signature_of(stages) == wanted);
            json!({ "shard": Json::Null, "plan": node(executed.unwrap_or(tree)) })
        })
        .collect();
    Some(Plan {
        winning,
        rejected,
        totals,
        sharded: false,
    })
}

/// A pipeline stage that runs above the query layer, as a node over its input.
fn stage_node(stage: &Json, input: Option<Json>) -> Json {
    let (operator, details) = stage
        .as_object()
        .and_then(|object| object.iter().find(|(key, _)| key.starts_with('$')))
        .map(|(key, value)| (key.as_str(), value.clone()))
        .unwrap_or(("?", Json::Null));
    let mut out = json!({ "stage": operator, "details": details, "children": input.into_iter().collect::<Vec<_>>() });
    for key in ["nReturned", "executionTimeMillisEstimate"] {
        if let Some(n) = int(stage.get(key)) {
            out[key] = json!(n);
        }
    }
    out
}

/// The plan of an aggregation explain with `stages` (the first a `$cursor` holding the find layer's explain).
fn stages_plan(explain: &Json) -> Option<Plan> {
    let stages = explain.get("stages")?.as_array()?;
    let mut current: Option<Json> = None;
    let mut rejected = Vec::new();
    let mut totals = Totals::default();
    let mut sharded = false;
    let mut last_returned = None;
    for stage in stages {
        if let Some(cursor) = stage.get("$cursor") {
            let plan = find_plan(cursor)?;
            let mut winning = plan.winning;
            if winning.get("nReturned").is_none() {
                if let Some(n) = int(stage.get("nReturned")) {
                    winning["nReturned"] = json!(n);
                }
            }
            current = Some(winning);
            rejected = plan.rejected;
            totals = plan.totals;
            sharded |= plan.sharded;
            last_returned = int(stage.get("nReturned")).or(plan.totals.returned);
        } else {
            last_returned = int(stage.get("nReturned")).or(last_returned);
            current = Some(stage_node(stage, current.take()));
        }
    }
    if let Some(returned) = last_returned {
        totals.returned = Some(returned);
    }
    Some(Plan {
        winning: current?,
        rejected,
        totals,
        sharded,
    })
}

/// The plan of one shard's or one unsharded answer.
fn any_plan(explain: &Json) -> Option<Plan> {
    if explain.get("stages").is_some_and(Json::is_array) {
        // A pipeline partly pushed into the query layer can also carry the planner at the top level.
        return stages_plan(explain);
    }
    if explain.get("queryPlanner").is_some() {
        return find_plan(explain);
    }
    None
}

/// The plan of a sharded aggregation: `shards` by name, the merging part (`splitPipeline.mergerPart`) on top.
fn sharded_aggregate_plan(explain: &Json) -> Option<Plan> {
    let shards = explain.get("shards")?.as_object()?;
    let merge = match text(explain, "mergeType") {
        Some("mongos") => "SHARD_MERGE (mongos)",
        Some("primaryShard") => "SHARD_MERGE (primary shard)",
        Some("anyShard") => "SHARD_MERGE (any shard)",
        _ => "SHARD_MERGE",
    };
    let mut root = json!({ "stage": merge, "children": [] });
    let mut rejected = Vec::new();
    let mut totals = Totals::default();
    for (name, shard) in shards {
        let Some(plan) = any_plan(shard) else {
            continue;
        };
        let mut shard_node = json!({ "stage": "SHARD", "shard": name, "children": [plan.winning] });
        if let Some(host) = text(shard, "host") {
            shard_node["details"] = json!({ "host": host });
        }
        for (key, value) in [
            ("nReturned", plan.totals.returned),
            ("executionTimeMillisEstimate", plan.totals.millis),
            ("totalKeysExamined", plan.totals.keys),
            ("totalDocsExamined", plan.totals.docs),
        ] {
            if let Some(value) = value {
                shard_node[key] = json!(value);
            }
        }
        totals = totals.add(plan.totals);
        children_mut(&mut root).push(shard_node);
        rejected.extend(plan.rejected.into_iter().map(|mut entry| {
            entry["shard"] = json!(name);
            entry
        }));
    }
    if children_mut(&mut root).is_empty() {
        return None;
    }
    let merger = explain
        .get("splitPipeline")
        .and_then(|split| split.get("mergerPart"))
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let mut current = root;
    // `$mergeCursors` is the merge itself (the root above).
    for stage in merger.iter().filter(|stage| stage.get("$mergeCursors").is_none()) {
        current = stage_node(stage, Some(current));
    }
    Some(Plan {
        winning: current,
        rejected,
        totals,
        sharded: true,
    })
}

fn summary(winning: &Json, totals: Totals) -> Json {
    fn walk(node: &Json, scan: &mut bool, sort: &mut bool, indexes: &mut Vec<String>) {
        match text(node, "stage").unwrap_or_default() {
            "COLLSCAN" => *scan = true,
            "SORT" | "$sort" => *sort = true,
            _ => {}
        }
        if let Some(name) = text(node, "indexName") {
            if !indexes.iter().any(|known| known == name) {
                indexes.push(name.to_owned());
            }
        }
        for child in node.get("children").and_then(Json::as_array).into_iter().flatten() {
            walk(child, scan, sort, indexes);
        }
    }
    let (mut scan, mut sort, mut indexes) = (false, false, Vec::new());
    walk(winning, &mut scan, &mut sort, &mut indexes);
    let ratio = match (totals.docs, totals.returned) {
        (Some(docs), Some(returned)) => Some((docs as f64 / returned.max(1) as f64 * 100.0).round() / 100.0),
        _ => None,
    };
    json!({
        "nReturned": totals.returned,
        "keysExamined": totals.keys,
        "docsExamined": totals.docs,
        "executionTimeMillis": totals.millis,
        "isCollScan": scan,
        "usedIndexes": indexes,
        "sortInMemory": sort,
        "fetchRatio": ratio,
    })
}

/// The normalised plan of an explain answer (relaxed Extended JSON), or None for a shape this module does not know.
pub fn normalise(explain: &Json) -> Option<Json> {
    let (plan, shape) = if explain.get("stages").is_some_and(Json::is_array) {
        (stages_plan(explain)?, "aggregate")
    } else if explain.get("queryPlanner").is_some() {
        (find_plan(explain)?, "find")
    } else if explain.get("shards").is_some_and(Json::is_object) {
        (sharded_aggregate_plan(explain)?, "aggregate")
    } else {
        return None;
    };
    let shape = if plan.sharded { "sharded" } else { shape };
    let version = explain
        .get("explainVersion")
        .cloned()
        .or_else(|| {
            explain
                .get("shards")
                .and_then(Json::as_object)
                .and_then(|shards| shards.values().find_map(|shard| shard.get("explainVersion").cloned()))
        })
        .unwrap_or(Json::Null);
    Some(json!({
        "shape": shape,
        "explainVersion": version,
        "summary": summary(&plan.winning, plan.totals),
        "winningPlan": plan.winning,
        "rejectedPlans": plan.rejected,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Json {
        let text = match name {
            "find_6_classic" => include_str!("fixtures/explain/find_6_classic.json"),
            "find_7_sbe" => include_str!("fixtures/explain/find_7_sbe.json"),
            "find_8_collscan" => include_str!("fixtures/explain/find_8_collscan.json"),
            "find_8_express" => include_str!("fixtures/explain/find_8_express.json"),
            "aggregate_6_cursor" => include_str!("fixtures/explain/aggregate_6_cursor.json"),
            "aggregate_7_pushdown" => include_str!("fixtures/explain/aggregate_7_pushdown.json"),
            "sharded_find_7" => include_str!("fixtures/explain/sharded_find_7.json"),
            "sharded_aggregate_6" => include_str!("fixtures/explain/sharded_aggregate_6.json"),
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(text).unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    fn stages(node: &Json) -> Vec<String> {
        let mut out = vec![text(node, "stage").unwrap_or("?").to_owned()];
        if let Some(child) = node["children"].as_array().and_then(|children| children.first()) {
            out.extend(stages(child));
        }
        out
    }

    #[test]
    fn arguments_are_checked() {
        assert_eq!(explain_kind(&json!({})).unwrap(), "find");
        assert_eq!(explain_kind(&json!({ "kind": "aggregate" })).unwrap(), "aggregate");
        assert!(explain_kind(&json!({ "kind": "update" })).is_err());
        assert_eq!(verbosity(&json!({})).unwrap(), "queryPlanner");
        assert_eq!(
            verbosity(&json!({ "verbosity": "allPlansExecution" })).unwrap(),
            "allPlansExecution"
        );
        assert!(verbosity(&json!({ "verbosity": "everything" })).is_err());
    }

    #[test]
    fn commands_carry_the_query_and_the_time_bound() {
        let args = json!({
            "filter": "{ \"status\": \"paid\" }",
            "sort": { "createdAt": -1 },
            "projection": {},
            "limit": 20,
            "skip": 0,
            "hint": "status_1",
            "collation": { "locale": "en" },
        });
        let (command, shell) = explained_command("orders", &args, "executionStats", Duration::from_millis(1500)).unwrap();
        assert_eq!(command.get_str("find").unwrap(), "orders");
        assert_eq!(command.get_document("filter").unwrap(), &doc! { "status": "paid" });
        assert!(command.get("projection").is_none() && command.get("skip").is_none());
        assert_eq!(command.get_i64("limit").unwrap(), 20);
        assert_eq!(command.get_str("hint").unwrap(), "status_1");
        assert_eq!(command.get_i64("maxTimeMS").unwrap(), 1500);
        assert_eq!(
            shell,
            r#"db.getCollection("orders").explain("executionStats").find({"status":"paid"}).sort({"createdAt":-1}).limit(20)"#
        );
        let pipeline = json!({ "kind": "aggregate", "pipeline": ["{ \"$match\": { \"a\": 1 } }", { "$group": { "_id": "$b" } }] });
        let (command, shell) = explained_command("orders", &pipeline, "queryPlanner", Duration::from_secs(2)).unwrap();
        assert_eq!(command.get_array("pipeline").unwrap().len(), 2);
        assert!(command.get_document("cursor").is_ok());
        assert!(
            shell.contains(".aggregate([{\"$match\":{\"a\":1}}, {\"$group\":{\"_id\":\"$b\"}}])"),
            "{shell}"
        );
        for writes in [
            json!([{ "$out": "copy" }]),
            json!([{ "$match": {} }, { "$merge": { "into": "t" } }]),
        ] {
            let args = json!({ "kind": "aggregate", "pipeline": writes });
            let error = explained_command("orders", &args, "queryPlanner", Duration::from_secs(1)).unwrap_err();
            assert!(error.message().contains("only reads"), "{error}");
        }
        assert!(explained_command("orders", &json!({ "kind": "aggregate" }), "queryPlanner", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn classic_find_with_rejected_plans() {
        let plan = normalise(&fixture("find_6_classic")).unwrap();
        assert_eq!(plan["shape"], "find");
        assert_eq!(plan["explainVersion"], "1");
        let winning = &plan["winningPlan"];
        assert_eq!(stages(winning), ["SORT", "FETCH", "IXSCAN"]);
        let scan = &winning["children"][0]["children"][0];
        assert_eq!(scan["indexName"], "status_1");
        assert_eq!(scan["keyPattern"], json!({ "status": 1 }));
        assert_eq!(scan["direction"], "forward");
        assert_eq!(scan["totalKeysExamined"], 1204);
        assert!(scan["bounds"]["status"].is_array());
        assert_eq!(winning["children"][0]["totalDocsExamined"], 1204);
        assert_eq!(winning["children"][0]["filter"], json!({ "total": { "$gt": 100 } }));
        assert_eq!(winning["nReturned"], 20);
        let rejected = plan["rejectedPlans"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        // allPlansExecution gives the rejected plan its trial counters.
        let rejected_plan = &rejected[0]["plan"];
        assert_eq!(stages(rejected_plan), ["SORT", "FETCH", "IXSCAN"]);
        assert_eq!(rejected_plan["children"][0]["children"][0]["indexName"], "total_1");
        assert_eq!(rejected_plan["children"][0]["children"][0]["totalKeysExamined"], 101);
        let summary = &plan["summary"];
        assert_eq!(summary["nReturned"], 20);
        assert_eq!(summary["keysExamined"], 1204);
        assert_eq!(summary["docsExamined"], 1204);
        assert_eq!(summary["executionTimeMillis"], 7);
        assert_eq!(summary["isCollScan"], false);
        assert_eq!(summary["usedIndexes"], json!(["status_1"]));
        assert_eq!(summary["sortInMemory"], true);
        assert_eq!(summary["fetchRatio"], 60.2);
    }

    #[test]
    fn slot_based_find_joins_counters_by_plan_node() {
        let plan = normalise(&fixture("find_7_sbe")).unwrap();
        assert_eq!(plan["explainVersion"], "2");
        let winning = &plan["winningPlan"];
        assert_eq!(stages(winning), ["LIMIT", "FETCH", "IXSCAN"]);
        assert_eq!(winning["nReturned"], 20);
        let fetch = &winning["children"][0];
        assert_eq!(fetch["nReturned"], 20);
        assert_eq!(fetch["totalDocsExamined"], 20);
        let scan = &fetch["children"][0];
        assert_eq!(scan["indexName"], "customerId_1_placedAt_-1");
        assert_eq!(scan["totalKeysExamined"], 20);
        assert_eq!(scan["executionTimeMillisEstimate"], 0);
        assert_eq!(plan["rejectedPlans"].as_array().unwrap().len(), 1);
        let rejected = &plan["rejectedPlans"][0]["plan"];
        assert_eq!(stages(rejected), ["LIMIT", "FETCH", "IXSCAN"]);
        assert_eq!(rejected["children"][0]["children"][0]["indexName"], "placedAt_-1");
        let summary = &plan["summary"];
        assert_eq!(summary["usedIndexes"], json!(["customerId_1_placedAt_-1"]));
        assert_eq!(summary["sortInMemory"], false);
        assert_eq!(summary["fetchRatio"], 1.0);
    }

    #[test]
    fn query_planner_only_collection_scan() {
        let plan = normalise(&fixture("find_8_collscan")).unwrap();
        let winning = &plan["winningPlan"];
        assert_eq!(stages(winning), ["COLLSCAN"]);
        assert_eq!(winning["filter"], json!({ "note": { "$regex": "late" } }));
        let summary = &plan["summary"];
        assert_eq!(summary["isCollScan"], true);
        assert_eq!(summary["nReturned"], Json::Null);
        assert_eq!(summary["fetchRatio"], Json::Null);
        assert_eq!(summary["usedIndexes"], json!([]));
        assert!(plan["rejectedPlans"].as_array().unwrap().is_empty());
    }

    #[test]
    fn express_id_lookup_uses_the_id_index() {
        let plan = normalise(&fixture("find_8_express")).unwrap();
        assert_eq!(stages(&plan["winningPlan"]), ["EXPRESS_IXSCAN"]);
        assert_eq!(plan["summary"]["usedIndexes"], json!(["_id_"]));
        assert_eq!(plan["winningPlan"]["totalKeysExamined"], 1);
        assert_eq!(plan["winningPlan"]["totalDocsExamined"], 1);
    }

    #[test]
    fn aggregation_stages_sit_above_the_cursor() {
        let plan = normalise(&fixture("aggregate_6_cursor")).unwrap();
        assert_eq!(plan["shape"], "aggregate");
        let winning = &plan["winningPlan"];
        assert_eq!(stages(winning), ["$sort", "$group", "PROJECTION_SIMPLE", "COLLSCAN"]);
        assert_eq!(winning["nReturned"], 12);
        assert_eq!(winning["children"][0]["details"]["_id"], "$customerId");
        let summary = &plan["summary"];
        assert_eq!(summary["isCollScan"], true);
        assert_eq!(summary["sortInMemory"], true);
        assert_eq!(summary["nReturned"], 12);
        assert_eq!(summary["docsExamined"], 5000);

        let pushed = normalise(&fixture("aggregate_7_pushdown")).unwrap();
        assert_eq!(stages(&pushed["winningPlan"]), ["$sort", "GROUP", "IXSCAN"]);
        assert_eq!(pushed["summary"]["usedIndexes"], json!(["status_1"]));
        assert_eq!(pushed["summary"]["nReturned"], 3);
    }

    #[test]
    fn sharded_find_merges_shards() {
        let plan = normalise(&fixture("sharded_find_7")).unwrap();
        assert_eq!(plan["shape"], "sharded");
        let winning = &plan["winningPlan"];
        assert_eq!(winning["stage"], "SHARD_MERGE");
        let shards = winning["children"].as_array().unwrap();
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0]["shard"], "shard01");
        assert_eq!(stages(&shards[0]), ["SHARD", "SHARDING_FILTER", "FETCH", "IXSCAN"]);
        assert_eq!(stages(&shards[1]), ["SHARD", "SHARDING_FILTER", "COLLSCAN"]);
        assert_eq!(shards[1]["totalDocsExamined"], 2600);
        let summary = &plan["summary"];
        assert_eq!(summary["isCollScan"], true);
        assert_eq!(summary["usedIndexes"], json!(["status_1"]));
        assert_eq!(summary["docsExamined"], 3000);
        assert_eq!(plan["rejectedPlans"][0]["shard"], "shard01");

        let aggregated = normalise(&fixture("sharded_aggregate_6")).unwrap();
        assert_eq!(aggregated["shape"], "sharded");
        let winning = &aggregated["winningPlan"];
        assert_eq!(
            stages(winning),
            [
                "$group",
                "SHARD_MERGE (mongos)",
                "SHARD",
                "$group",
                "PROJECTION_SIMPLE",
                "FETCH",
                "IXSCAN"
            ]
        );
        assert_eq!(winning["children"][0]["children"].as_array().unwrap().len(), 2);
        assert_eq!(aggregated["summary"]["keysExamined"], 900);
    }

    #[test]
    fn unknown_shapes_fall_back_to_raw() {
        assert_eq!(normalise(&json!({ "ok": 1 })), None);
        assert_eq!(normalise(&json!({ "queryPlanner": { "namespace": "a.b" } })), None);
        assert_eq!(normalise(&json!({ "stages": [{ "$cursor": { "weird": true } }] })), None);
        assert_eq!(normalise(&json!({ "shards": {} })), None);
    }
}
