//! Aggregations from the collection workspace: a per-stage preview on a bounded sample, and the full run.
//!
//! The `pipeline` argument is an array of stage documents, each a relaxed or canonical Extended JSON value or JSON text
//! (one stage per element, one stage operator per document); the whole pipeline may also be given as JSON text.
//!
//! - `mongo.aggregatePreview { database, collection, pipeline, upTo?, sampleSize?, sampleMode?, limit?, maxTimeMS?,
//!   collation? }` → `{ stages: [{ index, operator, count, elapsedMs, documents, error }], sample: { mode, size, count },
//!   elapsedMs }`. Reads only: `$out` / `$merge` are refused. The input is a bounded sample (`sampleSize`, default
//!   [`DEFAULT_SAMPLE`], at most [`MAX_SAMPLE`]): the first documents in natural order (`sampleMode` `"first"`, the
//!   default) or a random sample (`"random"`: `$sample` once, then the same documents by `_id` for every stage, so the
//!   stages see one input). For each stage up to `upTo` (0-based, default the last) the pipeline prefix runs on that
//!   sample twice, once ending in `$count` and once in `$limit` (`limit` documents, default 20, at most 100); `$facet` is
//!   not used, so pipelines may hold their own `$facet` and large documents do not meet its 16 MB result limit. A
//!   stage that has to be first (`$geoNear`, `$search`, `$vectorSearch`, …) stays first and the sample (always the first
//!   documents) follows it. The first stage that fails carries the error and stops the preview. One time bound covers
//!   the whole preview: each run gets what is left of it.
//! - `mongo.aggregate { database, collection, pipeline, limit?, allowDiskUse?, collation?, hint?, maxTimeMS? }` →
//!   `{ documents, hasMore, elapsedMs, statement, wrote }`: at most `limit` documents (default 50, at most 1000). With
//!   `$out` / `$merge` (classified as DDL / a write) nothing is returned and `wrote` names the stage and the target
//!   collection; otherwise `wrote` is null.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, Bson, Document},
    options::Collation,
    Collection,
};
use serde_json::{json, Value as Json};

use super::{
    ops::{bool_arg, collation_arg, elapsed_ms, hint_arg, name_json, relaxed, u64_arg},
    values::{self, ID_FIELD},
};
use crate::error::{AppError, AppResult};

/// Input documents a preview samples when no size is given, and the most it samples.
pub const DEFAULT_SAMPLE: u64 = 1000;
pub const MAX_SAMPLE: u64 = 10_000;
/// Documents a preview returns per stage when no limit is given, and the most it returns.
const DEFAULT_PREVIEW_LIMIT: u64 = 20;
const MAX_PREVIEW_LIMIT: u64 = 100;
/// Documents a full run returns when no limit is given, and the most it returns.
const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 1000;

/// Stages that write their input into a collection.
pub const WRITE_STAGES: [&str; 2] = ["$out", "$merge"];
/// Stages the server accepts only as the first stage: a preview keeps them first and samples their output.
const FIRST_STAGES: [&str; 13] = [
    "$geoNear",
    "$search",
    "$searchMeta",
    "$vectorSearch",
    "$documents",
    "$collStats",
    "$indexStats",
    "$listSearchIndexes",
    "$planCacheStats",
    "$currentOp",
    "$listSessions",
    "$listLocalSessions",
    "$querySettings",
];
/// Stages that never end (change streams): not previewed.
const STREAM_STAGES: [&str; 2] = ["$changeStream", "$changeStreamSplitLargeEvent"];
/// Stages that read another collection of the same database, with the key that names it.
const LOOKUP_STAGES: [(&str, &str); 3] = [("$lookup", "from"), ("$graphLookup", "from"), ("$unionWith", "coll")];

fn parse_text(text: &str, what: &str) -> AppResult<Json> {
    serde_json::from_str(text.trim()).map_err(|error| AppError::new(format!("{what} is not valid JSON ({error})")))
}

/// The `pipeline` argument as JSON stage documents; `None` when absent. Each stage is a document with one stage
/// operator (a key starting with `$`).
pub fn pipeline_arg(args: &Json) -> AppResult<Option<Vec<Json>>> {
    let items = match args.get("pipeline") {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::String(text)) if text.trim().is_empty() => return Ok(Some(Vec::new())),
        Some(Json::String(text)) => match parse_text(text, "The pipeline")? {
            Json::Array(items) => items,
            _ => return Err(AppError::new("The pipeline must be an array of stages")),
        },
        Some(Json::Array(items)) => items.clone(),
        Some(_) => return Err(AppError::new("The pipeline must be an array of stages")),
    };
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let stage = match item {
                Json::String(text) => parse_text(&text, &format!("Stage {}", index + 1))?,
                other => other,
            };
            match &stage {
                Json::Object(object) if object.len() == 1 && object.keys().all(|key| key.starts_with('$')) => Ok(stage),
                _ => Err(AppError::new(format!(
                    "Stage {} must be a document with one stage operator, e.g. {{ \"$match\": {{}} }}",
                    index + 1
                ))),
            }
        })
        .collect::<AppResult<Vec<_>>>()
        .map(Some)
}

/// The stage operator of a stage document.
fn operator(stage: &Json) -> &str {
    stage
        .as_object()
        .and_then(|object| object.keys().next())
        .map(String::as_str)
        .unwrap_or_default()
}

/// The first `$out` / `$merge` anywhere in the stages (the server allows them last only, this looks everywhere).
pub fn write_stage(stages: &[Json]) -> Option<&'static str> {
    fn find(value: &Json) -> Option<&'static str> {
        match value {
            Json::Object(object) => object
                .iter()
                .find_map(|(key, inner)| WRITE_STAGES.iter().find(|stage| **stage == key).copied().or_else(|| find(inner))),
            Json::Array(items) => items.iter().find_map(find),
            _ => None,
        }
    }
    stages.iter().find_map(find)
}

/// A collection named as a string or as `{ db, coll }`; the database is kept when given (`db.coll`).
fn collection_name(value: &Json, key: &str) -> Option<String> {
    match value {
        Json::String(name) => Some(name.clone()),
        Json::Object(object) => {
            let coll = object.get(key).and_then(Json::as_str)?;
            Some(match object.get("db").and_then(Json::as_str) {
                Some(db) => format!("{db}.{coll}"),
                None => coll.to_owned(),
            })
        }
        _ => None,
    }
}

/// The collections the stages read besides the source: `$lookup` / `$graphLookup` `from`, `$unionWith`, at any depth
/// (a `$lookup` inside a `$facet` or a sub-pipeline).
pub fn read_targets(stages: &[Json]) -> Vec<String> {
    fn walk(value: &Json, found: &mut Vec<String>) {
        match value {
            Json::Object(object) => {
                for (key, inner) in object {
                    if let Some((_, name_key)) = LOOKUP_STAGES.iter().find(|(stage, _)| stage == key) {
                        let name = match (key.as_str(), inner) {
                            ("$unionWith", _) => collection_name(inner, name_key),
                            (_, Json::Object(spec)) => spec.get(*name_key).and_then(|from| collection_name(from, "coll")),
                            _ => None,
                        };
                        if let Some(name) = name.filter(|name| !found.contains(name)) {
                            found.push(name);
                        }
                    }
                    walk(inner, found);
                }
            }
            Json::Array(items) => items.iter().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    stages.iter().for_each(|stage| walk(stage, &mut found));
    found
}

/// The collection a final `$out` / `$merge` writes.
pub fn write_target(stages: &[Json]) -> Option<String> {
    let last = stages.last()?.as_object()?;
    match last.iter().next()? {
        (key, spec) if key == "$out" => collection_name(spec, "coll"),
        (key, Json::String(name)) if key == "$merge" => Some(name.clone()),
        (key, Json::Object(spec)) if key == "$merge" => spec.get("into").and_then(|into| collection_name(into, "coll")),
        _ => None,
    }
}

fn to_bson(stages: &[Json]) -> AppResult<Vec<Document>> {
    stages
        .iter()
        .enumerate()
        .map(|(index, stage)| match Bson::try_from(stage.clone()) {
            Ok(Bson::Document(document)) => Ok(document),
            Ok(_) => Err(AppError::new(format!("Stage {} must be a document", index + 1))),
            Err(error) => Err(AppError::new(format!("Stage {} is not valid Extended JSON ({error})", index + 1))),
        })
        .collect()
}

fn statement(coll: &Collection<Document>, pipeline: &[Document]) -> String {
    let stages: Vec<String> = pipeline.iter().map(values::document_text).collect();
    format!("db.getCollection({}).aggregate([{}])", name_json(coll.name()), stages.join(", "))
}

fn limit_stage(n: u64) -> Document {
    doc! { "$limit": i64::try_from(n).unwrap_or(i64::MAX) }
}

/// A `$count` result: 0 when the stage had no documents.
fn count_of(documents: &[Document]) -> u64 {
    match documents.first().and_then(|document| document.get("n")) {
        Some(Bson::Int32(n)) => u64::try_from(*n).unwrap_or(0),
        Some(Bson::Int64(n)) => u64::try_from(*n).unwrap_or(0),
        Some(Bson::Double(n)) => *n as u64,
        _ => 0,
    }
}

/// One preview run, bounded by what is left of the preview's time.
async fn run(
    coll: &Collection<Document>,
    pipeline: Vec<Document>,
    collation: &Option<Collation>,
    deadline: Instant,
) -> AppResult<Vec<Document>> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(1) {
        return Err(AppError::new(
            "The preview ran out of time (maxTimeMS); use a smaller sample or fewer stages",
        ));
    }
    let mut action = coll.aggregate(pipeline).max_time(left);
    if let Some(collation) = collation {
        action = action.collation(collation.clone());
    }
    Ok(action.await?.try_collect().await?)
}

pub async fn aggregate_preview(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let started = Instant::now();
    let deadline = started + max_time;
    let stages = pipeline_arg(args)?.unwrap_or_default();
    if let Some(stage) = write_stage(&stages) {
        return Err(AppError::new(format!(
            "{stage} writes into a collection, so it is not previewed; preview the stages before it, or run the whole aggregation"
        )));
    }
    if let Some(stage) = stages.iter().map(operator).find(|op| STREAM_STAGES.contains(op)) {
        return Err(AppError::new(format!(
            "{stage} opens a change stream, which a preview cannot sample"
        )));
    }
    let pipeline = to_bson(&stages)?;
    let collation = collation_arg(args)?;
    let size = u64_arg(args, "sampleSize")?.unwrap_or(DEFAULT_SAMPLE).clamp(1, MAX_SAMPLE);
    let limit = u64_arg(args, "limit")?.unwrap_or(DEFAULT_PREVIEW_LIMIT).clamp(1, MAX_PREVIEW_LIMIT);
    let random = match args.get("sampleMode").and_then(Json::as_str) {
        None | Some("first") => false,
        Some("random") => true,
        Some(other) => return Err(AppError::new(format!("sampleMode is \"first\" or \"random\", not \"{other}\""))),
    };
    let up_to = match u64_arg(args, "upTo")? {
        Some(index) => usize::try_from(index).unwrap_or(usize::MAX).min(pipeline.len().saturating_sub(1)),
        None => pipeline.len().saturating_sub(1),
    };
    // A stage that must be first stays first; the sample (first documents) follows it.
    let first_stage = stages.first().map(operator).filter(|op| FIRST_STAGES.contains(op));
    let (mode, input, sample_count) = if first_stage.is_some() {
        ("first", vec![limit_stage(size)], None)
    } else if random {
        let sampled = run(
            coll,
            vec![
                doc! { "$sample": { "size": i64::try_from(size).unwrap_or(i64::MAX) } },
                doc! { "$project": { ID_FIELD: 1 } },
            ],
            &collation,
            deadline,
        )
        .await?;
        let mut ids: Vec<Bson> = Vec::with_capacity(sampled.len());
        for mut document in sampled {
            let Some(id) = document.remove(ID_FIELD) else {
                return Err(AppError::new(
                    "A random sample needs documents with an _id; sample the first documents instead",
                ));
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let count = ids.len() as u64;
        ("random", vec![doc! { "$match": { ID_FIELD: { "$in": ids } } }], Some(count))
    } else {
        let input = vec![limit_stage(size)];
        let mut counting = input.clone();
        counting.push(doc! { "$count": "n" });
        let count = count_of(&run(coll, counting, &collation, deadline).await?);
        ("first", input, Some(count))
    };
    let prefix = |index: usize| -> Vec<Document> {
        let mut prefix = Vec::with_capacity(index + 3);
        let rest = if first_stage.is_some() {
            prefix.push(pipeline[0].clone());
            prefix.extend(input.iter().cloned());
            &pipeline[1..=index]
        } else {
            prefix.extend(input.iter().cloned());
            &pipeline[..=index]
        };
        prefix.extend(rest.iter().cloned());
        prefix
    };
    let mut results = Vec::new();
    for (index, stage) in stages.iter().enumerate().take(up_to + 1) {
        let stage_started = Instant::now();
        let mut counting = prefix(index);
        counting.push(doc! { "$count": "n" });
        let mut reading = prefix(index);
        reading.push(limit_stage(limit));
        let outcome = async {
            let count = count_of(&run(coll, counting, &collation, deadline).await?);
            let documents = run(coll, reading, &collation, deadline).await?;
            AppResult::Ok((count, documents))
        }
        .await;
        let operator = operator(stage);
        match outcome {
            Ok((count, documents)) => results.push(json!({
                "index": index,
                "operator": operator,
                "count": count,
                "elapsedMs": elapsed_ms(stage_started),
                "documents": documents.into_iter().map(relaxed).collect::<Vec<_>>(),
                "error": null,
            })),
            Err(error) => {
                results.push(json!({
                    "index": index,
                    "operator": operator,
                    "count": null,
                    "elapsedMs": elapsed_ms(stage_started),
                    "documents": [],
                    "error": error.message(),
                }));
                break;
            }
        }
    }
    Ok(json!({
        "stages": results,
        "sample": { "mode": mode, "size": size, "count": sample_count },
        "elapsedMs": elapsed_ms(started),
    }))
}

pub async fn aggregate(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let stages = pipeline_arg(args)?.unwrap_or_default();
    let mut pipeline = to_bson(&stages)?;
    let writes = write_stage(&stages);
    let limit = u64_arg(args, "limit")?.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let statement = statement(coll, &pipeline);
    if writes.is_none() {
        pipeline.push(limit_stage(limit + 1));
    }
    let mut action = coll.aggregate(pipeline).max_time(max_time);
    if bool_arg(args, "allowDiskUse") {
        action = action.allow_disk_use(true);
    }
    if let Some(collation) = collation_arg(args)? {
        action = action.collation(collation);
    }
    if let Some(hint) = hint_arg(args)? {
        action = action.hint(hint);
    }
    let started = Instant::now();
    let mut documents: Vec<Document> = action.await?.try_collect().await?;
    let elapsed_ms = elapsed_ms(started);
    let has_more = documents.len() as u64 > limit;
    documents.truncate(limit as usize);
    let wrote = writes.map(|stage| json!({ "stage": stage, "target": write_target(&stages) }));
    Ok(json!({
        "documents": documents.into_iter().map(relaxed).collect::<Vec<_>>(),
        "hasMore": has_more,
        "elapsedMs": elapsed_ms,
        "statement": statement,
        "wrote": wrote,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipelines_parse_from_values_text_stages_or_text() {
        let args = json!({ "pipeline": [{ "$match": { "a": 1 } }, "{ \"$limit\": 5 }"] });
        let stages = pipeline_arg(&args).unwrap().unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(operator(&stages[1]), "$limit");
        let text = json!({ "pipeline": "[{ \"$count\": \"n\" }]" });
        assert_eq!(pipeline_arg(&text).unwrap().unwrap().len(), 1);
        assert!(pipeline_arg(&json!({})).unwrap().is_none());
        assert_eq!(pipeline_arg(&json!({ "pipeline": "" })).unwrap(), Some(Vec::new()));
        for bad in [
            json!({ "pipeline": { "$match": {} } }),
            json!({ "pipeline": [{ "$match": {}, "$limit": 1 }] }),
            json!({ "pipeline": [{ "match": {} }] }),
            json!({ "pipeline": ["{ nope"] }),
            json!({ "pipeline": [3] }),
        ] {
            assert!(pipeline_arg(&bad).is_err(), "{bad}");
        }
        let canonical = to_bson(&[json!({ "$match": { "_id": { "$oid": "65f1a2b3c4d5e6f708192a3b" } } })]).unwrap();
        assert!(matches!(
            canonical[0].get_document("$match").unwrap().get(ID_FIELD),
            Some(Bson::ObjectId(_))
        ));
    }

    #[test]
    fn writes_and_other_collections_are_found() {
        let stages = vec![
            json!({ "$lookup": { "from": "customers", "localField": "c", "foreignField": "_id", "as": "c" } }),
            json!({ "$facet": { "x": [{ "$graphLookup": { "from": "tree", "startWith": "$p", "connectFromField": "p", "connectToField": "_id", "as": "t" } }] } }),
            json!({ "$unionWith": { "coll": "archive", "pipeline": [{ "$unionWith": "old" }] } }),
            json!({ "$out": { "db": "reports", "coll": "daily" } }),
        ];
        assert_eq!(read_targets(&stages), ["customers", "tree", "archive", "old"]);
        assert_eq!(write_stage(&stages), Some("$out"));
        assert_eq!(write_target(&stages).as_deref(), Some("reports.daily"));
        assert_eq!(write_target(&[json!({ "$merge": "totals" })]).as_deref(), Some("totals"));
        assert_eq!(
            write_target(&[json!({ "$merge": { "into": { "db": "r", "coll": "t" }, "whenMatched": "replace" } })]).as_deref(),
            Some("r.t")
        );
        assert_eq!(write_stage(&[json!({ "$match": { "out": 1 } })]), None);
    }

    #[test]
    fn counts_read_any_number_type() {
        assert_eq!(count_of(&[doc! { "n": 3 }]), 3);
        assert_eq!(count_of(&[doc! { "n": 4_i64 }]), 4);
        assert_eq!(count_of(&[]), 0);
    }
}
