//! Grid edits with optimistic concurrency. Elasticsearch has no transactions, so an apply is all-or-nothing in two
//! phases:
//!
//! 1. **Check**: one realtime `_mget` reads every document an update or delete targets and every id an insert would
//!    create. A missing document, a version (`_seq_no:_primary_term` from the page) or original values that no longer
//!    match, or an insert id that exists already stops the apply before anything is written (`CONFLICT` with the
//!    current documents, or an `ENGINE_ERROR` for a duplicate id).
//! 2. **Write**: one `_bulk` (`refresh=wait_for`, so the grid sees the result) with `create` for inserts and
//!    `if_seq_no` / `if_primary_term` from the check on every update and delete; updates send `detect_noop: false` so
//!    the version always moves. A document changed between the two phases fails its item with 409; the items that did
//!    succeed are then undone (best effort, guarded by their new versions) and the apply reports `CONFLICT`.

use std::collections::HashMap;

use reqwest::Method;
use serde_json::{json, Map, Value as Json};

use super::{
    client::{EsBody, EsRequest},
    connection::EsEngine,
    errors,
    grid::{self, Target, ID, INDEX},
    values,
};
use crate::{
    contract::{tabular::conflict_error, ApplyResult, Field, ObjectRef, RowChange, RowChangeSet, RowCheck, RowConflict, Value, ValueKind},
    error::{AppError, AppResult},
};

/// A document as the check read it.
#[derive(Clone, Debug)]
struct Current {
    seq_no: i64,
    primary_term: i64,
    source: Json,
}

/// Where one change goes.
#[derive(Clone, Debug)]
struct Address {
    index: String,
    /// `None` for an insert without a key (the cluster generates the id).
    id: Option<String>,
}

fn key_value<'a>(key: &'a [Field], column: &str) -> Option<&'a Value> {
    key.iter().find(|field| field.column == column).map(|field| &field.value)
}

fn address(target: &Target, change: &RowChange) -> AppResult<Address> {
    let index_from = |key: &[Field]| -> AppResult<String> {
        if target.concrete {
            return Ok(target.name.clone());
        }
        match key_value(key, INDEX) {
            Some(Value::Text(index)) if !index.is_empty() => Ok(index.clone()),
            _ => Err(AppError::new("The row has no _index; reload the grid.")),
        }
    };
    let key_column = target.key.as_ref().map_or(ID, |key| key.name.as_str());
    match change {
        RowChange::Insert { values } => {
            if !target.concrete && (target.name.contains('*') || target.name.contains(',')) {
                return Err(AppError::new(format!(
                    "{} is a pattern; insert documents into one index, alias or data stream.",
                    target.name
                )));
            }
            let id = match key_value(values, key_column) {
                Some(Value::Null) | None if target.key.is_some() => {
                    return Err(AppError::new(format!("Enter the key field {key_column} of the new document.")))
                }
                Some(Value::Null) | None => None,
                Some(value) => Some(target.id_of(value)?),
            };
            Ok(Address {
                index: target.name.clone(),
                id,
            })
        }
        RowChange::Update { key, .. } | RowChange::Delete { key, .. } => {
            let value =
                key_value(key, key_column).ok_or_else(|| AppError::new(format!("The row has no {key_column}; reload the grid.")))?;
            Ok(Address {
                index: index_from(key)?,
                id: Some(target.id_of(value)?),
            })
        }
    }
}

/// The JSON source of an insert, or the partial document of an update, typed by the mapping.
fn source_of(target: &Target, fields: &[Field], insert: bool) -> AppResult<Map<String, Json>> {
    let mut source = Map::new();
    for field in fields {
        if field.column == ID || field.column == INDEX {
            if !insert {
                return Err(AppError::new(format!("{} cannot be changed.", field.column)));
            }
            continue;
        }
        let key = target.key.as_ref().map(|key| key.name.as_str());
        if !insert && Some(field.column.as_str()) == key {
            return Err(AppError::new(format!(
                "{} is the document key and cannot be changed; insert a new document instead.",
                field.column
            )));
        }
        let kind = target
            .column(&field.column)
            .map(|column| column.kind.clone())
            .ok_or_else(|| AppError::new(format!("{} has no field {}.", target.name, field.column)))?;
        source.insert(field.column.clone(), values::to_json(&field.value, &kind)?);
    }
    Ok(source)
}

/// The row as fields, in page column order, for conflict reports.
fn current_fields(target: &Target, address: &Address, current: &Current) -> Vec<Field> {
    let mut fields = vec![Field::new(ID, Value::Text(address.id.clone().unwrap_or_default()))];
    if !target.concrete {
        fields.push(Field::new(INDEX, Value::Text(address.index.clone())));
    }
    for column in &target.columns {
        fields.push(Field::new(
            &column.name,
            values::to_value(&current.source[&column.name], &column.kind),
        ));
    }
    fields
}

/// Whether the current document still has the original values (compared as typed values; numbers numerically).
fn matches_original(target: &Target, original: &[Field], current: &Current) -> bool {
    original.iter().all(|field| {
        if field.column == ID || field.column == INDEX {
            return true;
        }
        let kind = target
            .column(&field.column)
            .map(|column| column.kind.clone())
            .unwrap_or(ValueKind::Json);
        let now = values::to_value(&current.source[&field.column], &kind);
        same(&now, &field.value, &kind)
    })
}

fn same(now: &Value, original: &Value, kind: &ValueKind) -> bool {
    if now == original {
        return true;
    }
    // Compare through the stored JSON so 9.9 and "9.90", or Int and Decimal, agree.
    match (values::to_json(now, kind), values::to_json(original, kind)) {
        (Ok(a), Ok(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => a == b,
        },
        _ => false,
    }
}

fn parse_version(version: &Value) -> Option<(i64, i64)> {
    let text = match version {
        Value::Text(text) | Value::Json(text) => text.as_str(),
        _ => return None,
    };
    let (seq_no, term) = text.trim_matches('"').split_once(':')?;
    Some((seq_no.parse().ok()?, term.parse().ok()?))
}

/// Reads documents in realtime; missing ones are absent from the map.
async fn mget(engine: &EsEngine, addresses: &[&Address]) -> AppResult<HashMap<(String, String), Current>> {
    let docs: Vec<Json> = addresses
        .iter()
        .filter_map(|address| address.id.as_ref().map(|id| json!({ "_index": address.index, "_id": id })))
        .collect();
    let mut found = HashMap::new();
    if docs.is_empty() {
        return Ok(found);
    }
    let response = engine
        .client()
        .call(&EsRequest::new(Method::POST, "/_mget?realtime=true").json(json!({ "docs": docs })))
        .await?;
    for (address, doc) in addresses
        .iter()
        .filter(|address| address.id.is_some())
        .zip(response["docs"].as_array().into_iter().flatten())
    {
        if let Some(error) = doc.get("error") {
            return Err(errors::from_response(400, &json!({ "error": error })));
        }
        if doc["found"] == true {
            found.insert(
                (address.index.clone(), address.id.clone().unwrap_or_default()),
                Current {
                    seq_no: doc["_seq_no"].as_i64().unwrap_or(-1),
                    primary_term: doc["_primary_term"].as_i64().unwrap_or(-1),
                    source: doc["_source"].clone(),
                },
            );
        }
    }
    Ok(found)
}

/// Applies the changes (see the module docs).
pub async fn apply(engine: &EsEngine, changes: &RowChangeSet) -> AppResult<ApplyResult> {
    if changes.changes.is_empty() {
        return Ok(ApplyResult::default());
    }
    let target = grid::resolve(engine, &changes.object.name).await?;
    if target.routing_required {
        return Err(AppError::new(format!(
            "{} requires custom routing, which the grid does not edit; use the console.",
            target.name
        )));
    }
    let addresses = changes
        .changes
        .iter()
        .map(|change| address(&target, change))
        .collect::<AppResult<Vec<_>>>()?;
    let current = mget(engine, &addresses.iter().collect::<Vec<_>>()).await?;
    let lookup = |address: &Address| current.get(&(address.index.clone(), address.id.clone().unwrap_or_default()));

    // Phase 1: check everything before writing anything.
    let mut conflicts = Vec::new();
    for (index, (change, address)) in changes.changes.iter().zip(&addresses).enumerate() {
        match change {
            RowChange::Insert { .. } => {
                if address.id.is_some() && lookup(address).is_some() {
                    return Err(AppError::coded(
                        errors::ENGINE_ERROR,
                        format!(
                            "Nothing was saved: a document with id {} already exists in {}.",
                            address.id.clone().unwrap_or_default(),
                            address.index
                        ),
                    )
                    .with_vendor_code("version_conflict_engine_exception")
                    .with("status", errors::STATUS_CONFLICT));
                }
            }
            RowChange::Update { .. } | RowChange::Delete { .. } => match lookup(address) {
                None => conflicts.push(RowConflict::new(index, change, None)),
                Some(doc) => {
                    let unchanged = match change.check() {
                        RowCheck::KeyOnly => true,
                        RowCheck::Version(version) => parse_version(version) == Some((doc.seq_no, doc.primary_term)),
                        RowCheck::Original(original) => matches_original(&target, original, doc),
                    };
                    if !unchanged {
                        conflicts.push(RowConflict::new(index, change, Some(current_fields(&target, address, doc))));
                    }
                }
            },
        }
    }
    if !conflicts.is_empty() {
        return Err(conflict_error(&changes.object, conflicts));
    }

    // Phase 2: one bulk request, each update and delete guarded by the version the check read.
    let mut lines = Vec::new();
    let mut result = ApplyResult::default();
    for (change, address) in changes.changes.iter().zip(&addresses) {
        let mut meta = json!({ "_index": address.index });
        if let Some(id) = &address.id {
            meta["_id"] = json!(id);
        }
        if let Some(doc) = lookup(address).filter(|_| !matches!(change, RowChange::Insert { .. })) {
            meta["if_seq_no"] = json!(doc.seq_no);
            meta["if_primary_term"] = json!(doc.primary_term);
        }
        match change {
            RowChange::Insert { values } => {
                let mut source = source_of(&target, values, true)?;
                if let (Some(key), Some(id)) = (&target.key, &address.id) {
                    // The key field holds the id; keep its typed value from the insert.
                    if !source.contains_key(&key.name) {
                        source.insert(key.name.clone(), json!(id));
                    }
                }
                lines.push(json!({ "create": meta }));
                lines.push(Json::Object(source));
                result.inserted += 1;
            }
            RowChange::Update { values, .. } => {
                let doc = source_of(&target, values, false)?;
                lines.push(json!({ "update": meta }));
                lines.push(json!({ "doc": doc, "detect_noop": false }));
                result.updated += 1;
            }
            RowChange::Delete { .. } => {
                lines.push(json!({ "delete": meta }));
                result.deleted += 1;
            }
        }
    }
    let mut request = EsRequest::new(Method::POST, "/_bulk?refresh=wait_for");
    request.body = Some(EsBody::NdJson(lines));
    let response = engine.client().call(&request).await?;
    if response["errors"] != true {
        return Ok(result);
    }

    // A document changed between the check and the write: undo what succeeded and report the conflict.
    let items = response["items"].as_array().cloned().unwrap_or_default();
    let mut undo = Vec::new();
    let mut failed = Vec::new();
    for (index, ((change, address), item)) in changes.changes.iter().zip(&addresses).zip(&items).enumerate() {
        let outcome = item
            .as_object()
            .and_then(|item| item.values().next())
            .cloned()
            .unwrap_or(Json::Null);
        let status = outcome["status"].as_u64().unwrap_or(0);
        if status >= 300 {
            failed.push((index, status, outcome));
            continue;
        }
        let guard = json!({
            "_index": outcome["_index"],
            "_id": outcome["_id"],
            "if_seq_no": outcome["_seq_no"],
            "if_primary_term": outcome["_primary_term"],
        });
        match (change, lookup(address)) {
            (RowChange::Insert { .. }, _) => undo.push(json!({ "delete": guard })),
            (RowChange::Update { .. }, Some(before)) => {
                undo.push(json!({ "index": guard }));
                undo.push(before.source.clone());
            }
            (RowChange::Delete { .. }, Some(before)) => {
                undo.push(json!({ "create": { "_index": outcome["_index"], "_id": outcome["_id"] } }));
                undo.push(before.source.clone());
            }
            _ => {}
        }
    }
    let compensated = if undo.is_empty() {
        true
    } else {
        let mut request = EsRequest::new(Method::POST, "/_bulk?refresh=wait_for");
        request.body = Some(EsBody::NdJson(undo));
        matches!(engine.client().call(&request).await, Ok(body) if body["errors"] != true)
    };
    let all_conflicts = failed.iter().all(|(_, status, _)| *status == u64::from(errors::STATUS_CONFLICT));
    if all_conflicts {
        let reread = mget(engine, &failed.iter().map(|(index, _, _)| &addresses[*index]).collect::<Vec<_>>()).await?;
        let conflicts = failed
            .iter()
            .map(|(index, _, _)| {
                let address = &addresses[*index];
                let now = reread
                    .get(&(address.index.clone(), address.id.clone().unwrap_or_default()))
                    .map(|doc| current_fields(&target, address, doc));
                RowConflict::new(*index, &changes.changes[*index], now)
            })
            .collect();
        return Err(with_compensation(
            conflict_error(&changes.object, conflicts),
            compensated,
            &changes.object,
        ));
    }
    let (_, status, outcome) = &failed[0];
    let error = errors::from_response(*status as u16, &json!({ "error": outcome["error"] }));
    Err(with_compensation(error, compensated, &changes.object))
}

fn with_compensation(error: AppError, compensated: bool, object: &ObjectRef) -> AppError {
    if compensated {
        error.with("compensated", true)
    } else {
        error.with("compensated", false).with_detail(format!(
            "Some changes to {} could not be undone; reload the grid to see the current documents.",
            object.display_name()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::NodeKind;

    fn target() -> Target {
        let columns = values::columns_from_mapping(&json!({ "properties": {
            "id": { "type": "long" }, "name": { "type": "keyword" }, "price": { "type": "scaled_float", "scaling_factor": 100 }
        } }));
        Target {
            name: "items".into(),
            concrete: true,
            indices: vec!["items".into()],
            key: columns.iter().find(|c| c.name == "id").cloned(),
            columns,
            routing_required: false,
        }
    }

    #[test]
    fn addresses_come_from_the_declared_key_or_the_index_column() {
        let t = target();
        let insert = RowChange::Insert {
            values: vec![Field::new("id", Value::Int(7)), Field::new("name", Value::Text("x".into()))],
        };
        let address = address(&t, &insert).unwrap();
        assert_eq!((address.index.as_str(), address.id.as_deref()), ("items", Some("7")));
        let keyless = RowChange::Insert {
            values: vec![Field::new("name", Value::Text("x".into()))],
        };
        assert!(super::address(&t, &keyless).is_err(), "a declared key is required");
        let mut alias = t.clone();
        alias.concrete = false;
        alias.name = "items-current".into();
        let delete = RowChange::Delete {
            key: vec![Field::new("_index", Value::Text("items-2".into())), Field::new("id", Value::Int(3))],
            original: vec![],
            version: None,
            force: false,
        };
        let address = super::address(&alias, &delete).unwrap();
        assert_eq!((address.index.as_str(), address.id.as_deref()), ("items-2", Some("3")));
        alias.name = "items-*".into();
        assert!(super::address(&alias, &insert).is_err(), "no inserts into a pattern");
    }

    #[test]
    fn keys_and_meta_fields_cannot_be_updated() {
        let t = target();
        assert!(source_of(&t, &[Field::new("id", Value::Int(1))], false).is_err());
        assert!(source_of(&t, &[Field::new("_id", Value::Text("1".into()))], false).is_err());
        assert!(source_of(&t, &[Field::new("nope", Value::Int(1))], false).is_err());
        let source = source_of(&t, &[Field::new("price", Value::Decimal("9.99".into()))], false).unwrap();
        assert_eq!(source["price"], json!(9.99));
    }

    #[test]
    fn versions_and_original_values_are_compared() {
        assert_eq!(parse_version(&Value::Text("12:1".into())), Some((12, 1)));
        assert_eq!(parse_version(&Value::Int(3)), None);
        let t = target();
        let doc = Current {
            seq_no: 1,
            primary_term: 1,
            source: json!({ "id": 1, "name": "a", "price": 9.9 }),
        };
        assert!(matches_original(&t, &[Field::new("price", Value::Decimal("9.90".into()))], &doc));
        assert!(matches_original(&t, &[Field::new("name", Value::Text("a".into()))], &doc));
        assert!(!matches_original(&t, &[Field::new("name", Value::Text("b".into()))], &doc));
        let address = Address {
            index: "items".into(),
            id: Some("1".into()),
        };
        let fields = current_fields(&t, &address, &doc);
        assert_eq!(fields[0].value, Value::Text("1".into()));
        assert_eq!(fields.len(), 4);
        let _ = ObjectRef::new(None, None, "items", NodeKind::COLLECTION);
    }
}
