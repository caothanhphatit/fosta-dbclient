//! `mongo.schema`: the shape of a collection, analysed over a bounded sample of its documents.
//!
//! `mongo.schema { database, collection, sampleMode?, sampleSize?, filter?, references?, maxTimeMS? }` →
//! `{ sample: { mode, size, count }, paths, truncated, depthCapped, references: { probed, collections, note },
//! elapsedMs }`. Reads only.
//!
//! The sample is the first documents in natural order (`sampleMode` `"first"`, the default), a random sample
//! (`"random"`, `$sample`) or the first documents matching `filter` (`"filtered"`); `sampleSize` defaults to
//! [`DEFAULT_SAMPLE`] and is at most [`MAX_SAMPLE`]. The documents are streamed through [`Analyser`], which keeps per
//! dotted path (recursing into sub-documents and documents in arrays, at most [`MAX_DEPTH`] levels and [`MAX_PATHS`]
//! paths, `truncated` / `depthCapped` say when a cap was met):
//!
//! - `count` / `percent`: documents holding the path (once per document, wherever it occurs in arrays);
//! - `types`: the BSON types of its values (`$type` names), most frequent first; `mixed` when there are two or more
//!   besides null;
//! - `array`: for arrays, the min / average / max length and the types of the elements (the first [`MAX_ELEMENTS`]
//!   elements of each array are analysed; scalar elements count as values of the path, like a query matches them);
//! - `samples`: a few distinct values (strings cut at [`MAX_VALUE_CHARS`]);
//! - `distinct` / `topValues`: the number of distinct values and the most frequent ones with their counts while there
//!   are at most [`DISTINCT_CAP`] (null above that: high cardinality);
//! - `min` / `max`: of numbers and of dates;
//! - `reference`: an ObjectId field whose sampled values are `_id`s of another collection of the same database
//!   (inferred, bounded: at most [`MAX_REF_PATHS`] fields, [`MAX_REF_PROBES`] collections each, [`REF_IDS`] ids per
//!   probe, `system.*` never probed, within the time bound). `references: false` skips the probing.
//!
//! One time bound covers the sampling and the probing; probes that do not fit are skipped (`references.note`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, oid::ObjectId, Bson, Document},
    Collection,
};
use serde_json::{json, Value as Json};

use super::{
    aggregate::{DEFAULT_SAMPLE, MAX_SAMPLE},
    ops::{document_arg, elapsed_ms, u64_arg},
    values::ID_FIELD,
};
use crate::error::{AppError, AppResult};

/// Levels of sub-documents analysed, and the most paths reported.
pub const MAX_DEPTH: usize = 8;
pub const MAX_PATHS: usize = 1000;
/// Elements of each array analysed.
pub const MAX_ELEMENTS: usize = 100;
/// Distinct values tracked per path; above that the path is high-cardinality and has no top values.
pub const DISTINCT_CAP: usize = 100;
/// Top values and sample values reported per path.
const TOP_VALUES: usize = 10;
const SAMPLE_VALUES: usize = 3;
/// Strings in samples and top values are cut at this many characters; longer strings are not tracked as values.
pub const MAX_VALUE_CHARS: usize = 80;
const MAX_TRACKED_CHARS: usize = 200;
/// Reference probing bounds.
pub const MAX_REF_PATHS: usize = 8;
pub const MAX_REF_PROBES: usize = 4;
pub const REF_IDS: usize = 20;
const MAX_COLLECTIONS: usize = 200;
/// People-like field names (before `Id`) that usually point at a users collection.
const USER_ALIASES: [&str; 10] = [
    "owner",
    "author",
    "creator",
    "createdby",
    "updatedby",
    "modifiedby",
    "assignee",
    "member",
    "reviewer",
    "account",
];

/// The `$type` name of a value.
pub fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Symbol(_) => "symbol",
        Bson::Decimal128(_) => "decimal",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "maxKey",
        Bson::MinKey => "minKey",
        Bson::DbPointer(_) => "dbPointer",
    }
}

#[derive(Default)]
struct ArrayStats {
    count: u64,
    min: u64,
    max: u64,
    total: u64,
    element_types: HashMap<&'static str, u64>,
}

struct PathStats {
    path: String,
    parent: Option<usize>,
    depth: usize,
    present: u64,
    /// The document (1-based) that last held the path, so it is counted once per document.
    last_document: u64,
    types: HashMap<&'static str, u64>,
    array: Option<ArrayStats>,
    samples: Vec<Json>,
    sample_keys: Vec<String>,
    /// Distinct values (canonical Extended JSON → shown value, count); None once above [`DISTINCT_CAP`].
    values: Option<HashMap<String, (Json, u64)>>,
    /// Least and greatest number (as f64 for comparing, and the value itself).
    numbers: Option<((f64, Bson), (f64, Bson))>,
    dates: Option<(i64, i64)>,
    object_ids: Vec<ObjectId>,
}

impl PathStats {
    fn new(path: String, parent: Option<usize>, depth: usize) -> Self {
        Self {
            path,
            parent,
            depth,
            present: 0,
            last_document: 0,
            types: HashMap::new(),
            array: None,
            samples: Vec::new(),
            sample_keys: Vec::new(),
            values: Some(HashMap::new()),
            numbers: None,
            dates: None,
            object_ids: Vec::new(),
        }
    }

    fn non_null_types(&self) -> u64 {
        self.types.iter().filter(|(name, _)| **name != "null").map(|(_, n)| n).sum()
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_VALUE_CHARS {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(MAX_VALUE_CHARS - 1).collect();
        cut.push('…');
        cut
    }
}

/// A value as shown in samples and top values: relaxed Extended JSON, strings cut.
fn shown(value: &Bson) -> Json {
    match value {
        Bson::String(text) => Json::String(cut(text)),
        other => other.clone().into_relaxed_extjson(),
    }
}

fn number_of(value: &Bson) -> Option<f64> {
    match value {
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(n) if n.is_finite() => Some(*n),
        Bson::Decimal128(n) => n.to_string().parse().ok().filter(|n: &f64| n.is_finite()),
        _ => None,
    }
}

fn sorted_types(types: &HashMap<&'static str, u64>) -> Vec<Json> {
    let mut list: Vec<(&&str, &u64)> = types.iter().collect();
    list.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    list.into_iter()
        .map(|(name, count)| json!({ "type": name, "count": count }))
        .collect()
}

/// The schema of documents fed one by one (`add`), bounded in paths, depth and values kept.
#[derive(Default)]
pub struct Analyser {
    paths: Vec<PathStats>,
    index: HashMap<String, usize>,
    documents: u64,
    truncated: bool,
    depth_capped: bool,
}

impl Analyser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn documents(&self) -> u64 {
        self.documents
    }

    pub fn add(&mut self, document: &Document) {
        self.documents += 1;
        self.walk(document, "", None, 0);
    }

    fn walk(&mut self, document: &Document, prefix: &str, parent: Option<usize>, depth: usize) {
        if depth >= MAX_DEPTH {
            self.depth_capped = true;
            return;
        }
        for (key, value) in document {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            if let Some(slot) = self.slot(path, parent, depth) {
                self.record(slot, value, depth);
            }
        }
    }

    fn slot(&mut self, path: String, parent: Option<usize>, depth: usize) -> Option<usize> {
        if let Some(slot) = self.index.get(&path) {
            return Some(*slot);
        }
        if self.paths.len() >= MAX_PATHS {
            self.truncated = true;
            return None;
        }
        let slot = self.paths.len();
        self.index.insert(path.clone(), slot);
        self.paths.push(PathStats::new(path, parent, depth));
        Some(slot)
    }

    fn record(&mut self, slot: usize, value: &Bson, depth: usize) {
        let documents = self.documents;
        let stats = &mut self.paths[slot];
        if stats.last_document != documents {
            stats.last_document = documents;
            stats.present += 1;
        }
        *stats.types.entry(type_name(value)).or_default() += 1;
        match value {
            Bson::Document(inner) => {
                let path = stats.path.clone();
                self.walk(inner, &path, Some(slot), depth + 1);
            }
            Bson::Array(items) => {
                let length = items.len() as u64;
                let array = stats.array.get_or_insert_with(|| ArrayStats {
                    min: u64::MAX,
                    ..ArrayStats::default()
                });
                array.count += 1;
                array.total += length;
                array.min = array.min.min(length);
                array.max = array.max.max(length);
                for item in items.iter().take(MAX_ELEMENTS) {
                    *array.element_types.entry(type_name(item)).or_default() += 1;
                }
                let path = stats.path.clone();
                for item in items.iter().take(MAX_ELEMENTS) {
                    match item {
                        Bson::Document(inner) => self.walk(inner, &path, Some(slot), depth + 1),
                        Bson::Array(_) => {}
                        scalar => self.scalar(slot, scalar),
                    }
                }
            }
            scalar => self.scalar(slot, scalar),
        }
    }

    fn scalar(&mut self, slot: usize, value: &Bson) {
        let stats = &mut self.paths[slot];
        if matches!(value, Bson::Null | Bson::Undefined) {
            return;
        }
        let tracked = matches!(
            value,
            Bson::String(_)
                | Bson::Boolean(_)
                | Bson::Int32(_)
                | Bson::Int64(_)
                | Bson::Double(_)
                | Bson::Decimal128(_)
                | Bson::ObjectId(_)
                | Bson::DateTime(_)
        );
        let key = tracked.then(|| value.clone().into_canonical_extjson().to_string());
        if stats.samples.len() < SAMPLE_VALUES {
            let sample_key = key.clone().unwrap_or_else(|| value.clone().into_canonical_extjson().to_string());
            if !stats.sample_keys.contains(&sample_key) {
                stats.sample_keys.push(sample_key);
                stats.samples.push(shown(value));
            }
        }
        if let (Some(key), Some(values)) = (key, stats.values.as_mut()) {
            let too_long = matches!(value, Bson::String(text) if text.chars().count() > MAX_TRACKED_CHARS);
            if too_long {
                stats.values = None;
            } else if let Some(entry) = values.get_mut(&key) {
                entry.1 += 1;
            } else if values.len() >= DISTINCT_CAP {
                stats.values = None;
            } else {
                values.insert(key, (shown(value), 1));
            }
        }
        if let Some(n) = number_of(value) {
            let numbers = stats.numbers.get_or_insert_with(|| ((n, value.clone()), (n, value.clone())));
            if n < numbers.0 .0 {
                numbers.0 = (n, value.clone());
            }
            if n > numbers.1 .0 {
                numbers.1 = (n, value.clone());
            }
        }
        match value {
            Bson::DateTime(date) => {
                let millis = date.timestamp_millis();
                let dates = stats.dates.get_or_insert((millis, millis));
                dates.0 = dates.0.min(millis);
                dates.1 = dates.1.max(millis);
            }
            Bson::ObjectId(id) if stats.object_ids.len() < REF_IDS && !stats.object_ids.contains(id) => {
                stats.object_ids.push(*id);
            }
            _ => {}
        }
    }

    /// Paths that may reference another collection: mostly ObjectIds (not `_id` itself), names like references first.
    pub fn reference_candidates(&self) -> Vec<(String, Vec<ObjectId>)> {
        let mut candidates: Vec<&PathStats> = self
            .paths
            .iter()
            .filter(|stats| stats.path != ID_FIELD && !stats.object_ids.is_empty())
            .filter(|stats| {
                let ids = stats.types.get("objectId").copied().unwrap_or(0)
                    + stats
                        .array
                        .as_ref()
                        .and_then(|array| array.element_types.get("objectId"))
                        .copied()
                        .unwrap_or(0);
                ids * 2 >= stats.non_null_types().max(1)
            })
            .collect();
        candidates.sort_by_key(|stats| !is_ref_name(&stats.path));
        candidates
            .into_iter()
            .take(MAX_REF_PATHS)
            .map(|stats| (stats.path.clone(), stats.object_ids.clone()))
            .collect()
    }

    /// The paths in tree order (each path followed by its children, in first-seen order).
    pub fn report(&self, references: &HashMap<String, Json>) -> Vec<Json> {
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); self.paths.len()];
        let mut roots = Vec::new();
        for (slot, stats) in self.paths.iter().enumerate() {
            match stats.parent {
                Some(parent) => children[parent].push(slot),
                None => roots.push(slot),
            }
        }
        let mut order = Vec::with_capacity(self.paths.len());
        let mut stack: Vec<usize> = roots.into_iter().rev().collect();
        while let Some(slot) = stack.pop() {
            order.push(slot);
            stack.extend(children[slot].iter().rev());
        }
        let documents = self.documents.max(1) as f64;
        order
            .into_iter()
            .map(|slot| {
                let stats = &self.paths[slot];
                let name = stats.path.rsplit('.').next().unwrap_or_default();
                let non_null = stats.types.keys().filter(|name| **name != "null").count();
                let array = stats.array.as_ref().map(|array| {
                    json!({
                        "count": array.count,
                        "minLength": array.min,
                        "avgLength": (array.total as f64 / array.count.max(1) as f64 * 10.0).round() / 10.0,
                        "maxLength": array.max,
                        "elementTypes": sorted_types(&array.element_types),
                    })
                });
                let (distinct, top) = match &stats.values {
                    Some(values) => {
                        let mut list: Vec<(&String, &(Json, u64))> = values.iter().collect();
                        list.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(a.0.cmp(b.0)));
                        let top: Vec<Json> = list
                            .into_iter()
                            .take(TOP_VALUES)
                            .map(|(_, (value, count))| json!({ "value": value, "count": count }))
                            .collect();
                        (Some(values.len()), (!top.is_empty()).then_some(top))
                    }
                    None => (None, None),
                };
                let (min, max) = match (&stats.numbers, &stats.dates) {
                    (Some((low, high)), _) => (
                        Some(low.1.clone().into_relaxed_extjson()),
                        Some(high.1.clone().into_relaxed_extjson()),
                    ),
                    (None, Some((low, high))) => (
                        Some(Bson::DateTime(mongodb::bson::DateTime::from_millis(*low)).into_relaxed_extjson()),
                        Some(Bson::DateTime(mongodb::bson::DateTime::from_millis(*high)).into_relaxed_extjson()),
                    ),
                    _ => (None, None),
                };
                json!({
                    "path": stats.path,
                    "name": name,
                    "depth": stats.depth,
                    "count": stats.present,
                    "percent": (stats.present as f64 / documents * 1000.0).round() / 10.0,
                    "types": sorted_types(&stats.types),
                    "mixed": non_null > 1,
                    "array": array,
                    "samples": stats.samples,
                    "distinct": distinct,
                    "topValues": top,
                    "min": min,
                    "max": max,
                    "reference": references.get(&stats.path).cloned(),
                })
            })
            .collect()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn depth_capped(&self) -> bool {
        self.depth_capped
    }
}

/// A field name like `customerId`, `customer_id`, `tagIds`.
fn is_ref_name(path: &str) -> bool {
    ref_base(path.rsplit('.').next().unwrap_or_default()).is_some()
}

fn ref_base(name: &str) -> Option<&str> {
    ["_ids", "_id", "Ids", "Id", "IDs", "ID"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .filter(|base| !base.is_empty())
}

fn normal(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

fn plurals(word: &str) -> Vec<String> {
    let mut forms = vec![word.to_owned(), format!("{word}s"), format!("{word}es")];
    if let Some(stem) = word.strip_suffix('y') {
        if !stem.ends_with(['a', 'e', 'i', 'o', 'u']) {
            forms.push(format!("{stem}ies"));
        }
    }
    forms
}

/// The collections to probe for `path`: those named after the field (`customerId` → `customers`), then the others
/// (never `self` unless its name matches, never `system.*`), at most [`MAX_REF_PROBES`].
pub fn probe_order(path: &str, collections: &[String], this: &str) -> Vec<String> {
    let name = path.rsplit('.').next().unwrap_or_default();
    let mut wanted: Vec<String> = Vec::new();
    if let Some(base) = ref_base(name) {
        wanted.extend(plurals(&normal(base)));
        // `parentCategoryId` → categories, `billing_address_id` → addresses; people-like names → users.
        let mut words: Vec<String> = Vec::new();
        for c in base.chars() {
            if matches!(c, '_' | '-') {
                words.push(String::new());
                continue;
            }
            if c.is_uppercase() || words.is_empty() {
                words.push(String::new());
            }
            if let Some(word) = words.last_mut() {
                word.push(c);
            }
        }
        words.retain(|word| !word.is_empty());
        if let (true, Some(last)) = (words.len() > 1, words.last().filter(|word| word.len() >= 3)) {
            wanted.extend(plurals(&normal(last)));
        }
        if USER_ALIASES.contains(&normal(base).as_str()) {
            wanted.extend(plurals("user"));
        }
    }
    let usable: Vec<&String> = collections.iter().filter(|name| !name.starts_with("system.")).collect();
    let mut order: Vec<String> = usable
        .iter()
        .filter(|name| {
            let name = normal(name);
            wanted.iter().any(|form| name == *form || name.ends_with(form.as_str()))
        })
        .map(|name| (*name).clone())
        .collect();
    let mut others: Vec<String> = usable
        .iter()
        .filter(|name| name.as_str() != this && !order.contains(name))
        .map(|name| (*name).clone())
        .collect();
    others.sort();
    order.extend(others);
    order.truncate(MAX_REF_PROBES);
    order
}

fn sample_pipeline(args: &Json) -> AppResult<(&'static str, u64, Vec<Document>)> {
    let size = u64_arg(args, "sampleSize")?.unwrap_or(DEFAULT_SAMPLE).clamp(1, MAX_SAMPLE);
    let limit = i64::try_from(size).unwrap_or(i64::MAX);
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    Ok(match args.get("sampleMode").and_then(Json::as_str) {
        None | Some("first") => ("first", size, vec![doc! { "$limit": limit }]),
        Some("random") => ("random", size, vec![doc! { "$sample": { "size": limit } }]),
        Some("filtered") => ("filtered", size, vec![doc! { "$match": filter }, doc! { "$limit": limit }]),
        Some(other) => {
            return Err(AppError::new(format!(
                "sampleMode is \"first\", \"random\" or \"filtered\", not \"{other}\""
            )))
        }
    })
}

/// Probes the reference candidates within the deadline: `(path → { collection, matched, probed, inferred })`, the
/// collections listed, and a note when probing was cut short.
async fn probe_references(
    coll: &Collection<Document>,
    candidates: Vec<(String, Vec<ObjectId>)>,
    deadline: Instant,
) -> (HashMap<String, Json>, usize, Option<String>) {
    let mut found = HashMap::new();
    if candidates.is_empty() {
        return (found, 0, None);
    }
    let database = coll.client().database(&coll.namespace().db);
    let left = deadline.saturating_duration_since(Instant::now());
    let listed = tokio::time::timeout(left, database.list_collection_names()).await;
    let mut collections = match listed {
        Ok(Ok(names)) => names,
        Ok(Err(error)) => return (found, 0, Some(format!("The collections could not be listed ({error})"))),
        Err(_) => return (found, 0, Some("Out of time before probing references".to_owned())),
    };
    collections.truncate(MAX_COLLECTIONS);
    let mut note = None;
    'paths: for (path, ids) in candidates {
        let ids: Vec<Bson> = ids.into_iter().map(Bson::ObjectId).collect();
        let mut best: Option<(String, u64)> = None;
        for target in probe_order(&path, &collections, coll.name()) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left < Duration::from_millis(5) {
                note = Some("Out of time: some references were not probed".to_owned());
                break 'paths;
            }
            let matched = database
                .collection::<Document>(&target)
                .count_documents(doc! { ID_FIELD: { "$in": ids.clone() } })
                .max_time(left)
                .await;
            // A collection that cannot be read (no rights, a view that fails) is not the target.
            if let Ok(matched) = matched {
                if matched > 0 && best.as_ref().is_none_or(|(_, n)| matched > *n) {
                    best = Some((target, matched));
                }
                if matched == ids.len() as u64 {
                    break;
                }
            }
        }
        if let Some((collection, matched)) = best {
            found.insert(
                path,
                json!({ "collection": collection, "matched": matched, "probed": ids.len(), "inferred": true }),
            );
        }
    }
    (found, collections.len(), note)
}

pub async fn schema(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let started = Instant::now();
    let deadline = started + max_time;
    let (mode, size, pipeline) = sample_pipeline(args)?;
    let mut cursor = coll.aggregate(pipeline).max_time(max_time).await?;
    let mut analyser = Analyser::new();
    while let Some(document) = cursor.try_next().await? {
        analyser.add(&document);
    }
    let probe = args.get("references").and_then(Json::as_bool).unwrap_or(true);
    let (references, listed, note) = if probe {
        probe_references(coll, analyser.reference_candidates(), deadline).await
    } else {
        (HashMap::new(), 0, None)
    };
    Ok(json!({
        "sample": { "mode": mode, "size": size, "count": analyser.documents() },
        "paths": analyser.report(&references),
        "truncated": analyser.truncated(),
        "depthCapped": analyser.depth_capped(),
        "references": { "probed": probe, "collections": listed, "note": note },
        "elapsedMs": elapsed_ms(started),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyse(documents: &[Document]) -> (Analyser, Vec<Json>) {
        let mut analyser = Analyser::new();
        for document in documents {
            analyser.add(document);
        }
        let report = analyser.report(&HashMap::new());
        (analyser, report)
    }

    fn path<'a>(report: &'a [Json], name: &str) -> &'a Json {
        report
            .iter()
            .find(|entry| entry["path"] == name)
            .unwrap_or_else(|| panic!("{name}"))
    }

    #[test]
    fn presence_types_and_mixed() {
        let (_, report) = analyse(&[
            doc! { "_id": 1, "name": "a", "age": 30 },
            doc! { "_id": 2, "name": "b", "age": "thirty" },
            doc! { "_id": 3, "name": null },
            doc! { "_id": 4 },
        ]);
        let name = path(&report, "name");
        assert_eq!(name["count"], 3);
        assert_eq!(name["percent"], 75.0);
        assert_eq!(
            name["types"],
            json!([{ "type": "string", "count": 2 }, { "type": "null", "count": 1 }])
        );
        assert_eq!(name["mixed"], false);
        let age = path(&report, "age");
        assert_eq!(age["mixed"], true);
        assert_eq!(age["percent"], 50.0);
        assert_eq!(age["min"], 30);
        assert_eq!(report[0]["path"], "_id");
    }

    #[test]
    fn nested_documents_and_arrays_of_documents_in_tree_order() {
        let (_, report) = analyse(&[
            doc! { "a": { "x": 1 }, "items": [{ "sku": "p1", "qty": 2 }, { "sku": "p2", "qty": 1 }], "tags": ["red", "blue"] },
            doc! { "b": true, "a": { "y": 2 }, "items": [], "tags": ["red"] },
        ]);
        let order: Vec<&str> = report.iter().map(|entry| entry["path"].as_str().unwrap()).collect();
        assert_eq!(order, ["a", "a.x", "a.y", "items", "items.sku", "items.qty", "tags", "b"]);
        let sku = path(&report, "items.sku");
        assert_eq!(sku["count"], 1, "counted once per document");
        assert_eq!(sku["depth"], 1);
        assert_eq!(sku["distinct"], 2);
        let items = path(&report, "items");
        assert_eq!(items["array"]["minLength"], 0);
        assert_eq!(items["array"]["maxLength"], 2);
        assert_eq!(items["array"]["avgLength"], 1.0);
        assert_eq!(items["array"]["elementTypes"], json!([{ "type": "object", "count": 2 }]));
        let tags = path(&report, "tags");
        assert_eq!(tags["topValues"][0], json!({ "value": "red", "count": 2 }));
        assert_eq!(tags["array"]["elementTypes"], json!([{ "type": "string", "count": 3 }]));
    }

    #[test]
    fn cardinality_is_capped_and_values_are_cut() {
        let documents: Vec<Document> = (0..(DISTINCT_CAP as i32 + 5))
            .map(|i| doc! { "n": i, "status": if i % 2 == 0 { "paid" } else { "open" }, "long": "x".repeat(300) })
            .collect();
        let (_, report) = analyse(&documents);
        let n = path(&report, "n");
        assert_eq!(n["distinct"], Json::Null);
        assert_eq!(n["topValues"], Json::Null);
        assert_eq!(n["min"], 0);
        assert_eq!(n["max"], DISTINCT_CAP as i32 + 4);
        let status = path(&report, "status");
        assert_eq!(status["distinct"], 2);
        assert_eq!(status["topValues"][0]["value"], "paid");
        let long = path(&report, "long");
        assert_eq!(long["distinct"], Json::Null);
        assert_eq!(long["samples"][0].as_str().unwrap().chars().count(), MAX_VALUE_CHARS);
    }

    #[test]
    fn dates_min_max_and_samples() {
        let early = mongodb::bson::DateTime::from_millis(1_000);
        let late = mongodb::bson::DateTime::from_millis(2_000_000_000_000);
        let (_, report) = analyse(&[doc! { "at": late }, doc! { "at": early }, doc! { "at": late }]);
        let at = path(&report, "at");
        assert_eq!(at["min"], Bson::DateTime(early).into_relaxed_extjson());
        assert_eq!(at["max"], Bson::DateTime(late).into_relaxed_extjson());
        assert_eq!(at["samples"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn depth_and_path_caps() {
        let mut deep = doc! { "leaf": 1 };
        for level in 0..MAX_DEPTH + 2 {
            deep = doc! { format!("l{level}"): deep };
        }
        let (analyser, report) = analyse(&[deep]);
        assert!(analyser.depth_capped());
        assert_eq!(report.len(), MAX_DEPTH);
        let wide: Document = (0..MAX_PATHS + 10).map(|i| (format!("f{i}"), Bson::Int32(1))).collect();
        let (analyser, report) = analyse(&[wide]);
        assert!(analyser.truncated());
        assert_eq!(report.len(), MAX_PATHS);
    }

    #[test]
    fn reference_candidates_and_probe_order() {
        let customer = ObjectId::new();
        let (analyser, _) = analyse(&[
            doc! { "_id": ObjectId::new(), "customerId": customer, "tagIds": [ObjectId::new()], "owner": ObjectId::new(), "n": 1 },
            doc! { "_id": ObjectId::new(), "customerId": customer, "owner": "nobody" },
        ]);
        let candidates: Vec<String> = analyser.reference_candidates().into_iter().map(|(path, _)| path).collect();
        assert_eq!(candidates, ["customerId", "tagIds", "owner"]);
        assert_eq!(analyser.reference_candidates()[0].1, [customer]);
        let collections: Vec<String> = ["orders", "customers", "system.users", "tags", "audit", "zeta", "beta"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            probe_order("customerId", &collections, "orders"),
            ["customers", "audit", "beta", "tags"]
        );
        assert_eq!(
            probe_order("items.tagIds", &collections, "orders"),
            ["tags", "audit", "beta", "customers"]
        );
        assert_eq!(
            probe_order("parentCategoryId", &["categories".to_owned(), "a".to_owned()], "x"),
            ["categories", "a"]
        );
        assert_eq!(
            probe_order("customerId", &["a".to_owned(), "shop_customers".to_owned()], "x"),
            ["shop_customers", "a"]
        );
        assert_eq!(probe_order("ownerId", &["a".to_owned(), "users".to_owned()], "x"), ["users", "a"]);
    }

    #[test]
    fn sample_modes() {
        assert_eq!(sample_pipeline(&json!({})).unwrap().0, "first");
        let (mode, size, pipeline) = sample_pipeline(&json!({ "sampleMode": "random", "sampleSize": 50_000 })).unwrap();
        assert_eq!((mode, size), ("random", MAX_SAMPLE));
        assert!(pipeline[0].contains_key("$sample"));
        let (mode, _, pipeline) =
            sample_pipeline(&json!({ "sampleMode": "filtered", "filter": "{ \"a\": 1 }", "sampleSize": 10 })).unwrap();
        assert_eq!(mode, "filtered");
        assert_eq!(pipeline[0], doc! { "$match": { "a": 1 } });
        assert!(sample_pipeline(&json!({ "sampleMode": "all" })).is_err());
    }
}
