//! The vector suite: `vector.*` operations through the statement gateway (as the vector views call them) against a
//! table with a vector column. An engine's `contract_tests.rs` calls [`suite_vector`] from a test named
//! `contract_<engine>_vector` with its [`Fixture`] and a [`VectorFixture`]. Recall runs with a fixed seed, so the
//! queries are the same on every run.

use serde_json::{json, Value as Json};

use super::suites::{connect, Fixture, CONNECTION};
use crate::{
    contract::Capability,
    error::AppResult,
    gateway::{
        self,
        rules::{Preset, SafetySettings},
        Caller, Context,
    },
    runtime::ConnectionManager,
};

/// A table with a vector column and at least 10 rows, all with a vector.
pub struct VectorFixture {
    pub database: &'static str,
    /// As the table view sends it (`products`, or `public.products` for engines with schemas).
    pub table: &'static str,
    pub column: &'static str,
    pub dims: u32,
    /// The integer primary key and one key that exists.
    pub key_column: &'static str,
    pub key: i64,
    /// Whether the column has a vector index (recall needs one).
    pub indexed: bool,
}

/// What the suite saw, for engine-specific checks.
pub struct VectorRun {
    pub info: Json,
    /// A cosine search for the row `key`.
    pub search: Json,
}

fn context() -> Context {
    Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(Preset::ReadOnly),
    }
}

async fn invoke(manager: &ConnectionManager, name: &str, args: Json) -> AppResult<Json> {
    gateway::invoke(manager, &context(), Caller::EngineView, name, args, None).await
}

fn float(value: &Json) -> f64 {
    value["v"]
        .as_f64()
        .or_else(|| value["v"].as_str().and_then(|text| text.parse().ok()))
        .unwrap_or_else(|| panic!("not a float cell: {value}"))
}

/// Runs the suite; `None` when the server has no vector support (printed, so CI shows the skip).
pub async fn suite_vector(f: &Fixture, v: &VectorFixture) -> Option<VectorRun> {
    let (manager, handle) = connect(f).await;
    if !handle.conn.capabilities().contains(Capability::Vector) {
        println!("{}: no vector support on this server; skipped", f.engine);
        manager.disconnect(CONNECTION).await;
        return None;
    }
    let engine = f.engine;
    let table = json!({ "database": v.database, "table": v.table });
    let target = |extra: Json| {
        let mut args = json!({ "database": v.database, "table": v.table, "column": v.column });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };

    // Info: the column with its dimensions, the index and the metrics.
    let info = invoke(&manager, "vector.info", table).await.unwrap();
    assert_eq!(info["supported"], true, "{engine}: {info}");
    let column = info["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|column| column["name"] == v.column)
        .unwrap_or_else(|| panic!("{engine}: {} not in {info}", v.column));
    assert_eq!(column["dims"], v.dims, "{engine}");
    assert!(!column["typeName"].as_str().unwrap().is_empty(), "{engine}");
    let metrics: Vec<String> = info["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_owned())
        .collect();
    assert!(metrics.iter().any(|metric| metric == "cosine"), "{engine}: {metrics:?}");
    // A column whose similarity is fixed lists its own metrics; searches use only those.
    let fixed: Vec<String> = column["metrics"]
        .as_array()
        .map(|list| list.iter().map(|m| m.as_str().unwrap().to_owned()).collect())
        .unwrap_or_default();
    let refused_metric = metrics.iter().find(|metric| !fixed.is_empty() && !fixed.contains(metric)).cloned();
    let metrics = if fixed.is_empty() { metrics } else { fixed };
    if v.indexed {
        let index = info["indexes"].as_array().unwrap().iter().find(|index| index["column"] == v.column);
        let index = index.unwrap_or_else(|| panic!("{engine}: no index in {info}"));
        assert!(!index["metric"].as_str().unwrap().is_empty(), "{engine}");
        assert!(!index["kind"].as_str().unwrap().is_empty(), "{engine}");
    }

    // Fetch: one row's vector, by typed key cells.
    let key = json!([{ "column": v.key_column, "value": { "t": "int", "v": v.key } }]);
    let fetched = invoke(&manager, "vector.fetch", target(json!({ "key": key }))).await.unwrap();
    let values = fetched.as_array().unwrap();
    assert_eq!(values.len(), v.dims as usize, "{engine}");
    let literal = format!(
        "[{}]",
        values
            .iter()
            .map(|value| value.as_f64().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let missing = json!([{ "column": v.key_column, "value": { "t": "int", "v": -424242 } }]);
    assert!(
        invoke(&manager, "vector.fetch", target(json!({ "key": missing }))).await.is_err(),
        "{engine}"
    );

    // Search: k rows ordered by distance, the stored vector itself (distance 0) first, for every metric.
    let search = |query: Json, metric: &str| target(json!({ "query": query, "k": 5, "metric": metric, "filter": null }));
    let by_row = invoke(&manager, "vector.search", search(json!({ "kind": "row", "key": key }), "cosine"))
        .await
        .unwrap();
    for metric in std::iter::once("auto").chain(metrics.iter().map(String::as_str)) {
        let found = invoke(
            &manager,
            "vector.search",
            search(json!({ "kind": "literal", "text": literal }), metric),
        )
        .await
        .unwrap_or_else(|error| panic!("{engine} {metric}: {error}"));
        let columns = found["result"]["columns"].as_array().unwrap();
        assert_eq!(columns[0]["name"], "vec_distance", "{engine}");
        assert_eq!(columns[0]["kind"]["kind"], "float", "{engine} {metric}");
        let rows = found["result"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5, "{engine} {metric}");
        let distances: Vec<f64> = rows.iter().map(|row| float(&row[0])).collect();
        assert!(
            distances.windows(2).all(|pair| pair[0] <= pair[1] + 1e-9),
            "{engine} {metric}: {distances:?}"
        );
        if metric != "dot" {
            assert!(distances[0].abs() < 1e-4, "{engine} {metric}: nearest {distances:?}");
        }
        assert_eq!(found["queryDims"], v.dims, "{engine}");
        assert!(
            found["primaryKey"].as_array().unwrap().iter().any(|name| name == v.key_column),
            "{engine}"
        );
        assert!(!found["sql"].as_str().unwrap().is_empty(), "{engine}");
    }
    let first = float(&by_row["result"]["rows"][0][0]);
    assert!(first.abs() < 1e-4, "{engine}: {by_row}");
    let wrong = invoke(
        &manager,
        "vector.search",
        search(json!({ "kind": "literal", "text": "[1,2,3,4,5,6,7]" }), "auto"),
    )
    .await;
    assert!(
        wrong.expect_err("dimension mismatch").to_string().contains("dimensions"),
        "{engine}"
    );
    if let Some(metric) = refused_metric {
        let refused = invoke(
            &manager,
            "vector.search",
            search(json!({ "kind": "literal", "text": literal }), &metric),
        )
        .await;
        assert!(
            refused.expect_err("a metric outside the column's").to_string().contains("only"),
            "{engine} {metric}"
        );
    }

    // Health: the column, index, rows and sample checks.
    let health = invoke(&manager, "vector.health", target(json!({ "sample": 100 }))).await.unwrap();
    let checks = health.as_array().unwrap();
    assert!(checks.len() > 3, "{engine}: {health}");
    assert_eq!(checks[0]["label"], "Column", "{engine}");
    assert!(
        checks.iter().all(|check| check["status"].is_string() && check["value"].is_string()),
        "{engine}"
    );
    assert!(checks.iter().any(|check| check["label"] == "Sample"), "{engine}: {health}");

    // Recall, seeded: in [0, 1]; exact search against itself is exactly 1.
    if v.indexed {
        let recall = invoke(&manager, "vector.recall", target(json!({ "k": 5, "queries": 5, "seed": 7 })))
            .await
            .unwrap();
        let value = recall["recall"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&value), "{engine}: {recall}");
        assert!(recall["queries"].as_u64().unwrap() >= 1, "{engine}");
        let again = invoke(
            &manager,
            "vector.recall",
            target(json!({ "k": 5, "queries": 5, "seed": 7, "selfCheck": true })),
        )
        .await
        .unwrap();
        assert_eq!(again["recall"].as_f64(), Some(1.0), "{engine}: {again}");
        assert_eq!(again["worstRecall"].as_f64(), Some(1.0), "{engine}: {again}");
    }

    let unknown = invoke(&manager, "vector.drop", json!({})).await.expect_err("unknown operation");
    assert_eq!(unknown.code(), Some(gateway::BLOCKED), "{engine}");
    manager.disconnect(CONNECTION).await;
    Some(VectorRun { info, search: by_row })
}
