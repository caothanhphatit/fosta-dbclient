//! Request classification for the statement gateway, from the HTTP method and path (the body only matters for SQL).
//!
//! | Class         | Requests                                                                                              |
//! |---------------|-------------------------------------------------------------------------------------------------------|
//! | `Read`        | every GET/HEAD; `_search`, `_msearch`, `_count`, `_validate`, `_explain`, `_field_caps`, `_analyze`, ingest `_simulate`, `_mget`, `_termvectors`, PIT open/close, scroll, `_query` (ES\|QL), `_sql` SELECT/SHOW/DESCRIBE, `_eql`, async search, `_cluster/allocation/explain` |
//! | `Write`       | `_doc`, `_create`, `_update`, `_bulk`, `_update_by_query`, `_reindex`, DELETE of one document, POST `_aliases`-free data writes |
//! | `Ddl`         | PUT `<index>`, `_mapping`, `_settings`, `_alias(es)`, index / component / legacy templates, ingest pipelines, ILM / ISM policies, data stream create, rollover, shrink/split/clone, `_open` |
//! | `Destructive` | DELETE `<index>`, `_delete_by_query`, `_close`, DELETE of templates, pipelines, policies, data streams, snapshots and repositories |
//! | `Admin`       | `_cluster/settings`, reroute, voting exclusions, node shutdown, `_tasks/.../_cancel`, snapshot create/restore, refresh/flush/forcemerge/cache clear, `_security`, `_license`, ILM/ISM start/stop, anything not recognised |

use reqwest::Method;
use serde::{Deserialize, Serialize};

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
}

/// Classifies a request. `path` may include a query string; `body` is used to tell SQL reads from other statements.
pub fn classify(method: &Method, path: &str, body: Option<&serde_json::Value>) -> RequestClassification {
    let path = path.split('?').next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    let apis: Vec<&str> = segments.iter().copied().filter(|segment| segment.starts_with('_')).collect();
    let target = segments
        .first()
        .filter(|segment| !segment.starts_with('_'))
        .map(|segment| segment.to_string());
    let has = |name: &str| apis.contains(&name);
    let first = segments.first().copied().unwrap_or("");
    let done = |class: RequestClass, api: &str| RequestClassification {
        class,
        api: api.to_string(),
        target: target.clone(),
    };
    use RequestClass::*;

    // Read-only APIs whatever the method (search bodies are sent with POST).
    if has("_sql") || (has("_plugins") && segments.contains(&"_sql")) {
        let query = body
            .and_then(|body| body["query"].as_str())
            .unwrap_or("")
            .trim_start()
            .to_ascii_uppercase();
        let read = query.is_empty()
            || ["SELECT", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "WITH"]
                .iter()
                .any(|word| query.starts_with(word));
        return done(if read { Read } else { Write }, "sql");
    }
    let read_apis = [
        "_search",
        "_msearch",
        "_count",
        "_validate",
        "_explain",
        "_field_caps",
        "_analyze",
        "_simulate",
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
        "_ppl",
        "_disk_usage",
        "_resolve",
        "_health_report",
    ];
    if let Some(api) = apis.iter().find(|api| read_apis.contains(api)) {
        // `_search/point_in_time` (OpenSearch PIT), scroll and async search deletes only release resources.
        if !(*api == "_disk_usage" && *method != Method::GET) {
            return done(Read, api.trim_start_matches('_'));
        }
    }
    if first == "_cluster" && segments.get(1) == Some(&"allocation") {
        return done(Read, "allocation explain");
    }
    if *method == Method::GET || *method == Method::HEAD {
        return done(Read, apis.first().map(|api| api.trim_start_matches('_')).unwrap_or("get"));
    }

    // Document writes.
    if has("_delete_by_query") {
        return done(Destructive, "delete by query");
    }
    if has("_update_by_query") {
        return done(Write, "update by query");
    }
    if has("_reindex") {
        return done(Write, "reindex");
    }
    if has("_bulk") {
        return done(Write, "bulk");
    }
    if has("_doc") || has("_create") || has("_update") {
        return done(
            Write,
            if *method == Method::DELETE {
                "delete document"
            } else {
                "index document"
            },
        );
    }

    // Index lifecycle.
    if has("_close") {
        return done(Destructive, "close index");
    }
    if has("_open") || has("_rollover") || has("_shrink") || has("_split") || has("_clone") || has("_unfreeze") {
        return done(Ddl, apis.last().map(|api| api.trim_start_matches('_')).unwrap_or("index"));
    }
    if has("_refresh") || has("_flush") || has("_forcemerge") || has("_cache") || has("_upgrade") || has("_block") {
        return done(Admin, apis.last().map(|api| api.trim_start_matches('_')).unwrap_or("maintenance"));
    }
    if has("_mapping") || (has("_settings") && first != "_cluster") {
        return done(Ddl, if has("_mapping") { "put mapping" } else { "put settings" });
    }
    if has("_alias") || has("_aliases") {
        return done(Ddl, "aliases");
    }

    // Catalog objects: templates, pipelines, lifecycle policies, data streams, snapshots.
    let deleting = *method == Method::DELETE;
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
    if let Some((_, api)) = object_apis.iter().find(|(name, _)| first == *name) {
        if first == "_ilm" && matches!(segments.get(1), Some(&"start") | Some(&"stop")) {
            return done(Admin, "ILM start/stop");
        }
        if first == "_enrich" && segments.last() == Some(&"_execute") {
            return done(Write, "enrich execute");
        }
        return done(if deleting { Destructive } else { Ddl }, api);
    }
    if first == "_plugins" && segments.get(1) == Some(&"_ism") {
        return match segments.get(2) {
            Some(&"policies") => done(if deleting { Destructive } else { Ddl }, "ISM policy"),
            _ => done(Admin, "ISM"),
        };
    }
    if first == "_snapshot" {
        return match (segments.len(), deleting, segments.last()) {
            (_, _, Some(&"_restore")) => done(Admin, "snapshot restore"),
            (_, true, _) => done(
                Destructive,
                if segments.len() > 2 {
                    "delete snapshot"
                } else {
                    "delete repository"
                },
            ),
            (2, false, _) => done(Ddl, "snapshot repository"),
            _ => done(Admin, "snapshot"),
        };
    }

    // Creating or deleting an index (PUT /<index>, DELETE /<index>).
    if target.is_some() && apis.is_empty() {
        if deleting {
            return done(Destructive, "delete index");
        }
        if *method == Method::PUT {
            return done(Ddl, "create index");
        }
    }

    // Cluster and node administration, and everything not recognised.
    let api = match first {
        "_cluster" => "cluster",
        "_nodes" => "nodes",
        "_tasks" => "tasks",
        "_security" => "security",
        "_license" => "license",
        _ => "unknown",
    };
    done(Admin, api)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn class(method: Method, path: &str) -> RequestClass {
        classify(&method, path, None).class
    }

    #[test]
    fn classifies_requests() {
        use RequestClass::*;
        let cases = [
            (Method::GET, "/products/_search", Read),
            (Method::POST, "/products/_search?typed_keys=true", Read),
            (Method::POST, "/_query", Read),
            (Method::POST, "/products/_count", Read),
            (Method::POST, "/_ingest/pipeline/p/_simulate", Read),
            (Method::POST, "/_analyze", Read),
            (Method::POST, "/products/_pit?keep_alive=1m", Read),
            (Method::DELETE, "/_pit", Read),
            (Method::DELETE, "/_search/point_in_time", Read),
            (Method::GET, "/_cat/indices", Read),
            (Method::HEAD, "/products", Read),
            (Method::POST, "/products/_doc", Write),
            (Method::PUT, "/products/_doc/1", Write),
            (Method::DELETE, "/products/_doc/1", Write),
            (Method::POST, "/products/_update/1", Write),
            (Method::POST, "/_bulk", Write),
            (Method::POST, "/products/_update_by_query", Write),
            (Method::POST, "/_reindex", Write),
            (Method::POST, "/products/_delete_by_query", Destructive),
            (Method::PUT, "/products", Ddl),
            (Method::PUT, "/products/_mapping", Ddl),
            (Method::PUT, "/products/_settings", Ddl),
            (Method::POST, "/_aliases", Ddl),
            (Method::PUT, "/_index_template/logs", Ddl),
            (Method::PUT, "/_ingest/pipeline/p", Ddl),
            (Method::PUT, "/_ilm/policy/p", Ddl),
            (Method::PUT, "/_plugins/_ism/policies/p", Ddl),
            (Method::POST, "/products/_open", Ddl),
            (Method::DELETE, "/products", Destructive),
            (Method::POST, "/products/_close", Destructive),
            (Method::DELETE, "/_index_template/logs", Destructive),
            (Method::DELETE, "/_ingest/pipeline/p", Destructive),
            (Method::DELETE, "/_data_stream/logs-app", Destructive),
            (Method::DELETE, "/_snapshot/repo/snap1", Destructive),
            (Method::PUT, "/_cluster/settings", Admin),
            (Method::POST, "/_tasks/node:1/_cancel", Admin),
            (Method::PUT, "/_nodes/n1/shutdown", Admin),
            (Method::POST, "/products/_forcemerge", Admin),
            (Method::POST, "/products/_refresh", Admin),
            (Method::PUT, "/_snapshot/repo/snap1", Admin),
            (Method::POST, "/_snapshot/repo/snap1/_restore", Admin),
            (Method::POST, "/_ilm/stop", Admin),
            (Method::POST, "/_something_new", Admin),
        ];
        for (method, path, expected) in cases {
            assert_eq!(class(method.clone(), path), expected, "{method} {path}");
        }
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
        let target = classify(&Method::DELETE, "/logs-*", None);
        assert_eq!((target.api.as_str(), target.target.as_deref()), ("delete index", Some("logs-*")));
    }
}
