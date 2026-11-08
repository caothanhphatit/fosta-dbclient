//! Structured errors from Elasticsearch / OpenSearch error bodies.
//!
//! A REST error is `{ "error": { "type", "reason", "caused_by": { … }, "root_cause": [ … ], "failed_shards": [ … ],
//! "line", "col" }, "status" }`. It becomes an `ENGINE_ERROR` whose vendor code is the error `type`
//! (`version_conflict_engine_exception`, `parsing_exception`), whose message is the `reason`, whose `cause` lists the
//! `caused_by` chain, the root causes and the first shard failure as `type: reason`, and which carries the HTTP status.
//! `parsing_exception` and JSON errors report `line` / `col` in the request body, which become a position.

use serde_json::Value;

use super::client::error_message;
use crate::error::{AppError, ErrorPosition};

pub const ENGINE_ERROR: &str = "ENGINE_ERROR";
/// HTTP 409 on a write: a version conflict (`if_seq_no` mismatch) or a document that already exists.
pub const STATUS_CONFLICT: u16 = 409;

/// Where in the request body the server located the error (1-based line and column of the body).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPosition {
    pub line: u32,
    pub column: u32,
}

/// The `type: reason` texts of a `caused_by` chain, the root causes and the first shard failure, without repeats.
fn cause_chain(error: &Value) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut push = |node: &Value| {
        if let Some(reason) = node["reason"].as_str() {
            let text = format!("{}: {reason}", node["type"].as_str().unwrap_or("error"));
            if !chain.contains(&text) {
                chain.push(text);
            }
        }
    };
    let mut cause = &error["caused_by"];
    let mut depth = 0;
    while cause.is_object() && depth < 16 {
        push(cause);
        cause = &cause["caused_by"];
        depth += 1;
    }
    if let Some(roots) = error["root_cause"].as_array() {
        for root in roots.iter().take(4) {
            push(root);
        }
    }
    if let Some(failure) = error["failed_shards"].as_array().and_then(|shards| shards.first()) {
        push(&failure["reason"]);
        let mut inner = &failure["reason"]["caused_by"];
        let mut depth = 0;
        while inner.is_object() && depth < 16 {
            push(inner);
            inner = &inner["caused_by"];
            depth += 1;
        }
    }
    chain
}

/// The body position of an error, looked up on the error and its causes (`parsing_exception`, `x_content_parse_exception`).
pub fn body_position(body: &Value) -> Option<BodyPosition> {
    fn find(node: &Value, depth: usize) -> Option<BodyPosition> {
        if depth > 16 || !node.is_object() {
            return None;
        }
        if let (Some(line), Some(column)) = (node["line"].as_u64(), node["col"].as_u64()) {
            return Some(BodyPosition {
                line: line as u32,
                column: column as u32,
            });
        }
        find(&node["caused_by"], depth + 1).or_else(|| node["root_cause"].as_array()?.iter().find_map(|root| find(root, depth + 1)))
    }
    find(&body["error"], 0)
}

/// The vendor code: the error `type`, or the root cause's when the top level is a generic wrapper.
pub fn error_type(body: &Value) -> Option<String> {
    let error = &body["error"];
    let top = error["type"].as_str()?;
    // `search_phase_execution_exception: all shards failed` says little; the root cause names the real problem.
    if top == "search_phase_execution_exception" {
        if let Some(root) = error["root_cause"][0]["type"].as_str() {
            return Some(root.to_owned());
        }
    }
    Some(top.to_owned())
}

/// Turns an HTTP error response into a structured `ENGINE_ERROR`.
pub fn from_response(status: u16, body: &Value) -> AppError {
    let error = &body["error"];
    let message = match error {
        Value::Object(_) => {
            // The root cause's reason is the useful sentence for wrapped errors ("all shards failed").
            let reason = if error["type"] == "search_phase_execution_exception" {
                error["root_cause"][0]["reason"].as_str().or(error["reason"].as_str())
            } else {
                error["reason"].as_str()
            };
            match reason.filter(|reason| !reason.is_empty()) {
                Some(reason) => match status {
                    401 => format!("The cluster rejected the credentials (401). {reason}"),
                    403 => format!("The user has no permission for this request (403). {reason}"),
                    _ => reason.to_owned(),
                },
                None => error_message(status, body),
            }
        }
        _ => error_message(status, body),
    };
    let vendor = error_type(body).unwrap_or_else(|| format!("http_{status}"));
    let mut out = AppError::coded(ENGINE_ERROR, message)
        .with_vendor_code(vendor)
        .with("status", status)
        .with_cause(if error.is_object() { cause_chain(error) } else { Vec::new() })
        .retryable(matches!(status, 429 | 502 | 503 | 504));
    if let Some(position) = body_position(body) {
        out = out.with_position(ErrorPosition {
            line: Some(position.line),
            column: Some(position.column),
            offset: None,
        });
    }
    out
}

/// Moves a body position to the editor: the body starts on `body_line` (1-based) of the text that was run, and the text
/// starts at `offset` characters into the editor. Columns stay as reported.
pub fn shift_position(error: AppError, body_line: u32, text: &str) -> AppError {
    let Some(position) = error.field("position").cloned() else {
        return error;
    };
    let (Some(line), Some(column)) = (position["line"].as_u64(), position["column"].as_u64()) else {
        return error;
    };
    let line = body_line + line as u32 - 1;
    // The 0-based character offset of (line, column) in `text`.
    let mut offset = 0usize;
    for (index, text_line) in text.split('\n').enumerate() {
        if index + 1 == line as usize {
            offset += (column as usize).saturating_sub(1).min(text_line.chars().count());
            break;
        }
        offset += text_line.chars().count() + 1;
    }
    error.with_position(ErrorPosition {
        line: Some(line),
        column: Some(column as u32),
        offset: Some(offset as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn error_bodies_become_structured_engine_errors() {
        let body = json!({
            "error": {
                "root_cause": [{ "type": "version_conflict_engine_exception", "reason": "[5]: version conflict, document already exists (current version [1])" }],
                "type": "version_conflict_engine_exception",
                "reason": "[5]: version conflict, document already exists (current version [1])",
                "index": "contract_items"
            },
            "status": 409
        });
        let error = from_response(409, &body);
        assert_eq!(error.code(), Some(ENGINE_ERROR));
        assert_eq!(error.field("vendorCode"), Some(&json!("version_conflict_engine_exception")));
        assert_eq!(error.field("status"), Some(&json!(409)));
        assert!(error.message().contains("document already exists"));
    }

    #[test]
    fn caused_by_chains_and_shard_failures_fill_the_cause() {
        let body = json!({
            "error": {
                "root_cause": [{ "type": "query_shard_exception", "reason": "failed to create query: For input string: \"x\"" }],
                "type": "search_phase_execution_exception",
                "reason": "all shards failed",
                "caused_by": { "type": "number_format_exception", "reason": "For input string: \"x\"",
                               "caused_by": { "type": "illegal_argument_exception", "reason": "deeper" } },
                "failed_shards": [{ "shard": 0, "reason": { "type": "query_shard_exception", "reason": "failed to create query: For input string: \"x\"" } }]
            },
            "status": 400
        });
        let error = from_response(400, &body);
        assert_eq!(error.field("vendorCode"), Some(&json!("query_shard_exception")));
        assert!(error.message().starts_with("failed to create query"), "{}", error.message());
        let cause: Vec<String> = serde_json::from_value(error.field("cause").cloned().unwrap()).unwrap();
        assert_eq!(cause[0], "number_format_exception: For input string: \"x\"");
        assert_eq!(cause[1], "illegal_argument_exception: deeper");
        assert!(cause.iter().any(|text| text.starts_with("query_shard_exception")));
        // No repeats: the root cause and the shard failure are the same text.
        assert_eq!(cause.iter().filter(|text| text.starts_with("query_shard_exception")).count(), 1);
    }

    #[test]
    fn parsing_errors_carry_a_position_shifted_to_the_editor() {
        let body = json!({
            "error": { "root_cause": [{ "type": "parsing_exception", "reason": "unknown query [matc]", "line": 1, "col": 22 }],
                       "type": "parsing_exception", "reason": "unknown query [matc]", "line": 1, "col": 22 },
            "status": 400
        });
        let error = from_response(400, &body);
        assert_eq!(error.field("vendorCode"), Some(&json!("parsing_exception")));
        assert_eq!(error.field("position").unwrap()["line"], 1);
        let text = "GET /t/_search\n{ \"query\": { \"matc\": {} } }";
        let shifted = shift_position(error, 2, text);
        let position = shifted.field("position").unwrap();
        assert_eq!(position["line"], 2);
        assert_eq!(position["offset"], 15 + 21);
    }

    #[test]
    fn plain_and_auth_errors_keep_a_sentence() {
        let auth = from_response(
            401,
            &json!({ "error": { "type": "security_exception", "reason": "unable to authenticate user [elastic]" }, "status": 401 }),
        );
        assert!(auth.message().starts_with("The cluster rejected the credentials (401)."));
        assert_eq!(auth.field("vendorCode"), Some(&json!("security_exception")));
        let text = from_response(404, &json!("Not Found"));
        assert_eq!(text.field("vendorCode"), Some(&json!("http_404")));
        let busy = from_response(
            429,
            &json!({ "error": { "type": "es_rejected_execution_exception", "reason": "rejected" } }),
        );
        assert_eq!(busy.field("retryable"), Some(&json!(true)));
    }
}
