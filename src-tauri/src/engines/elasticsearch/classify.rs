//! Request classification for the statement gateway, from the HTTP method and path (the body only matters for SQL and
//! `_aliases`).
//!
//! | Class         | Requests                                                                                              |
//! |---------------|-------------------------------------------------------------------------------------------------------|
//! | `Read`        | every GET/HEAD except `_flush` / `_refresh`; `_search`, `_msearch`, `_count`, `_validate`, `_explain`, `_field_caps`, `_analyze`, `_mget`, `_termvectors`, PIT open/close, scroll, `_query` (ES\|QL), `_sql` SELECT/SHOW/DESCRIBE, `_eql`, async search, `_cluster/allocation/explain`, ingest `_simulate` |
//! | `Write`       | `_doc`, `_create`, `_update`, `_bulk`, `_reindex`, DELETE of one document                              |
//! | `Ddl`         | PUT `<index>`, `_mapping`, `_settings`, `_alias(es)`, index / component / legacy templates, ingest pipelines, ILM / ISM policies, data stream create, rollover, shrink/split/clone, `_open` |
//! | `Destructive` | DELETE `<index>`, `_delete_by_query`, `_update_by_query`, `_close`, `_aliases` with `remove_index`, DELETE of templates, pipelines, policies, data streams, snapshots and repositories |
//! | `Admin`       | `_cluster/settings`, reroute, voting exclusions, node shutdown, `_tasks/.../_cancel`, snapshot create/restore, refresh/flush/forcemerge/cache clear, `_security`, `_license`, ILM/ISM start/stop, anything not recognised |
//!
//! `Admin` ranks above `Destructive` in the gateway, so "dangerous" requests are `Destructive` or `Admin`.
//!
//! The API is read from its position: the first segment when it starts with `_` (`/_cluster/settings`), otherwise the
//! second one after the target (`/products/_search`). Later segments are names and ids, so a document id such as
//! `_search` in `PUT /products/_doc/_search` does not make a write look like a read. Segments are percent-decoded the
//! way the server decodes them, and a path the HTTP client would change before sending (`.` / `..` segments, `#`, `\`,
//! characters it re-encodes) is refused, so what is classified is exactly what is sent.

use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestClass {
    Read,
    Write,
    Ddl,
    Admin,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestClassification {
    pub class: RequestClass,
    /// A short name of the API (`search`, `delete index`, `put mapping`...).
    pub api: String,
    /// Target indices / names from the path, when there are any.
    pub target: Option<String>,
    /// Why it got this class when that is not obvious.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Set when the request must not run at all (a path the client would rewrite).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// Checks that `path` (without its query string) reaches the server unchanged and returns its percent-decoded
/// segments. Errors say why the path is refused.
pub fn path_segments(path: &str) -> Result<Vec<String>, String> {
    let raw = path.split('?').next().unwrap_or("");
    if raw.contains('#') || raw.contains('\\') {
        return Err("The path contains '#' or '\\', which the HTTP client would rewrite; encode them as %23 / %5C.".into());
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("The path contains spaces or control characters; percent-encode them.".into());
    }
    // The URL parser resolves dot segments (also `%2e`) and re-encodes some characters: the path it would send must be
    // the path as written.
    let absolute = if raw.starts_with('/') { raw.to_owned() } else { format!("/{raw}") };
    let resolved = Url::parse(&format!("http://cluster{absolute}")).map_err(|error| format!("The path is not valid: {error}"))?;
    if resolved.path() != absolute {
        return Err(format!(
            "The path '{absolute}' would be sent as '{}'; write the path as it should reach the cluster.",
            resolved.path()
        ));
    }
    let mut segments = Vec::new();
    for raw_segment in absolute.split('/').filter(|segment| !segment.is_empty()) {
        let decoded = percent_decode_strict(raw_segment).ok_or_else(|| format!("'{raw_segment}' is not validly percent-encoded."))?;
        if decoded == "." || decoded == ".." {
            return Err("'.' and '..' are not allowed as path segments.".into());
        }
        if decoded.contains('/') || decoded.contains('\\') || decoded.chars().any(char::is_control) {
            return Err(format!(
                "The path segment '{raw_segment}' decodes to a separator or a control character."
            ));
        }
        segments.push(decoded);
    }
    Ok(segments)
}

/// Percent-decoding that fails on malformed escapes and non-UTF-8 results.
fn percent_decode_strict(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Targets that start with `_` but name indices rather than an API.
fn is_target_like(segment: &str) -> bool {
    segment == "_all" || segment.contains(',') || segment.contains('*')
}

const READ_APIS: &[&str] = &[
    "_search",
    "_msearch",
    "_count",
    "_validate",
    "_explain",
    "_field_caps",
    "_analyze",
    "_mget",
    "_termvectors",
    "_mtermvectors",
    "_rank_eval",
    "_render",
    "_query",
    "_eql",
    "_async_search",
    "_pit",
    "_knn_search",
    "_terms_enum",
    "_resolve",
    "_health_report",
    "_disk_usage",
];

/// Classifies a request. `path` may include a query string; `body` tells SQL reads from other statements and finds
/// `remove_index` in `_aliases`.
pub fn classify(method: &Method, path: &str, body: Option<&Value>) -> RequestClassification {
    let segments = match path_segments(path) {
        Ok(segments) => segments,
        Err(reason) => {
            return RequestClassification {
                class: RequestClass::Admin,
                api: "invalid path".into(),
                target: None,
                reason: None,
                refused: Some(reason),
            }
        }
    };
    let first = segments.first().map(String::as_str).unwrap_or("");
    let target = (!first.is_empty() && (!first.starts_with('_') || is_target_like(first))).then(|| first.to_owned());
    let api_index = usize::from(target.is_some());
    let api = segments.get(api_index).map(String::as_str).unwrap_or("");
    let rest: Vec<&str> = segments.iter().skip(api_index + 1).map(String::as_str).collect();
    let done = |class: RequestClass, name: &str| RequestClassification {
        class,
        api: name.to_owned(),
        target: target.clone(),
        reason: None,
        refused: None,
    };
    let because = |class: RequestClass, name: &str, reason: &str| RequestClassification {
        reason: Some(reason.to_owned()),
        ..done(class, name)
    };
    let short = |name: &str| name.trim_start_matches('_').to_owned();
    use RequestClass::*;
    let deleting = *method == Method::DELETE;
    let reading = *method == Method::GET || *method == Method::HEAD;

    // SQL and PPL: read-only APIs, except SQL statements other than queries (OpenSearch can DELETE).
    let sql = (api == "_sql" && target.is_none())
        || (matches!(api, "_plugins" | "_opendistro") && target.is_none() && rest.first() == Some(&"_sql"));
    if sql {
        let query = body
            .and_then(|body| body["query"].as_str())
            .unwrap_or("")
            .trim_start()
            .to_ascii_uppercase();
        let read = query.is_empty()
            || ["SELECT", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "WITH"]
                .iter()
                .any(|word| query.starts_with(word));
        return if read {
            done(Read, "sql")
        } else {
            because(Write, "sql", "A SQL statement that is not a query.")
        };
    }
    if api == "_plugins" && target.is_none() && rest.first() == Some(&"_ppl") {
        return done(Read, "ppl");
    }

    // Read-only APIs whatever the method (search bodies are sent with POST; DELETE of a PIT, scroll or async search only
    // releases resources). `_disk_usage` analyses every shard, so only a GET (which the server refuses) is a read.
    if READ_APIS.contains(&api) {
        if api == "_disk_usage" && !reading {
            return because(Admin, "disk usage", "Analyses every shard of the index.");
        }
        return done(Read, &short(api));
    }
    if target.is_none() && api == "_ingest" && rest.last() == Some(&"_simulate") {
        return done(Read, "simulate pipeline");
    }
    if target.is_none() && api == "_cluster" && rest.first() == Some(&"allocation") {
        return done(Read, "allocation explain");
    }
    // Flush and refresh also answer GET.
    if matches!(api, "_flush" | "_refresh") {
        return done(Admin, &short(api));
    }
    if reading {
        return done(Read, if api.is_empty() { "get" } else { api.trim_start_matches('_') });
    }

    // Document writes.
    match api {
        "_delete_by_query" => return because(Destructive, "delete by query", "Deletes every document the query matches."),
        "_update_by_query" => return because(Destructive, "update by query", "Changes every document the query matches."),
        "_reindex" => return done(Write, "reindex"),
        "_bulk" => return done(Write, "bulk"),
        "_doc" | "_create" | "_update" => {
            return done(Write, if deleting { "delete document" } else { "index document" });
        }
        _ => {}
    }

    // Index lifecycle.
    match api {
        "_close" => return because(Destructive, "close index", "A closed index cannot be searched or written."),
        "_open" | "_rollover" | "_shrink" | "_split" | "_clone" | "_unfreeze" | "_freeze" => return done(Ddl, &short(api)),
        "_forcemerge" | "_cache" | "_upgrade" | "_block" => return done(Admin, &short(api)),
        "_mapping" => return done(Ddl, "put mapping"),
        "_settings" => {
            return if target.is_none() {
                because(Ddl, "put settings", "Changes the settings of every index.")
            } else {
                done(Ddl, "put settings")
            };
        }
        "_alias" => return done(Ddl, "aliases"),
        "_aliases" => {
            let removes_index = body
                .and_then(|body| body["actions"].as_array())
                .is_some_and(|actions| actions.iter().any(|action| action.get("remove_index").is_some()));
            return if removes_index {
                because(Destructive, "aliases", "A remove_index action deletes the index.")
            } else {
                done(Ddl, "aliases")
            };
        }
        _ => {}
    }

    // Catalog objects at the root: templates, pipelines, lifecycle policies, data streams, snapshots.
    if target.is_none() {
        let object_apis = [
            ("_index_template", "index template"),
            ("_component_template", "component template"),
            ("_template", "legacy template"),
            ("_ingest", "ingest pipeline"),
            ("_ilm", "ILM policy"),
            ("_data_stream", "data stream"),
            ("_enrich", "enrich policy"),
            ("_transform", "transform"),
            ("_scripts", "stored script"),
            ("_synonyms", "synonyms set"),
            ("_query_rules", "query rules"),
        ];
        if let Some((_, name)) = object_apis.iter().find(|(object, _)| api == *object) {
            if api == "_ilm" && matches!(rest.first(), Some(&"start") | Some(&"stop")) {
                return done(Admin, "ILM start/stop");
            }
            if api == "_enrich" && rest.last() == Some(&"_execute") {
                return done(Write, "enrich execute");
            }
            return done(if deleting { Destructive } else { Ddl }, name);
        }
        if api == "_plugins" && rest.first() == Some(&"_ism") {
            return match rest.get(1) {
                Some(&"policies") => done(if deleting { Destructive } else { Ddl }, "ISM policy"),
                _ => done(Admin, "ISM"),
            };
        }
        if api == "_snapshot" {
            return match (rest.len(), deleting, rest.last()) {
                (_, _, Some(&"_restore")) => done(Admin, "snapshot restore"),
                (0, true, _) | (1, true, _) => done(Destructive, "delete repository"),
                (_, true, _) => done(Destructive, "delete snapshot"),
                (1, false, _) => done(Ddl, "snapshot repository"),
                _ => done(Admin, "snapshot"),
            };
        }
        if api == "_cluster" && rest.first() == Some(&"settings") {
            return because(Admin, "cluster settings", "Changes cluster-wide settings.");
        }
    }

    // Creating or deleting an index (PUT /<index>, DELETE /<index>).
    if let Some(name) = target.as_deref().filter(|_| api.is_empty()) {
        if deleting {
            return if is_target_like(name) {
                because(Destructive, "delete index", "Deletes every index the pattern matches.")
            } else {
                done(Destructive, "delete index")
            };
        }
        if *method == Method::PUT {
            return done(Ddl, "create index");
        }
    }

    // Cluster and node administration, and everything not recognised.
    let name = match api {
        "_cluster" => "cluster",
        "_nodes" => "nodes",
        "_tasks" => "tasks",
        "_security" => "security",
        "_license" => "license",
        _ => "unknown",
    };
    done(Admin, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn class(method: Method, path: &str) -> RequestClass {
        let classification = classify(&method, path, None);
        assert!(
            classification.refused.is_none(),
            "{method} {path} refused: {:?}",
            classification.refused
        );
        classification.class
    }

    fn refused(method: Method, path: &str) -> String {
        classify(&method, path, None)
            .refused
            .unwrap_or_else(|| panic!("{method} {path} must be refused"))
    }

    #[test]
    fn classifies_requests() {
        use RequestClass::*;
        let cases = [
            (Method::GET, "/products/_search", Read),
            (Method::POST, "/products/_search?typed_keys=true", Read),
            (Method::POST, "/_query", Read),
            (Method::POST, "/products/_count", Read),
            (Method::GET, "/products/_mapping", Read),
            (Method::POST, "/_ingest/pipeline/p/_simulate", Read),
            (Method::POST, "/_ingest/pipeline/_simulate", Read),
            (Method::POST, "/_analyze", Read),
            (Method::POST, "/products/_pit?keep_alive=1m", Read),
            (Method::DELETE, "/_pit", Read),
            (Method::DELETE, "/_search/point_in_time", Read),
            (Method::POST, "/products/_search/point_in_time?keep_alive=1m", Read),
            (Method::GET, "/_cat/indices", Read),
            (Method::GET, "/_cluster/settings", Read),
            (Method::HEAD, "/products", Read),
            (Method::GET, "/", Read),
            (Method::GET, "/_all/_search", Read),
            (Method::POST, "/logs-*,products/_search", Read),
            (Method::POST, "/products/_doc", Write),
            (Method::PUT, "/products/_doc/1", Write),
            (Method::PUT, "/products/_create/1", Write),
            (Method::DELETE, "/products/_doc/1", Write),
            (Method::POST, "/products/_update/1", Write),
            (Method::POST, "/_bulk", Write),
            (Method::PUT, "/products/_bulk", Write),
            (Method::POST, "/_reindex", Write),
            (Method::POST, "/products/_update_by_query", Destructive),
            (Method::POST, "/products/_delete_by_query", Destructive),
            (Method::PUT, "/products", Ddl),
            (Method::PUT, "/products/_mapping", Ddl),
            (Method::PUT, "/products/_settings", Ddl),
            (Method::PUT, "/_settings", Ddl),
            (Method::POST, "/_aliases", Ddl),
            (Method::PUT, "/products/_alias/current", Ddl),
            (Method::PUT, "/_index_template/logs", Ddl),
            (Method::PUT, "/_ingest/pipeline/p", Ddl),
            (Method::PUT, "/_ilm/policy/p", Ddl),
            (Method::PUT, "/_plugins/_ism/policies/p", Ddl),
            (Method::PUT, "/_data_stream/logs-app", Ddl),
            (Method::POST, "/products/_open", Ddl),
            (Method::POST, "/logs-app/_rollover", Ddl),
            (Method::DELETE, "/products", Destructive),
            (Method::DELETE, "/_all", Destructive),
            (Method::DELETE, "/logs-*", Destructive),
            (Method::POST, "/products/_close", Destructive),
            (Method::DELETE, "/_index_template/logs", Destructive),
            (Method::DELETE, "/_ingest/pipeline/p", Destructive),
            (Method::DELETE, "/_data_stream/logs-app", Destructive),
            (Method::DELETE, "/_snapshot/repo/snap1", Destructive),
            (Method::DELETE, "/_snapshot/repo", Destructive),
            (Method::DELETE, "/_plugins/_ism/policies/p", Destructive),
            (Method::PUT, "/_cluster/settings", Admin),
            (Method::POST, "/_cluster/reroute", Admin),
            (Method::POST, "/_tasks/node:1/_cancel", Admin),
            (Method::PUT, "/_nodes/n1/shutdown", Admin),
            (Method::POST, "/products/_forcemerge", Admin),
            (Method::POST, "/products/_refresh", Admin),
            (Method::GET, "/products/_refresh", Admin),
            (Method::GET, "/_flush", Admin),
            (Method::POST, "/products/_cache/clear", Admin),
            (Method::POST, "/products/_disk_usage?run_expensive_tasks=true", Admin),
            (Method::PUT, "/_snapshot/repo", Ddl),
            (Method::PUT, "/_snapshot/repo/snap1", Admin),
            (Method::POST, "/_snapshot/repo/snap1/_restore", Admin),
            (Method::POST, "/_ilm/stop", Admin),
            (Method::POST, "/_security/user/bob", Admin),
            (Method::POST, "/_something_new", Admin),
            (Method::POST, "/products", Admin),
            (Method::PATCH, "/products/_doc/1", Write),
            (Method::PUT, "/products/legacy_type/1", Admin),
        ];
        for (method, path, expected) in cases {
            assert_eq!(class(method.clone(), path), expected, "{method} {path}");
        }
    }

    #[test]
    fn the_spec_dangerous_requests_are_destructive_or_admin() {
        for (method, path) in [
            (Method::DELETE, "/products"),
            (Method::POST, "/products/_delete_by_query"),
            (Method::POST, "/products/_update_by_query"),
            (Method::PUT, "/_cluster/settings"),
        ] {
            assert!(
                matches!(class(method.clone(), path), RequestClass::Destructive | RequestClass::Admin),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn the_api_is_read_from_its_position_not_from_any_segment() {
        use RequestClass::*;
        // A document id that looks like a read API is still a document write.
        assert_eq!(class(Method::PUT, "/products/_doc/_search"), Write);
        assert_eq!(class(Method::POST, "/products/_doc/_count"), Write);
        assert_eq!(class(Method::DELETE, "/products/_doc/_search"), Write);
        assert_eq!(class(Method::POST, "/products/_update/_mget"), Write);
        // A pipeline named like an API is a pipeline.
        assert_eq!(class(Method::DELETE, "/_ingest/pipeline/_search"), Destructive);
        // A read API after the target, with names after it, stays a read.
        assert_eq!(class(Method::GET, "/products/_explain/1"), Read);
        assert_eq!(class(Method::POST, "/products/_termvectors/1"), Read);
    }

    #[test]
    fn paths_the_client_would_rewrite_are_refused() {
        // Each of these would reach the cluster as a different, more dangerous request.
        for (method, path) in [
            (Method::DELETE, "/_count/../products"),
            (Method::POST, "/_search/../products/_delete_by_query"),
            (Method::DELETE, "/products#/_search"),
            (Method::DELETE, "/_search/%2e%2e/products"),
            (Method::DELETE, "/_search/.%2E/products"),
            (Method::DELETE, "/_search/%2E./products"),
            (Method::DELETE, "/_count/./../products"),
            (Method::DELETE, "/_search\\..\\products"),
            (Method::DELETE, "/_search/products%2F..%2Fx"),
            (Method::POST, "/products/_search\t/../_delete_by_query"),
            (Method::DELETE, "/products /_search"),
            (Method::DELETE, "/prod%ZZucts"),
            (Method::DELETE, "/prod%FFucts"),
            (Method::DELETE, "/products/{x}"),
            (Method::DELETE, "/./products"),
            (Method::DELETE, "/products/."),
        ] {
            let reason = refused(method.clone(), path);
            assert!(!reason.is_empty(), "{method} {path}");
        }
        // The query string is not part of the path check.
        assert_eq!(class(Method::GET, "/_search?q=a/../b#c"), RequestClass::Read);
    }

    #[test]
    fn percent_encoded_api_names_are_decoded_like_the_server_does() {
        use RequestClass::*;
        assert_eq!(class(Method::POST, "/products/%5Fdelete_by_query"), Destructive);
        assert_eq!(class(Method::POST, "/products/%5fupdate_by_query"), Destructive);
        assert_eq!(class(Method::DELETE, "/%70roducts"), Destructive);
        assert_eq!(class(Method::PUT, "/%5Fcluster/settings"), Admin);
        let target = classify(&Method::DELETE, "/logs%2D2026", None);
        assert_eq!(target.target.as_deref(), Some("logs-2026"));
    }

    #[test]
    fn classifies_sql_by_statement() {
        let select = json!({ "query": "SELECT * FROM products" });
        assert_eq!(
            classify(&Method::POST, "/_sql?format=json", Some(&select)).class,
            RequestClass::Read
        );
        let show = json!({ "query": "show tables" });
        assert_eq!(classify(&Method::POST, "/_plugins/_sql", Some(&show)).class, RequestClass::Read);
        let delete = json!({ "query": "DELETE FROM products WHERE 1=1" });
        assert_eq!(classify(&Method::POST, "/_plugins/_sql", Some(&delete)).class, RequestClass::Write);
        let commented = json!({ "query": "/* x */ DELETE FROM products" });
        assert_eq!(
            classify(&Method::POST, "/_plugins/_sql", Some(&commented)).class,
            RequestClass::Write
        );
        assert_eq!(classify(&Method::POST, "/_plugins/_ppl", None).class, RequestClass::Read);
        let target = classify(&Method::DELETE, "/logs-*", None);
        assert_eq!((target.api.as_str(), target.target.as_deref()), ("delete index", Some("logs-*")));
    }

    #[test]
    fn aliases_that_remove_an_index_are_destructive() {
        let add = json!({ "actions": [{ "add": { "index": "a", "alias": "b" } }] });
        assert_eq!(classify(&Method::POST, "/_aliases", Some(&add)).class, RequestClass::Ddl);
        let remove = json!({ "actions": [{ "add": { "index": "a", "alias": "b" } }, { "remove_index": { "index": "old" } }] });
        let classification = classify(&Method::POST, "/_aliases", Some(&remove));
        assert_eq!(classification.class, RequestClass::Destructive);
        assert!(classification.reason.is_some());
    }
}
