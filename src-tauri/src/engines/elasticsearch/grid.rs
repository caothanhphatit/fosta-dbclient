//! Browsing documents as rows: the target's mapping gives the columns, structured filters become Query DSL clauses
//! under `bool.filter`, and paging uses `from` / `size` for small sets and `search_after` (with a point in time when
//! there is no declared key) beyond them.
//!
//! **Keys.** `_id` cannot be sorted (Elasticsearch 8 has no field data for it) or ranged, so an index may declare the
//! field that holds its document id in the mapping: `"_meta": { "fosta": { "key": "id" } }`. The document `_id` is then
//! that field's value as text, the field is the key column (sortable, rangeable, the `search_after` tiebreaker) and
//! `_id` is shown read-only. Without a declaration the key is `_id` (equality filters only). A target that is not one
//! concrete index (an alias, a data stream, a pattern) adds `_index` to the key, so edits go to the document's own
//! backing index.
//!
//! **Paging token.** `Page::next_keyset` is one JSON value: `{ "from": n }` (small sets), `{ "after": [...] }`
//! (`search_after` on the sort with the declared key as tiebreaker) or `{ "pit": id, "after" | "from": … }` (point in
//! time with `_shard_doc` as tiebreaker, opened when a set without a declared key outgrows `from` / `size`).

use std::time::Instant;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};

use super::{
    client::EsRequest,
    connection::{segment, EsEngine},
    dsl,
    search::MAX_RESULT_WINDOW,
    values::{self, FieldColumn},
};
use crate::{
    contract::{
        BrowseRequest, ColumnHints, ColumnInfo, Filter, FilterCond, FilterOp, Page, RawFilterLang, SortKey, Total, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

pub const ID: &str = "_id";
pub const INDEX: &str = "_index";
/// How long a point in time opened for paging stays open after each page.
const PIT_KEEP_ALIVE: &str = "5m";

/// What a browse or apply works on.
#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    /// The name is one concrete index (not an alias, data stream or pattern).
    pub concrete: bool,
    pub indices: Vec<String>,
    pub columns: Vec<FieldColumn>,
    /// The declared key field (`_meta.fosta.key`).
    pub key: Option<FieldColumn>,
    /// `_routing.required`: documents cannot be addressed by id alone.
    pub routing_required: bool,
}

impl Target {
    pub fn column(&self, name: &str) -> Option<&FieldColumn> {
        self.columns.iter().find(|column| column.name == name)
    }

    pub fn key_columns(&self) -> Vec<String> {
        let mut keys = Vec::new();
        if !self.concrete {
            keys.push(INDEX.to_owned());
        }
        keys.push(self.key.as_ref().map_or_else(|| ID.to_owned(), |key| key.name.clone()));
        keys
    }

    /// Every column of a page: `_id`, `_index` for non-concrete targets, then the mapping's fields.
    pub fn column_infos(&self) -> Vec<ColumnInfo> {
        let mut infos = Vec::new();
        let mut id = ColumnInfo::result(ID, "_id", ValueKind::Text);
        id.nullable = false;
        id.hints = ColumnHints {
            primary: self.key.is_none(),
            read_only: self.key.is_some(),
            ..ColumnHints::default()
        };
        infos.push(id);
        if !self.concrete {
            let mut index = ColumnInfo::result(INDEX, "_index", ValueKind::Text);
            index.nullable = false;
            index.hints = ColumnHints {
                primary: true,
                read_only: true,
                ..ColumnHints::default()
            };
            infos.push(index);
        }
        let key = self.key.as_ref().map(|key| key.name.as_str());
        infos.extend(self.columns.iter().map(|column| column.info(Some(column.name.as_str()) == key)));
        infos
    }

    /// The `_id` for a key value (the declared key's text form).
    pub fn id_of(&self, value: &Value) -> AppResult<String> {
        let id = match value {
            Value::Text(text) | Value::Decimal(text) => text.clone(),
            Value::Int(i) => i.to_string(),
            Value::Null => return Err(AppError::new("The document key is empty.")),
            other => other.display().ok_or_else(|| AppError::new("The document key is empty."))?,
        };
        if id.is_empty() || id.len() > 512 {
            return Err(AppError::new("A document id must be 1 to 512 characters long."));
        }
        Ok(id)
    }
}

/// Reads the mapping of `name` and works out its columns and key.
pub async fn resolve(engine: &EsEngine, name: &str) -> AppResult<Target> {
    if name.trim().is_empty() || name == "." || name == ".." {
        return Err(AppError::new("Pick an index, alias or data stream."));
    }
    let body = engine.client().get(&format!("/{}/_mapping", segment(name))).await?;
    let Some(indices) = body.as_object().filter(|indices| !indices.is_empty()) else {
        return Err(AppError::new(format!("{name} matches no index.")));
    };
    let concrete = indices.len() == 1 && indices.contains_key(name);
    let mut columns: Vec<FieldColumn> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    let mut routing_required = false;
    for entry in indices.values() {
        let mappings = &entry["mappings"];
        for column in values::columns_from_mapping(mappings) {
            if !columns.iter().any(|known| known.name == column.name) {
                columns.push(column);
            }
        }
        keys.push(mappings["_meta"]["fosta"]["key"].as_str().map(str::to_owned));
        routing_required |= mappings["_routing"]["required"].as_bool().unwrap_or(false);
    }
    // The key counts only when every index declares the same one and it is a scalar field.
    let declared = keys
        .first()
        .cloned()
        .flatten()
        .filter(|key| keys.iter().all(|other| other.as_deref() == Some(key)));
    let key = declared
        .and_then(|key| columns.iter().find(|column| column.name == key).cloned())
        .filter(|column| matches!(column.kind, ValueKind::Int | ValueKind::Text) && column.exact.is_some());
    Ok(Target {
        name: name.to_owned(),
        concrete,
        indices: indices.keys().cloned().collect(),
        columns,
        key,
        routing_required,
    })
}

/// Escapes `*`, `?` and `\` for a wildcard pattern.
fn escape_wildcard(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '?' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A SQL LIKE pattern as a wildcard pattern: `%` → `*`, `_` → `?`.
fn like_to_wildcard(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '%' => out.push('*'),
            '_' => out.push('?'),
            '*' | '?' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

fn not(clause: Json) -> Json {
    json!({ "bool": { "must_not": [clause] } })
}

/// One structured condition as a Query DSL clause.
pub fn condition_clause(target: &Target, condition: &FilterCond) -> AppResult<Json> {
    condition.validate()?;
    let values = &condition.values;
    if condition.column == ID {
        let ids = || json!({ "ids": { "values": values } });
        return Ok(match condition.op {
            FilterOp::Eq | FilterOp::In => ids(),
            FilterOp::Ne | FilterOp::NotIn => not(ids()),
            FilterOp::IsNotNull => json!({ "match_all": {} }),
            FilterOp::IsNull => json!({ "match_none": {} }),
            _ => {
                return Err(AppError::new(
                    "_id can only be compared with =, ≠, in and not in. Declare a key field to range or sort by it.",
                ))
            }
        });
    }
    let (field, kind, exact) = if condition.column == INDEX {
        (INDEX.to_owned(), ValueKind::Text, Some(INDEX.to_owned()))
    } else {
        let column = target
            .column(&condition.column)
            .ok_or_else(|| AppError::new(format!("{} has no field {}.", target.name, condition.column)))?;
        (column.name.clone(), column.kind.clone(), column.exact.clone())
    };
    let typed = |text: &str| values::filter_value(text, &kind);
    let exact_field = || {
        exact.clone().ok_or_else(|| {
            AppError::new(format!(
                "{field} is analysed text without a keyword sub-field; use a Query DSL filter (match)."
            ))
        })
    };
    let pattern = |value: String, case_insensitive: bool| -> AppResult<Json> {
        Ok(json!({ "wildcard": { exact_field()?: { "value": value, "case_insensitive": case_insensitive } } }))
    };
    Ok(match condition.op {
        FilterOp::Eq => json!({ "term": { exact_field()?: typed(&values[0])? } }),
        FilterOp::Ne => not(json!({ "term": { exact_field()?: typed(&values[0])? } })),
        FilterOp::Lt => json!({ "range": { field: { "lt": typed(&values[0])? } } }),
        FilterOp::Le => json!({ "range": { field: { "lte": typed(&values[0])? } } }),
        FilterOp::Gt => json!({ "range": { field: { "gt": typed(&values[0])? } } }),
        FilterOp::Ge => json!({ "range": { field: { "gte": typed(&values[0])? } } }),
        FilterOp::Between => json!({ "range": { field: { "gte": typed(&values[0])?, "lte": typed(&values[1])? } } }),
        FilterOp::Contains => pattern(format!("*{}*", escape_wildcard(&values[0])), true)?,
        FilterOp::NotContains => not(pattern(format!("*{}*", escape_wildcard(&values[0])), true)?),
        FilterOp::StartsWith => json!({ "prefix": { exact_field()?: { "value": values[0], "case_insensitive": true } } }),
        FilterOp::EndsWith => pattern(format!("*{}", escape_wildcard(&values[0])), true)?,
        FilterOp::Like => pattern(like_to_wildcard(&values[0]), false)?,
        FilterOp::NotLike => not(pattern(like_to_wildcard(&values[0]), false)?),
        FilterOp::In => json!({ "terms": { exact_field()?: values.iter().map(|v| typed(v)).collect::<AppResult<Vec<_>>>()? } }),
        FilterOp::NotIn => not(json!({ "terms": { exact_field()?: values.iter().map(|v| typed(v)).collect::<AppResult<Vec<_>>>()? } })),
        FilterOp::IsNull => not(json!({ "exists": { "field": field } })),
        FilterOp::IsNotNull => json!({ "exists": { "field": field } }),
    })
}

/// The browse query: raw or structured filter under `bool.filter`.
pub fn query(target: &Target, filter: &Filter) -> AppResult<Json> {
    let clause = match filter {
        _ if filter.is_none() => return Ok(json!({ "match_all": {} })),
        Filter::None => return Ok(json!({ "match_all": {} })),
        Filter::Raw { language, text } => {
            if *language != RawFilterLang::EsQueryDsl {
                return Err(crate::contract::unsupported_capability("this filter language"));
            }
            dsl::parse_filter(text)?
        }
        Filter::Conditions { conditions, match_any } => {
            let clauses = conditions
                .iter()
                .map(|condition| condition_clause(target, condition))
                .collect::<AppResult<Vec<_>>>()?;
            if *match_any {
                json!({ "bool": { "should": clauses, "minimum_should_match": 1 } })
            } else {
                json!({ "bool": { "filter": clauses } })
            }
        }
    };
    Ok(json!({ "bool": { "filter": [clause] } }))
}

/// The sort clauses for the requested order (text fields sort on their keyword sub-field).
pub fn sort(engine: &EsEngine, target: &Target, keys: &[SortKey]) -> AppResult<Vec<Json>> {
    let mut clauses = Vec::new();
    for key in keys {
        let order = if key.descending { "desc" } else { "asc" };
        let field = match key.column.as_str() {
            ID if !engine.info().is_opensearch() => {
                return Err(AppError::new(
                    "Elasticsearch cannot sort by _id. Sort by another field, or declare a key field in the mapping (_meta.fosta.key).",
                ))
            }
            ID | INDEX => key.column.clone(),
            name => target
                .column(name)
                .and_then(|column| column.exact.clone())
                .ok_or_else(|| AppError::new(format!("{name} cannot be sorted (analysed text, object or unknown field).")))?,
        };
        clauses.push(json!({ field: { "order": order } }));
    }
    Ok(clauses)
}

/// The opaque paging token in `Page::next_keyset`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageToken {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Vec<Json>>,
    /// Open a point in time when this page is requested (set by the page before, which opened none, so a grid that is
    /// only looked at does not leave point-in-time contexts behind).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub open_pit: bool,
}

impl PageToken {
    pub fn to_keyset(&self) -> Vec<Value> {
        vec![Value::Json(serde_json::to_string(self).unwrap_or_default())]
    }

    pub fn from_keyset(after: &[Value]) -> AppResult<Self> {
        match after {
            [Value::Json(text)] | [Value::Text(text)] => {
                serde_json::from_str(text).map_err(|_| AppError::new("The page position is not valid; reload the grid."))
            }
            _ => Err(AppError::new("The page position is not valid; reload the grid.")),
        }
    }
}

/// One page of documents.
pub async fn browse(engine: &EsEngine, request: &BrowseRequest) -> AppResult<Page> {
    let started = Instant::now();
    let target = resolve(engine, &request.object.name).await?;
    let limit = request.limit.clamp(1, MAX_RESULT_WINDOW - 1);
    let mut token = match &request.after {
        Some(after) => PageToken::from_keyset(after)?,
        None => PageToken {
            from: Some(request.offset),
            ..PageToken::default()
        },
    };
    let mut sort = sort(engine, &target, &request.sort)?;
    // The declared key ends every order (and is the default one), so the order is total and `search_after` works
    // without a point in time.
    if let Some(key) = target.key.as_ref().and_then(|key| key.exact.clone()) {
        if !sort.iter().any(|clause| clause.get(&key).is_some()) {
            sort.push(json!({ key: { "order": "asc" } }));
        }
    }
    let mut body = Map::new();
    body.insert("query".into(), query(&target, &request.filter)?);
    body.insert("size".into(), json!(limit + 1));
    body.insert("seq_no_primary_term".into(), json!(true));
    if request.exact_total {
        body.insert("track_total_hits".into(), json!(true));
    }
    if let Some(columns) = &request.columns {
        let fields: Vec<&String> = columns.iter().filter(|column| !column.starts_with('_')).collect();
        body.insert("_source".into(), json!(fields));
    }
    // Index order is the cheapest; in a point in time `_shard_doc` follows it, which keeps that order and breaks its
    // ties, so the page that switches to the point in time continues the order of the pages before it.
    if sort.is_empty() {
        sort.push(json!("_doc"));
    }
    if token.open_pit && token.pit.is_none() {
        token.pit = Some(engine.open_point_in_time(&target.name, PIT_KEEP_ALIVE).await?);
    }
    let path = match &token.pit {
        Some(pit) => {
            sort.push(json!({ "_shard_doc": "asc" }));
            body.insert("pit".into(), json!({ "id": pit, "keep_alive": PIT_KEEP_ALIVE }));
            "/_search".to_owned()
        }
        None => format!("/{}/_search", segment(&target.name)),
    };
    body.insert("sort".into(), Json::Array(sort));
    let offset = match (&token.after, token.from) {
        (Some(after), _) => {
            body.insert("search_after".into(), Json::Array(after.clone()));
            None
        }
        (None, from) => {
            let from = from.unwrap_or(0);
            // One more than the page is fetched to know whether there is a next page.
            if from + limit + 1 > MAX_RESULT_WINDOW {
                return Err(AppError::new(format!(
                    "Pages beyond the first {MAX_RESULT_WINDOW} documents are reached with Next, not by position."
                )));
            }
            body.insert("from".into(), json!(from));
            Some(from)
        }
    };
    let statement = format!("POST {path}\n{}", Json::Object(body.clone()));
    let response = engine
        .client()
        .call(&EsRequest::new(Method::POST, path).json(Json::Object(body)))
        .await?;
    let mut hits: Vec<Json> = response["hits"]["hits"].as_array().cloned().unwrap_or_default();
    let has_more = hits.len() as u64 > limit;
    hits.truncate(limit as usize);
    let columns = target.column_infos();
    let wanted: Option<Vec<usize>> = request.columns.as_ref().map(|names| {
        columns
            .iter()
            .enumerate()
            .filter(|(_, column)| names.contains(&column.name))
            .map(|(index, _)| index)
            .collect()
    });
    let mut rows = Vec::with_capacity(hits.len());
    let mut versions = Vec::with_capacity(hits.len());
    for hit in &hits {
        let mut row = vec![Value::Text(hit["_id"].as_str().unwrap_or_default().to_owned())];
        if !target.concrete {
            row.push(Value::Text(hit["_index"].as_str().unwrap_or_default().to_owned()));
        }
        for column in &target.columns {
            row.push(values::to_value(&hit["_source"][&column.name], &column.kind));
        }
        if let Some(wanted) = &wanted {
            row = wanted.iter().map(|index| row[*index].clone()).collect();
        }
        rows.push(row);
        versions.push(match (hit["_seq_no"].as_i64(), hit["_primary_term"].as_i64()) {
            (Some(seq_no), Some(term)) => Value::Text(format!("{seq_no}:{term}")),
            _ => Value::Null,
        });
    }
    let total = response["hits"]["total"]["value"].as_u64().map(|value| Total {
        value,
        exact: response["hits"]["total"]["relation"] == "eq",
    });
    let pit = response["pit_id"].as_str().map(str::to_owned).or(token.pit.clone());
    let last_sort = hits.last().and_then(|hit| hit["sort"].as_array().cloned());
    let next_keyset = if has_more {
        next_token(engine, &target, &token, pit, offset, limit, total, last_sort)
            .await?
            .map(|token| token.to_keyset())
    } else {
        None
    };
    Ok(Page {
        columns: match &wanted {
            Some(wanted) => wanted.iter().map(|index| columns[*index].clone()).collect(),
            None => columns,
        },
        rows,
        has_more,
        key_columns: target.key_columns(),
        next_keyset,
        total: (request.estimate_total || request.exact_total).then_some(total).flatten(),
        statement: Some(statement),
        elapsed_ms: started.elapsed().as_millis() as u64,
        row_versions: Some(versions),
    })
}

/// The token for the page after this one (see the module docs).
#[allow(clippy::too_many_arguments)]
async fn next_token(
    engine: &EsEngine,
    target: &Target,
    token: &PageToken,
    pit: Option<String>,
    offset: Option<u64>,
    limit: u64,
    total: Option<Total>,
    last_sort: Option<Vec<Json>>,
) -> AppResult<Option<PageToken>> {
    if token.pit.is_some() {
        return Ok(last_sort.map(|after| PageToken {
            pit,
            after: Some(after),
            ..PageToken::default()
        }));
    }
    let next = offset.map(|offset| offset + limit);
    let small = total.is_some_and(|total| total.exact && total.value <= MAX_RESULT_WINDOW);
    match next {
        Some(next) if small && next + limit < MAX_RESULT_WINDOW => {
            return Ok(Some(PageToken {
                from: Some(next),
                ..PageToken::default()
            }))
        }
        _ => {}
    }
    // Deep paging: `search_after` on the sort, which ends with the declared key.
    if target.key.is_some() {
        return Ok(last_sort.map(|after| PageToken {
            after: Some(after),
            ..PageToken::default()
        }));
    }
    // No total order without a key: continue in a point in time, ordered by `_shard_doc`.
    if engine.features().point_in_time {
        if let Some(next) = next.filter(|next| next + limit < MAX_RESULT_WINDOW) {
            return Ok(Some(PageToken {
                from: Some(next),
                open_pit: true,
                ..PageToken::default()
            }));
        }
    }
    Ok(next.filter(|next| next + limit < MAX_RESULT_WINDOW).map(|next| PageToken {
        from: Some(next),
        ..PageToken::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(key: bool) -> Target {
        let columns = values::columns_from_mapping(&json!({ "properties": {
            "id": { "type": "long" },
            "name": { "type": "keyword" },
            "title": { "type": "text", "fields": { "keyword": { "type": "keyword" } } },
            "body": { "type": "text" },
            "price": { "type": "scaled_float", "scaling_factor": 100 },
            "note": { "type": "keyword" }
        } }));
        Target {
            name: "items".into(),
            concrete: true,
            indices: vec!["items".into()],
            key: key.then(|| columns.iter().find(|c| c.name == "id").cloned()).flatten(),
            columns,
            routing_required: false,
        }
    }

    fn clause(op: FilterOp, column: &str, values: &[&str]) -> AppResult<Json> {
        condition_clause(&target(true), &FilterCond::new(column, op, values))
    }

    #[test]
    fn every_operator_becomes_a_clause() {
        assert_eq!(clause(FilterOp::Eq, "id", &["5"]).unwrap(), json!({ "term": { "id": 5 } }));
        assert_eq!(
            clause(FilterOp::Ne, "id", &["5"]).unwrap(),
            json!({ "bool": { "must_not": [{ "term": { "id": 5 } }] } })
        );
        assert_eq!(
            clause(FilterOp::Lt, "id", &["11"]).unwrap(),
            json!({ "range": { "id": { "lt": 11 } } })
        );
        assert_eq!(
            clause(FilterOp::Between, "price", &["1.5", "2"]).unwrap(),
            json!({ "range": { "price": { "gte": 1.5, "lte": 2.0 } } })
        );
        assert_eq!(
            clause(FilterOp::Contains, "name", &["a*b"]).unwrap(),
            json!({ "wildcard": { "name": { "value": "*a\\*b*", "case_insensitive": true } } })
        );
        assert_eq!(
            clause(FilterOp::StartsWith, "title", &["Oak"]).unwrap(),
            json!({ "prefix": { "title.keyword": { "value": "Oak", "case_insensitive": true } } })
        );
        assert_eq!(
            clause(FilterOp::Like, "name", &["item 00_1%"]).unwrap(),
            json!({ "wildcard": { "name": { "value": "item 00?1*", "case_insensitive": false } } })
        );
        assert_eq!(
            clause(FilterOp::In, "id", &["1", "2"]).unwrap(),
            json!({ "terms": { "id": [1, 2] } })
        );
        assert_eq!(
            clause(FilterOp::IsNull, "note", &[]).unwrap(),
            json!({ "bool": { "must_not": [{ "exists": { "field": "note" } }] } })
        );
        assert_eq!(clause(FilterOp::Eq, "_id", &["a"]).unwrap(), json!({ "ids": { "values": ["a"] } }));
        assert!(clause(FilterOp::Lt, "_id", &["a"]).is_err());
        assert!(clause(FilterOp::Eq, "body", &["x"]).is_err(), "analysed text has no exact form");
        assert!(clause(FilterOp::Eq, "missing", &["x"]).is_err());
        assert!(clause(FilterOp::Eq, "id", &["five"]).is_err());
        assert!(clause(FilterOp::Between, "id", &["1"]).is_err(), "arity is checked");
    }

    #[test]
    fn filters_go_under_bool_filter_and_raw_filters_are_checked() {
        let t = target(false);
        assert_eq!(query(&t, &Filter::None).unwrap(), json!({ "match_all": {} }));
        let raw = Filter::Raw {
            language: RawFilterLang::EsQueryDsl,
            text: r#"{ "term": { "name": "a" } }"#.into(),
        };
        assert_eq!(
            query(&t, &raw).unwrap(),
            json!({ "bool": { "filter": [{ "term": { "name": "a" } }] } })
        );
        for text in [
            r#"{ "script": { "script": "1" } }"#,
            r#"{ "wrapper": { "query": "e30=" } }"#,
            r#"{ "query": { "match_all": {} }, "size": 5 }"#,
        ] {
            let bad = Filter::Raw {
                language: RawFilterLang::EsQueryDsl,
                text: text.into(),
            };
            assert_eq!(query(&t, &bad).unwrap_err().code(), Some(dsl::INVALID_FILTER), "{text}");
        }
        let sql = Filter::Raw {
            language: RawFilterLang::Sql,
            text: "1=1".into(),
        };
        assert!(query(&t, &sql).is_err());
        let any = Filter::Conditions {
            conditions: vec![
                FilterCond::new("name", FilterOp::Eq, &["a"]),
                FilterCond::new("name", FilterOp::Eq, &["b"]),
            ],
            match_any: true,
        };
        assert_eq!(query(&t, &any).unwrap()["bool"]["filter"][0]["bool"]["minimum_should_match"], 1);
    }

    #[test]
    fn keys_and_columns_follow_the_declaration() {
        let keyed = target(true);
        assert_eq!(keyed.key_columns(), vec!["id".to_owned()]);
        let infos = keyed.column_infos();
        assert!(infos[0].hints.read_only && !infos[0].hints.primary);
        assert!(infos.iter().find(|c| c.name == "id").unwrap().hints.primary);
        assert_eq!(keyed.id_of(&Value::Int(5)).unwrap(), "5");
        let mut alias = target(false);
        alias.concrete = false;
        assert_eq!(alias.key_columns(), vec!["_index".to_owned(), "_id".to_owned()]);
        assert!(alias.column_infos()[1].hints.primary);
    }

    #[test]
    fn page_tokens_round_trip_as_one_json_value() {
        let token = PageToken {
            pit: Some("abc".into()),
            from: None,
            after: Some(vec![json!(10), json!(3)]),
            open_pit: false,
        };
        let pending = PageToken {
            from: Some(1000),
            open_pit: true,
            ..PageToken::default()
        };
        assert_eq!(PageToken::from_keyset(&pending.to_keyset()).unwrap(), pending);
        assert!(!serde_json::to_string(&token).unwrap().contains("open_pit"));
        let keyset = token.to_keyset();
        assert_eq!(keyset.len(), 1);
        assert_eq!(PageToken::from_keyset(&keyset).unwrap(), token);
        assert!(PageToken::from_keyset(&[Value::Int(1)]).is_err());
    }
}
