//! The grid's raw filter: one Query DSL query, checked before it is placed under `bool.filter` of the browse search.
//!
//! This is a security boundary (a read-only workspace allows filters), so the check is strict and runs twice: in
//! `Engine::validate_filter` (the gateway) and again inside `browse`, which the assistant and the suites reach without
//! the gateway. A filter must be a JSON object and must not run code or smuggle a query past this check:
//!
//! - `script`, `script_score`, `script_fields`, `scripted_metric`, `_script`, and every key ending in `_script`
//!   (`terms_set.minimum_should_match_script`, scripted metric phases): Painless on the cluster;
//! - `runtime_mappings`: fields computed by scripts;
//! - `wrapper`: a base64-encoded query that the server decodes after this check.
//!
//! The filter is never merged into the request body, so search options (`size`, `aggs`, `pit`) cannot be set from it.

use serde_json::Value;

use crate::error::{AppError, AppResult};

pub const INVALID_FILTER: &str = "INVALID_FILTER";

/// Keys that run code on the cluster or hide a query from this check.
const DENIED_KEYS: &[&str] = &[
    "script",
    "script_score",
    "script_fields",
    "scripted_metric",
    "_script",
    "runtime_mappings",
    "wrapper",
];

fn reject(filter: &str, message: impl Into<String>) -> AppError {
    AppError::structured(message, serde_json::json!({ "code": INVALID_FILTER, "filter": filter }))
}

/// The first denied key in `value`, at any depth.
fn denied_key(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map.iter().find_map(|(key, inner)| {
            let lower = key.to_ascii_lowercase();
            if DENIED_KEYS.contains(&lower.as_str()) || lower.ends_with("_script") {
                Some(key.clone())
            } else {
                denied_key(inner)
            }
        }),
        Value::Array(items) => items.iter().find_map(denied_key),
        _ => None,
    }
}

/// Parses and checks a raw filter; returns the query to put under `bool.filter`. A filter pasted as
/// `{ "query": { … } }` is unwrapped.
pub fn parse_filter(text: &str) -> AppResult<Value> {
    let value: Value = serde_json::from_str(text.trim()).map_err(|error| {
        reject(
            text,
            format!("The filter must be a Query DSL query in JSON, e.g. {{ \"term\": {{ \"status\": \"paid\" }} }}. ({error})"),
        )
    })?;
    let Value::Object(map) = &value else {
        return Err(reject(text, "The filter must be a JSON object (a Query DSL query)."));
    };
    if map.is_empty() {
        return Err(reject(text, "The filter is empty; write a query such as { \"match_all\": {} }."));
    }
    if let Some(key) = denied_key(&value) {
        return Err(reject(
            text,
            format!("'{key}' is not allowed in a filter: it runs code on the cluster or hides a query from the check."),
        ));
    }
    let query = match map.get("query") {
        Some(inner) if map.len() == 1 && inner.is_object() => inner.clone(),
        _ => value.clone(),
    };
    if query.as_object().map_or(0, |map| map.len()) != 1 {
        return Err(reject(
            text,
            "The filter must be one query clause, e.g. { \"bool\": { … } } or { \"range\": { … } }.",
        ));
    }
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rejected(text: &str) -> String {
        match parse_filter(text) {
            Ok(value) => panic!("accepted {text}: {value}"),
            Err(error) => {
                assert_eq!(error.code(), Some(INVALID_FILTER), "{text}");
                error.to_string()
            }
        }
    }

    #[test]
    fn ordinary_queries_pass_and_query_wrappers_are_unwrapped() {
        assert_eq!(
            parse_filter(r#"{ "term": { "status": "paid" } }"#).unwrap(),
            json!({ "term": { "status": "paid" } })
        );
        assert_eq!(
            parse_filter(r#"{ "query": { "range": { "price": { "gte": 10 } } } }"#).unwrap(),
            json!({ "range": { "price": { "gte": 10 } } })
        );
        assert!(parse_filter(
            r#"{ "bool": { "must": [ { "match": { "description": "oak" } } ], "must_not": [ { "exists": { "field": "note" } } ] } }"#
        )
        .is_ok());
        assert!(parse_filter(r#"{ "nested": { "path": "variants", "query": { "term": { "variants.color": "red" } } } }"#).is_ok());
        // Field names that merely contain "script" are fine.
        assert!(parse_filter(r#"{ "term": { "transcript": "x" } }"#).is_ok());
        assert!(parse_filter(r#"{ "match": { "description": "javascript" } }"#).is_ok());
    }

    #[test]
    fn non_queries_are_rejected() {
        rejected("status = 'paid'");
        rejected("[1, 2]");
        rejected("\"text\"");
        rejected("{}");
        rejected("");
        // Search options are not a query clause.
        rejected(r#"{ "query": { "match_all": {} }, "size": 10000 }"#);
        rejected(r#"{ "match_all": {}, "aggs": { "x": { "terms": { "field": "a" } } } }"#);
    }

    #[test]
    fn every_code_running_or_smuggling_vector_is_rejected() {
        for text in [
            // script query, also nested deep in bool clauses
            r#"{ "script": { "script": "doc['a'].value > 1" } }"#,
            r#"{ "bool": { "filter": [ { "script": { "script": { "source": "1", "lang": "painless" } } } ] } }"#,
            r#"{ "bool": { "should": [ { "bool": { "must": [ { "script": { "script": "1" } } ] } } ] } }"#,
            // function_score with a script_score
            r#"{ "function_score": { "query": { "match_all": {} }, "script_score": { "script": "1" } } }"#,
            r#"{ "script_score": { "query": { "match_all": {} }, "script": { "source": "1" } } }"#,
            // terms_set with a script whose object has no "script" key inside
            r#"{ "terms_set": { "tags": { "terms": ["a"], "minimum_should_match_script": { "source": "params.num_terms" } } } }"#,
            // a base64 query decoded only by the server
            r#"{ "wrapper": { "query": "eyJzY3JpcHQiOnsic2NyaXB0IjoiMSJ9fQ==" } }"#,
            r#"{ "bool": { "filter": { "wrapper": { "query": "e30=" } } } }"#,
            // runtime fields, script fields, sort scripts, scripted metrics
            r#"{ "query": { "match_all": {} }, "runtime_mappings": { "x": { "type": "long", "script": "emit(1)" } } }"#,
            r#"{ "runtime_mappings": { "x": { "type": "long" } } }"#,
            r#"{ "script_fields": { "x": { "script": "1" } } }"#,
            r#"{ "bool": { "must": [ { "_script": { "type": "number" } } ] } }"#,
            r#"{ "scripted_metric": { "init_script": "state.x = 1" } }"#,
            r#"{ "bool": { "must": [ { "intervals": { "f": { "match": { "query": "x", "filter": { "script": { "source": "1" } } } } } } ] } }"#,
            // other casing and unicode escapes decode to the same key
            r#"{ "SCRIPT": { "script": "1" } }"#,
            r#"{ "script": { "script": "1" } }"#,
            r#"{ "wrapper": { "query": "e30=" } }"#,
            r#"{ "bool": { "filter": [ { "terms_set": { "t": { "terms": [], "Minimum_Should_Match_Script": {} } } } ] } }"#,
        ] {
            let message = rejected(text);
            assert!(message.contains("not allowed"), "{text}: {message}");
        }
    }
}
