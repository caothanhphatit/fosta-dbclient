//! Engine extras (P6): what an engine adds to a profile after the structural walk, through the contract's
//! [`Connection::profile_extras`] hook ([`ProfileExtras`]).
//!
//! - Metadata ([`ProfileExtras::metadata`]) is read like `describe`, as an engine call (Elasticsearch mappings,
//!   aliases and data streams; MongoDB collection options, validators and index specs for stable fingerprints).
//! - Reads of data the engine names ([`ProfileExtras::reads`]: MongoDB `mongo.schema` over a random sample, a Redis
//!   SCAN of key names) go through the gateway as the assistant (`gateway::assistant::invoke_read`: classified, Read
//!   only, audited, time-bounded), bounded per build and in time, and stop at the first refusal. Without a gateway
//!   context nothing is read.
//! - What the answers add ([`ProfileExtras::interpret`]) is applied here: fields replace the described columns
//!   (sensitive names flagged), references become `mongoRef` relations, facts and engine-level knowledge are scrubbed
//!   of secret-looking text. Sampled values become value hints only when the caller may keep them (the user lets the
//!   assistant read data and asked for hints), by the same rules as [`values`](super::values): sensitive names, keys
//!   and unlikely types skipped, secret-looking values dropping the column, formats only for emails, UUIDs and the like.
//!
//! [`Connection::profile_extras`]: crate::contract::Connection::profile_extras

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Map, Value as Json};

use super::{
    build::{Progress, ProgressFn, Step},
    fingerprint,
    model::{ColumnProfile, DatabaseProfile, ObjectProfile, RelationProfile, RelationSource},
    redact,
    values::{hint_of, sampled, Sampler},
    MAX_DETAILED_OBJECTS,
};
use crate::{
    contract::{cancelled, CallContext, CancellationToken, ExtrasRead, FieldShape, ObjectExtras, ObjectRef, ProfileEnrichment, CANCELLED},
    error::{AppError, AppResult},
    gateway,
    runtime::Handle,
};

/// Reads run per build, and how long they may take together.
pub const MAX_READS: usize = 200;
pub const BUDGET: Duration = Duration::from_secs(90);
/// Facts kept per object, and fields per object.
const MAX_FACTS: usize = 20;
const MAX_FIELDS: usize = 500;
/// The size of the engine-level knowledge, as JSON text.
const MAX_EXTRAS_BYTES: usize = 64 * 1024;
const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// What the engine's extras did during a build.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtrasOutcome {
    /// Objects the extras added to or changed.
    pub objects: usize,
    /// Reads run through the gateway, and those that failed.
    pub reads: usize,
    pub failed: usize,
    /// Relations found from field references.
    pub references: usize,
    /// Value hints kept from the extras' samples.
    pub hinted: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// How the extras may read: through the gateway as the assistant (`sampler`), keeping sampled values as hints only
/// with `values`.
pub struct ExtrasReads<'a> {
    pub sampler: &'a Sampler<'a>,
    pub values: bool,
}

fn reference_of(database: &str, object: &ObjectProfile) -> ObjectRef {
    let database = (!database.is_empty()).then_some(database);
    ObjectRef::new(database, object.schema.as_deref(), &object.name, object.kind.clone())
}

fn cancelled_error(error: &AppError, cancel: &CancellationToken) -> bool {
    cancel.is_cancelled() || error.code() == Some(CANCELLED)
}

/// Runs the engine's extras on `profile` (see the module docs). Returns the objects (by index) whose values were
/// sampled here, so value sampling leaves them alone, and what was done; nothing when the engine has no extras.
pub async fn enrich(
    handle: &Handle,
    reads: Option<ExtrasReads<'_>>,
    profile: &mut DatabaseProfile,
    cancel: &CancellationToken,
    progress: ProgressFn<'_>,
) -> AppResult<Option<(ExtrasOutcome, HashSet<usize>)>> {
    if handle.conn.profile_extras().is_none() {
        return Ok(None);
    }
    let mut outcome = ExtrasOutcome::default();
    let database = profile.database.clone();
    let objects: Vec<ObjectRef> = profile.objects.iter().map(|object| reference_of(&database, object)).collect();
    let ctx = CallContext::default().with_cancel(cancel.clone());
    let (name, listed) = (database.clone(), objects.clone());
    let metadata = handle
        .call_cancellable("read profile metadata", cancel.clone(), CANCEL_GRACE, |conn| async move {
            match conn.profile_extras() {
                Some(extras) => extras.metadata(&name, &listed, &ctx).await,
                None => Ok(ProfileEnrichment::default()),
            }
        })
        .await;
    let metadata = match metadata {
        Ok(metadata) => metadata,
        Err(error) if cancelled_error(&error, cancel) => return Err(cancelled()),
        Err(error) => {
            tracing::debug!(%error, "the engine's profile metadata failed");
            outcome.skipped = Some(format!("The engine's metadata could not be read: {error}"));
            ProfileEnrichment::default()
        }
    };
    let keep_values = reads.as_ref().is_some_and(|reads| reads.values);
    let mut state = State::default();
    apply(profile, metadata, false, &mut outcome, &mut state);
    let Some(reads) = reads else {
        // Without the read gate the extras stay metadata only: no document sample, no key names.
        let planned = handle
            .conn
            .profile_extras()
            .map(|extras| extras.reads(&database, &objects).len())
            .unwrap_or(0);
        if planned > 0 && outcome.skipped.is_none() {
            outcome.skipped = Some(NO_READS.to_owned());
        }
        finish(profile);
        return Ok(Some((outcome, state.sampled)));
    };
    let answered = run_reads(handle, reads.sampler, &database, &objects, cancel, progress, &mut outcome).await?;
    if !answered.is_empty() {
        let interpreted = handle
            .conn
            .profile_extras()
            .map(|extras| extras.interpret(&database, &answered))
            .unwrap_or_default();
        apply(profile, interpreted, keep_values, &mut outcome, &mut state);
    }
    finish(profile);
    if keep_values {
        profile.data_read = profile.data_read || outcome.hinted > 0;
    }
    Ok(Some((outcome, state.sampled)))
}

/// The note of a build whose engine would have sampled data, had the assistant been allowed to read it.
pub const NO_READS: &str =
    "Documents and key names were not sampled: the assistant may not read data on this connection, so only metadata was learned";

/// The profile's counts and fingerprint after the extras.
fn finish(profile: &mut DatabaseProfile) {
    profile.fingerprint = fingerprint::database_fingerprint(&profile.objects);
    profile.total_objects = profile.total_objects.max(profile.objects.len());
    profile.detailed_objects = profile.objects.iter().filter(|object| object.detailed).count();
}

/// Runs the engine's reads through the gateway, within [`MAX_READS`] and [`BUDGET`]; a refusal ends them.
async fn run_reads(
    handle: &Handle,
    sampler: &Sampler<'_>,
    database: &str,
    objects: &[ObjectRef],
    cancel: &CancellationToken,
    progress: ProgressFn<'_>,
    outcome: &mut ExtrasOutcome,
) -> AppResult<Vec<(ExtrasRead, Json)>> {
    let Some(extras) = handle.conn.profile_extras() else {
        return Ok(Vec::new());
    };
    let mut planned = extras.reads(database, objects);
    if planned.len() > MAX_READS {
        planned.truncate(MAX_READS);
        outcome.skipped = Some(format!("Only the first {MAX_READS} objects were sampled"));
    }
    let total = planned.len();
    let started = Instant::now();
    let mut answered = Vec::new();
    for (done, read) in planned.into_iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let Some(remaining) = BUDGET.checked_sub(started.elapsed()).filter(|left| !left.is_zero()) else {
            outcome.skipped = Some(format!("Sampling stopped after {} seconds", BUDGET.as_secs()));
            break;
        };
        progress(Progress {
            object: read.object.as_ref().map(|object| object.name.clone()),
            ..Progress::new(Step::Describing, done, total)
        });
        let call = gateway::assistant::invoke_read(sampler.manager, &sampler.context, &read.name, read.args.clone());
        let answer = tokio::select! {
            answer = tokio::time::timeout(remaining, call) => answer,
            _ = cancel.cancelled() => return Err(cancelled()),
        };
        outcome.reads += 1;
        match answer {
            Ok(Ok(value)) => answered.push((read, value)),
            Ok(Err(error)) if gateway::assistant::is_refusal(&error) => {
                outcome.skipped = Some(format!("The workspace's rules refused the assistant's reads: {error}"));
                break;
            }
            Ok(Err(error)) => {
                tracing::debug!(%error, "a profile extras read failed");
                outcome.failed += 1;
            }
            Err(_) => {
                outcome.skipped = Some(format!("Sampling stopped after {} seconds", BUDGET.as_secs()));
                break;
            }
        }
    }
    progress(Progress::new(Step::Describing, total, total));
    Ok(answered)
}

fn column_of(field: &FieldShape) -> ColumnProfile {
    let sensitive = redact::is_sensitive_name(&field.path);
    ColumnProfile {
        name: redact::cut(&field.path, 256),
        type_name: redact::cut(&field.type_name, 64),
        kind: field.kind.clone(),
        nullable: field.nullable,
        primary: field.primary,
        comment: None,
        default: None,
        sensitive,
        presence: field.presence.map(|percent| percent.clamp(0.0, 100.0)),
    }
}

/// A new object the walk did not list (a Redis key pattern).
fn new_object(reference: &ObjectRef) -> ObjectProfile {
    ObjectProfile {
        schema: reference.schema.clone(),
        name: redact::cut(&redact::scrub(&reference.name), 256),
        kind: reference.kind.clone(),
        fingerprint: String::new(),
        detailed: true,
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

fn position(profile: &DatabaseProfile, reference: &ObjectRef) -> Option<usize> {
    let schema = reference.schema.as_deref().filter(|schema| !schema.is_empty());
    profile
        .objects
        .iter()
        .position(|object| object.name == reference.name && object.schema.as_deref() == schema && object.kind == reference.kind)
        .or_else(|| {
            profile
                .objects
                .iter()
                .position(|object| object.name == reference.name && object.schema.as_deref() == schema)
        })
}

/// Applies one object's extras; `based` says (and is set when) its fingerprint rests on a metadata basis. Returns the
/// value hints kept.
fn apply_object(object: &mut ObjectProfile, extras: ObjectExtras, keep_values: bool, based: &mut bool) -> usize {
    let replaced = extras.fields.is_some();
    if let Some(fields) = extras.fields {
        object.columns = fields.iter().take(MAX_FIELDS).map(column_of).collect();
        let primary: Vec<String> = object
            .columns
            .iter()
            .filter(|column| column.primary)
            .map(|column| column.name.clone())
            .collect();
        if !primary.is_empty() {
            object.primary_key = primary;
        }
        object.detailed = true;
        // Hints of columns that are gone go with them.
        let names: HashSet<&str> = object.columns.iter().map(|column| column.name.as_str()).collect();
        object.value_hints.retain(|hint| names.contains(hint.column.as_str()));
    }
    for (key, value) in extras.facts {
        let value = redact::cut(&redact::scrub(&value), 200);
        match object.extra.iter().position(|(known, _)| *known == key) {
            Some(index) => object.extra[index].1 = value,
            None if object.extra.len() < MAX_FACTS => object.extra.push((key, value)),
            None => {}
        }
    }
    if extras.row_estimate.is_some() {
        object.row_estimate = extras.row_estimate;
    }
    let mut hinted = 0;
    if keep_values {
        for sample in extras.samples {
            let Some(column) = object.column(&sample.path).filter(|column| sampled(column)) else {
                continue;
            };
            let Some(hint) = hint_of(column, &sample.values, sample.more) else {
                continue;
            };
            object.value_hints.retain(|known| known.column != hint.column);
            object.value_hints.push(hint);
            hinted += 1;
        }
    }
    match extras.fingerprint_basis {
        Some(basis) => {
            object.fingerprint = fingerprint::basis_fingerprint(object.kind.as_str(), &object.qualified(), &basis);
            *based = true;
        }
        // Fields from a sample never move a fingerprint that rests on a metadata basis.
        None if replaced && !*based => object.fingerprint = fingerprint::of_object(object),
        None => {}
    }
    hinted
}

/// Objects (by index) across the enrichments of one build: those whose values the extras sampled, and those whose
/// fingerprint rests on a metadata basis.
#[derive(Default)]
struct State {
    sampled: HashSet<usize>,
    based: HashSet<usize>,
}

/// Applies one enrichment to `profile`.
fn apply(profile: &mut DatabaseProfile, enrichment: ProfileEnrichment, keep_values: bool, outcome: &mut ExtrasOutcome, state: &mut State) {
    let mut touched = HashSet::new();
    for extras in enrichment.objects {
        let index = match position(profile, &extras.object) {
            Some(index) => index,
            None if profile.objects.len() < MAX_DETAILED_OBJECTS => {
                let mut object = new_object(&extras.object);
                object.fingerprint = fingerprint::of_object(&object);
                profile.objects.push(object);
                profile.objects.len() - 1
            }
            None => {
                profile.truncated = true;
                continue;
            }
        };
        if !extras.samples.is_empty() {
            state.sampled.insert(index);
        }
        let mut based = state.based.contains(&index);
        outcome.hinted += apply_object(&mut profile.objects[index], extras, keep_values, &mut based);
        if based {
            state.based.insert(index);
        }
        touched.insert(index);
    }
    outcome.objects += touched.len();
    let mut seen: HashSet<(String, Vec<String>, String)> = profile
        .relations
        .iter()
        .filter(|relation| relation.source != RelationSource::InferredName)
        .map(|relation| (relation.from.clone(), relation.columns.clone(), relation.to.clone()))
        .collect();
    for reference in enrichment.references {
        let from = profile
            .objects
            .iter()
            .find(|object| object.name == reference.from || object.qualified() == reference.from)
            .map(ObjectProfile::qualified)
            .unwrap_or_else(|| reference.from.clone());
        let relation = RelationProfile {
            name: None,
            columns: vec![reference.path.clone()],
            to: reference.to.clone(),
            to_columns: vec![reference.to_field.clone()],
            to_database: None,
            source: RelationSource::MongoRef,
            confidence: reference.confidence.clamp(0.0, 1.0),
            from,
        };
        if !seen.insert((relation.from.clone(), relation.columns.clone(), relation.to.clone())) {
            continue;
        }
        // A matched reference replaces the guess from the field's name.
        profile.relations.retain(|known| {
            !(known.source == RelationSource::InferredName && known.from == relation.from && known.columns == relation.columns)
        });
        profile.relations.push(relation);
        outcome.references += 1;
    }
    if outcome.skipped.is_none() {
        outcome.skipped = enrichment.note;
    }
    merge_extras(&mut profile.extras, enrichment.extras);
}

/// Engine-level knowledge, scrubbed, within [`MAX_EXTRAS_BYTES`] (keys past the cap are left out).
fn merge_extras(into: &mut Map<String, Json>, from: Map<String, Json>) {
    for (key, value) in from {
        let value = scrubbed(value);
        let size: usize = into.values().map(|value| value.to_string().len()).sum::<usize>() + value.to_string().len();
        if size > MAX_EXTRAS_BYTES {
            continue;
        }
        into.insert(key, value);
    }
}

fn scrubbed(value: Json) -> Json {
    match value {
        Json::String(text) => Json::String(redact::cut(&redact::scrub(&text), 256)),
        Json::Array(items) => Json::Array(items.into_iter().map(scrubbed).collect()),
        Json::Object(map) => Json::Object(
            map.into_iter()
                .map(|(key, value)| (redact::cut(&redact::scrub(&key), 256), scrubbed(value)))
                .collect(),
        ),
        other => other,
    }
}
