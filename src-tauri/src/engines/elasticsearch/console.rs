//! The Dev Tools console: parses Kibana-style request blocks and runs them.
//!
//! ```text
//! # comment
//! GET /products/_search
//! {
//!   "query": { "match": { "name": """multi
//! line""" } }
//! }
//!
//! POST _bulk
//! { "index": { "_index": "t" } }
//! { "a": 1 }
//! ```
//!
//! A request starts at a line beginning with an HTTP method. Its body is every following line up to the next request
//! line. A body with one JSON value is sent as JSON; several values (`_bulk`, `_msearch`) are sent as NDJSON. Kibana's
//! triple-quoted strings and `#` / `//` comment lines are accepted.

use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    classify::{classify, RequestClassification},
    client::{EsBody, EsRequest, EsResponse},
    connection::EsEngine,
};
use crate::error::{AppError, AppResult};

const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "HEAD", "PATCH"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleRequest {
    /// 1-based line of the request line; the body starts on the next line.
    pub line: usize,
    pub method: String,
    /// Path with a leading `/` and the query string.
    pub path: String,
    pub body: Option<EsBody>,
    pub classification: RequestClassification,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleResult {
    pub request: ConsoleRequest,
    pub response: Option<EsResponse>,
    /// A transport failure (timeout, connection); HTTP errors come back in `response`.
    pub error: Option<String>,
}

/// Parses console text into requests. Errors name the line, like Kibana's console.
pub fn parse(input: &str) -> AppResult<Vec<ConsoleRequest>> {
    let lines: Vec<&str> = input.lines().collect();
    let mut requests = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty() || is_comment(line) {
            index += 1;
            continue;
        }
        let Some((method, path)) = request_line(line) else {
            return Err(AppError::new(format!(
                "Line {}: expected a request like `GET /index/_search`, found `{line}`.",
                index + 1
            )));
        };
        let start = index;
        index += 1;
        let mut body_lines = Vec::new();
        while index < lines.len() && request_line(lines[index].trim()).is_none() {
            body_lines.push(lines[index]);
            index += 1;
        }
        let body = parse_body(&body_lines).map_err(|error| AppError::new(format!("Line {}: {error}", start + 1)))?;
        let method_value = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);
        let json = match &body {
            Some(EsBody::Json(value)) => Some(value),
            _ => None,
        };
        let classification = classify(&method_value, &path, json);
        requests.push(ConsoleRequest {
            line: start + 1,
            method,
            path,
            body,
            classification,
        });
    }
    Ok(requests)
}

impl ConsoleRequest {
    /// The HTTP request to send. A request whose path the classifier refused (a path the client would rewrite) is never
    /// built, so it cannot reach the cluster whatever called it.
    pub fn to_es_request(&self, timeout: Duration, opaque_id: Option<String>) -> AppResult<EsRequest> {
        if let Some(reason) = &self.classification.refused {
            return Err(AppError::coded("INVALID_REQUEST", format!("Line {}: {reason}", self.line)));
        }
        if self.path.starts_with("kbn:") {
            return Err(AppError::coded(
                "INVALID_REQUEST",
                format!("Line {}: Kibana APIs (kbn:) are not available here.", self.line),
            ));
        }
        let method = Method::from_bytes(self.method.as_bytes()).map_err(|_| AppError::new("Unknown HTTP method."))?;
        let mut request = EsRequest::new(method, self.path.clone()).timeout(timeout);
        request.body = self.body.clone();
        request.opaque_id = opaque_id;
        Ok(request)
    }
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#') || line.starts_with("//")
}

/// `GET /path?x=1` → (`GET`, `/path?x=1`). The method must start the line (no indentation inside a body counts).
fn request_line(line: &str) -> Option<(String, String)> {
    let (method, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let method = method.to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return None;
    }
    let path = rest.trim();
    if path.starts_with('{') || path.starts_with('"') {
        return None;
    }
    if path.starts_with("kbn:") {
        return Some((method, path.to_string()));
    }
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Some((method, path))
}

fn parse_body(lines: &[&str]) -> Result<Option<EsBody>, String> {
    let kept: Vec<&str> = lines.iter().copied().filter(|line| !is_comment(line.trim())).collect();
    let text = kept.join("\n");
    if text.trim().is_empty() {
        return Ok(None);
    }
    let text = convert_triple_quotes(&text)?;
    let mut values = Vec::new();
    let stream = serde_json::Deserializer::from_str(&text).into_iter::<Value>();
    for value in stream {
        values.push(value.map_err(|error| format!("the body is not valid JSON ({error})."))?);
    }
    Ok(match values.len() {
        0 => None,
        1 => Some(EsBody::Json(values.remove(0))),
        _ => Some(EsBody::NdJson(values)),
    })
}

/// Turns Kibana's `"""raw text"""` into JSON strings.
fn convert_triple_quotes(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\"\"\"") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let end = after.find("\"\"\"").ok_or("a triple-quoted string is not closed.")?;
        out.push_str(&serde_json::to_string(&after[..end]).unwrap_or_default());
        rest = &after[end + 3..];
    }
    out.push_str(rest);
    Ok(out)
}

impl EsEngine {
    /// Runs every request in order, each with `timeout`. A transport failure stops the run; HTTP errors do not.
    pub async fn run_console(&self, input: &str, timeout: Duration) -> AppResult<Vec<ConsoleResult>> {
        let requests = parse(input)?;
        let mut results = Vec::new();
        for request in requests {
            if request.path.starts_with("kbn:") {
                results.push(ConsoleResult {
                    request,
                    response: None,
                    error: Some("Kibana APIs (kbn:) are not available here.".into()),
                });
                continue;
            }
            let outcome = self.run_console_request(&request, timeout).await;
            let failed = outcome.is_err();
            let (response, error) = match outcome {
                Ok(response) => (Some(response), None),
                Err(error) => (None, Some(error.to_string())),
            };
            results.push(ConsoleResult { request, response, error });
            if failed {
                break;
            }
        }
        Ok(results)
    }

    pub async fn run_console_request(&self, request: &ConsoleRequest, timeout: Duration) -> AppResult<EsResponse> {
        self.client.send(&request.to_es_request(timeout, None)?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::elasticsearch::classify::RequestClass;
    use serde_json::json;

    #[test]
    fn parses_kibana_console_text() {
        let input = r#"
# list indices
GET _cat/indices?v

GET /products/_search
{
  "query": { "match": { "name": """quoted "text"
on two lines""" } } // trailing comments are not supported, whole-line ones are
}

POST _bulk
{ "index": { "_index": "t" } }
// a comment line inside a body
{ "a": 1 }
delete products
"#
        .replace(" // trailing comments are not supported, whole-line ones are", "");
        let requests = parse(&input).unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            (requests[0].line, requests[0].method.as_str(), requests[0].path.as_str()),
            (3, "GET", "/_cat/indices?v")
        );
        assert_eq!(requests[0].body, None);
        assert_eq!(
            requests[1].body,
            Some(EsBody::Json(
                json!({ "query": { "match": { "name": "quoted \"text\"\non two lines" } } })
            ))
        );
        assert_eq!(requests[1].classification.class, RequestClass::Read);
        assert_eq!(
            requests[2].body,
            Some(EsBody::NdJson(vec![json!({ "index": { "_index": "t" } }), json!({ "a": 1 })]))
        );
        assert_eq!(requests[2].classification.class, RequestClass::Write);
        assert_eq!((requests[3].method.as_str(), requests[3].path.as_str()), ("DELETE", "/products"));
        assert_eq!(requests[3].classification.class, RequestClass::Destructive);
    }

    #[test]
    fn reports_errors_with_line_numbers() {
        let error = parse("GET /a/_search\n{ \"query\": }").unwrap_err().to_string();
        assert!(error.starts_with("Line 1:"), "{error}");
        let error = parse("\n\nnot a request").unwrap_err().to_string();
        assert!(error.starts_with("Line 3:"), "{error}");
        assert!(parse("POST /x\n{\"a\": \"\"\"open}").is_err());
        assert!(parse("   \n# only comments\n").unwrap().is_empty());
    }

    #[test]
    fn requests_with_rewritten_paths_are_never_built() {
        let requests = parse("DELETE /_count/../products\n\nGET /products/_search").unwrap();
        assert!(requests[0].classification.refused.is_some());
        let error = requests[0].to_es_request(Duration::from_secs(1), None).unwrap_err();
        assert_eq!(error.code(), Some("INVALID_REQUEST"));
        assert!(requests[1].to_es_request(Duration::from_secs(1), Some("fosta-1".into())).is_ok());
        let kibana = parse("GET kbn:/api/status").unwrap();
        assert!(kibana[0].to_es_request(Duration::from_secs(1), None).is_err());
    }

    #[test]
    fn parses_bulk_with_raw_strings_between_requests() {
        let input = r#"
PUT fosta-test-console
{ "settings": { "number_of_replicas": 0 } }

POST _bulk?refresh=true
{ "index": { "_index": "fosta-test-console", "_id": "1" } }
{ "title": """multi
line""" }
{ "index": { "_index": "fosta-test-console", "_id": "2" } }
{ "title": "two" }

GET fosta-test-console/_doc/404

DELETE fosta-test-console
"#;
        let requests = parse(input).unwrap();
        let summary: Vec<_> = requests.iter().map(|r| format!("{} {} {}", r.line, r.method, r.path)).collect();
        assert_eq!(
            summary,
            vec![
                "2 PUT /fosta-test-console",
                "5 POST /_bulk?refresh=true",
                "12 GET /fosta-test-console/_doc/404",
                "14 DELETE /fosta-test-console"
            ]
        );
        let Some(EsBody::NdJson(lines)) = &requests[1].body else {
            panic!("{:?}", requests[1].body)
        };
        assert_eq!(lines[1], json!({ "title": "multi\nline" }));
    }
}
