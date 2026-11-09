//! Grid filters. A SQL filter is spliced into `SELECT … WHERE <filter>`, so it must be one
//! boolean expression and nothing else: it is parsed as `SELECT 1 WHERE <filter>` and must
//! come back as exactly that one statement, without comments, semicolons, set operations,
//! `INTO`, `LIMIT`, locking clauses or functions with side effects (the classifier's deny-list: they sleep, lock,
//! read files, change server state or reach other servers).
//! A MongoDB filter must be a JSON query document without server-side JavaScript.

use std::ops::ControlFlow;

use serde::Serialize;
use sqlparser::{
    ast::{visit_statements, SetExpr, Statement},
    tokenizer::{Token, Tokenizer, Whitespace},
};

use super::classify::{dialect_for, parse};
use crate::engines::common::sqlclassify::side_effect_call;
use crate::{
    contract::{RawFilterLang, SqlDialectId},
    error::{AppError, AppResult},
};

pub const INVALID_FILTER: &str = "INVALID_FILTER";

/// MongoDB operators that run JavaScript on the server.
const DENIED_MONGO_OPERATORS: [&str; 3] = ["$where", "$function", "$accumulator"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FilterProblem<'a> {
    code: &'static str,
    filter: &'a str,
}

fn reject(filter: &str, message: impl Into<String>) -> AppError {
    AppError::structured(
        message,
        FilterProblem {
            code: INVALID_FILTER,
            filter,
        },
    )
}

/// One reading of a grid filter: one boolean expression, nothing else.
fn check_sql_filter(engine: SqlDialectId, filter: &str) -> AppResult<()> {
    let text = filter.trim();
    if text.is_empty() {
        return Ok(());
    }
    reject_comments_and_separators(engine, text).map_err(|message| reject(filter, message))?;
    let wrapped = format!("SELECT 1 WHERE {text}");
    let statements = parse(engine, &wrapped).map_err(|error| {
        reject(
            filter,
            format!("The filter is not a valid condition: {error}. Write it like the part after WHERE, e.g. status = 'paid'."),
        )
    })?;
    let [statement] = statements.as_slice() else {
        return Err(reject(
            filter,
            "The filter must be a single condition; several statements are not allowed.",
        ));
    };
    let Statement::Query(query) = statement else {
        return Err(reject(filter, "The filter must be a condition, not a statement."));
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(reject(
            filter,
            "The filter must be a single condition (UNION and the like are not allowed).",
        ));
    };
    let Some(selection) = select.selection.as_ref() else {
        return Err(reject(filter, "The filter must be a condition."));
    };
    // Anything beyond the condition (GROUP BY, LIMIT, INTO, FOR UPDATE, ORDER BY…) shows up as a
    // difference between the parsed statement and the bare `SELECT 1 WHERE <condition>`.
    if statement.to_string() != format!("SELECT 1 WHERE {selection}") {
        return Err(reject(
            filter,
            "The filter must be only a condition; clauses such as LIMIT, GROUP BY, INTO or FOR UPDATE are not allowed.",
        ));
    }
    let mut nested = 0;
    let _ = visit_statements(statement, |_| {
        nested += 1;
        ControlFlow::<()>::Continue(())
    });
    if nested != 1 {
        return Err(reject(filter, "The filter must not contain statements."));
    }
    if let Some(function) = side_effect_call(selection) {
        return Err(reject(filter, format!("{function}() is not allowed in a filter.")));
    }
    Ok(())
}

/// Checks a grid filter for a SQL engine; `Ok` means it is one boolean expression. For MariaDB/MySQL the check also runs
/// with every backslash taken literally, since a server whose `sql_mode` has `NO_BACKSLASH_ESCAPES` ends strings where the
/// default rules see an escaped quote.
pub fn validate_sql_filter(engine: SqlDialectId, filter: &str) -> AppResult<()> {
    check_sql_filter(engine, filter)?;
    if matches!(engine, SqlDialectId::MariaDb | SqlDialectId::MySql) && filter.contains('\\') {
        check_sql_filter(engine, &filter.replace('\\', "\\\\")).map_err(|_| {
            reject(
                filter,
                "The filter has a backslash before a quote, which some server settings do not treat as an escape. Double the quote instead ('').",
            )
        })?;
    }
    Ok(())
}

/// Comments and `;` are never part of a condition. MySQL runs the inside of `/*! … */`, which the
/// tokenizer expands, so those are caught on the raw text.
fn reject_comments_and_separators(engine: SqlDialectId, text: &str) -> Result<(), String> {
    if text.contains("/*!") || text.contains("/*M!") {
        return Err("Comments are not allowed in a filter.".to_owned());
    }
    let dialect = dialect_for(engine);
    let tokens = Tokenizer::new(dialect.as_ref(), text)
        .tokenize()
        .map_err(|error| format!("The filter is not a valid condition: {error}"))?;
    for token in tokens {
        match token {
            Token::Whitespace(Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)) => {
                return Err("Comments are not allowed in a filter.".to_owned());
            }
            Token::SemiColon => return Err("A filter is one condition; ';' is not allowed.".to_owned()),
            _ => {}
        }
    }
    Ok(())
}

/// Checks a MongoDB grid filter: a JSON object without server-side JavaScript operators.
pub fn validate_mongo_filter(filter: &str) -> AppResult<()> {
    let text = filter.trim();
    if text.is_empty() {
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        reject(
            filter,
            format!("Filter must be a JSON query document, e.g. {{ \"status\": \"ACTIVE\" }}. ({error})"),
        )
    })?;
    if !value.is_object() {
        return Err(reject(
            filter,
            "Filter must be a JSON query document (an object), e.g. { \"status\": \"ACTIVE\" }.",
        ));
    }
    if let Some(operator) = find_denied_operator(&value) {
        return Err(reject(
            filter,
            format!("{operator} runs JavaScript on the server and is not allowed in a filter."),
        ));
    }
    Ok(())
}

fn find_denied_operator(value: &serde_json::Value) -> Option<&'static str> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, inner)| {
            DENIED_MONGO_OPERATORS
                .iter()
                .find(|operator| **operator == key)
                .copied()
                .or_else(|| find_denied_operator(inner))
        }),
        serde_json::Value::Array(items) => items.iter().find_map(find_denied_operator),
        _ => None,
    }
}

/// Checks a raw filter in the engine's filter language: SQL must be one condition (parsed in the engine's dialect),
/// MongoDB and Elasticsearch filters must be a JSON object without code-running operators.
pub fn validate_raw_filter(language: RawFilterLang, dialect: Option<SqlDialectId>, text: &str) -> AppResult<()> {
    match language {
        RawFilterLang::Sql => validate_sql_filter(dialect.unwrap_or(SqlDialectId::Generic), text),
        RawFilterLang::MongoJson => validate_mongo_filter(text),
        RawFilterLang::EsQueryDsl => validate_es_filter(text),
    }
}

/// Elasticsearch Query DSL: a JSON object, and nothing that runs code or smuggles a query past this check
/// (`script`, any `*_script` key such as `script_score` or `minimum_should_match_script`, `runtime_mappings`, `wrapper`).
pub fn validate_es_filter(filter: &str) -> AppResult<()> {
    let value: serde_json::Value =
        serde_json::from_str(filter).map_err(|error| reject(filter, format!("The filter is not valid JSON: {error}")))?;
    if !value.is_object() {
        return Err(reject(filter, "The filter must be a JSON object (a Query DSL query)."));
    }
    if let Some(key) = find_key(&value, &|key| {
        key == "script" || key.ends_with("_script") || key.starts_with("script_") || key == "runtime_mappings" || key == "wrapper"
    }) {
        return Err(reject(
            filter,
            format!("'{key}' is not allowed in a filter: it runs code on the cluster."),
        ));
    }
    Ok(())
}

fn find_key<'a>(value: &'a serde_json::Value, denied: &dyn Fn(&str) -> bool) -> Option<&'a str> {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .find_map(|(key, inner)| denied(key).then_some(key.as_str()).or_else(|| find_key(inner, denied))),
        serde_json::Value::Array(items) => items.iter().find_map(|item| find_key(item, denied)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQL_ENGINES: [SqlDialectId; 3] = [SqlDialectId::MariaDb, SqlDialectId::MySql, SqlDialectId::MsSql];

    fn ok(engine: SqlDialectId, filter: &str) {
        if let Err(error) = validate_sql_filter(engine, filter) {
            panic!("{engine:?} rejected {filter:?}: {error}");
        }
    }

    fn rejected(engine: SqlDialectId, filter: &str) -> String {
        match validate_sql_filter(engine, filter) {
            Ok(()) => panic!("{engine:?} accepted {filter:?}"),
            Err(error) => {
                assert_eq!(error.code(), Some(INVALID_FILTER));
                error.to_string()
            }
        }
    }

    #[test]
    fn es_filters_are_objects_without_scripts() {
        assert!(validate_raw_filter(RawFilterLang::EsQueryDsl, None, r#"{ "term": { "status": "paid" } }"#).is_ok());
        let script = validate_raw_filter(
            RawFilterLang::EsQueryDsl,
            None,
            r#"{ "bool": { "filter": [ { "script": { "script": "1" } } ] } }"#,
        );
        assert_eq!(script.unwrap_err().code(), Some(INVALID_FILTER));
        assert!(validate_raw_filter(RawFilterLang::EsQueryDsl, None, "[1]").is_err());
        for smuggled in [
            r#"{ "wrapper": { "query": "eyJtYXRjaF9hbGwiOnt9fQ==" } }"#,
            r#"{ "terms_set": { "tags": { "terms": ["a"], "minimum_should_match_script": { "source": "1" } } } }"#,
            r#"{ "function_score": { "script_score": { "script": "1" } } }"#,
            r#"{ "bool": { "must": [ { "match_all": {} } ] }, "runtime_mappings": {} }"#,
        ] {
            let error = validate_raw_filter(RawFilterLang::EsQueryDsl, None, smuggled).unwrap_err();
            assert_eq!(error.code(), Some(INVALID_FILTER), "{smuggled}");
        }
        assert!(validate_raw_filter(RawFilterLang::Sql, Some(SqlDialectId::Postgres), "total > 10 AND name ILIKE 'a%'").is_ok());
        assert!(validate_raw_filter(RawFilterLang::Sql, Some(SqlDialectId::Sqlite), "1=1; DROP TABLE t").is_err());
    }

    #[test]
    fn ordinary_conditions_pass() {
        for engine in SQL_ENGINES {
            ok(engine, "");
            ok(engine, "status = 'paid'");
            ok(engine, "stock < 5 AND (price > 10 OR name LIKE '%oak%')");
            ok(engine, "id IN (SELECT order_id FROM order_items WHERE quantity > 2)");
            ok(engine, "created_at >= '2026-01-01' AND note IS NULL");
            ok(engine, "name = 'it''s -- not a comment; really'");
        }
        ok(
            SqlDialectId::MariaDb,
            "`category_id` = 3 AND JSON_VALUE(attributes, '$.color') = 'oak'",
        );
        ok(SqlDialectId::MsSql, "[Status] = N'paid' AND YEAR(InvoiceDate) = 2026");
        // What the grid's column filters build for SQL Server (src/core/sql.ts, tsql dialect).
        ok(
            SqlDialectId::MsSql,
            "([Status] LIKE N'%a[[]b%') AND ([Amount] > 5) AND ([Note] IS NULL)",
        );
    }

    #[test]
    fn stacked_statements_and_injections_are_rejected() {
        for engine in SQL_ENGINES {
            let message = rejected(engine, "1=0; DELETE FROM t; SELECT 1 WHERE 1");
            assert!(message.contains(';') || message.contains("single"), "{message}");
            rejected(engine, "1=1; DROP TABLE orders");
            rejected(engine, "1=1 -- trailing comment");
            rejected(engine, "1=1 /* comment */");
            rejected(engine, "1=1 UNION SELECT name FROM users");
            rejected(engine, "1=1 ORDER BY 1");
            rejected(engine, "DELETE FROM t");
            rejected(engine, "1=1) OR (1=1");
        }
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql] {
            rejected(engine, "1=1 /*! ; DROP TABLE t */");
            rejected(engine, "1=1 /*M! ; DROP TABLE t */");
            rejected(engine, "1=1 # hash comment");
            rejected(engine, "1=1 LIMIT 1");
            rejected(engine, "1=1 FOR UPDATE");
            rejected(engine, "1=1 INTO OUTFILE '/tmp/x'");
            rejected(engine, "SLEEP(10) = 0");
            rejected(engine, "id = 1 AND BENCHMARK(1000000, MD5('x'))");
            rejected(engine, "LOAD_FILE('/etc/passwd') IS NOT NULL");
        }
    }

    #[test]
    fn side_effect_functions_from_the_shared_list_are_rejected() {
        let pg = SqlDialectId::Postgres;
        let message = rejected(pg, "pg_catalog.pg_terminate_backend(42)");
        assert!(message.contains("pg_terminate_backend()"), "{message}");
        rejected(pg, "id = 1 AND pg_sleep(5) IS NOT NULL");
        rejected(pg, "id IN (SELECT a FROM dblink('host=x', 'DELETE FROM t') AS r(a int))");
        rejected(pg, "set_config('default_transaction_read_only', 'off', false) = 'off'");
        rejected(pg, "id = nextval('s')");
        ok(pg, "lower(name) = 'oak' AND id > 3");
        rejected(SqlDialectId::ClickHouse, "id IN (SELECT id FROM url('http://x', CSV, 'id UInt64'))");
        ok(SqlDialectId::ClickHouse, "url = 'http://x'");
        rejected(SqlDialectId::MsSql, "id IN (SELECT a FROM OPENQUERY(other, 'SELECT 1'))");
        rejected(SqlDialectId::Oracle, "UTL_HTTP.REQUEST('http://x') IS NOT NULL");
        ok(SqlDialectId::Oracle, "name = 'x'");
    }

    #[test]
    fn backslash_quotes_are_checked_both_ways_on_mysql() {
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql] {
            ok(engine, "name LIKE 'a\\_b%'");
            ok(engine, "path = 'C:\\\\temp'");
            rejected(engine, "name = 'a\\' OR 1=1; DROP TABLE t; -- '");
            rejected(engine, "name = 'a\\' OR sleep(5) = 0 OR name = '");
        }
        ok(SqlDialectId::MsSql, "path = 'C:\\temp\\'");
    }

    #[test]
    fn mongo_filters_must_be_query_documents() {
        assert!(validate_mongo_filter(r#"{ "type": "purchase", "n": { "$gt": 2 } }"#).is_ok());
        assert!(validate_mongo_filter("").is_ok());
        for bad in [
            "type = 'purchase'",
            "[1, 2]",
            r#"{ "$where": "sleep(1000)" }"#,
            r#"{ "$expr": { "$function": { "body": "x", "args": [], "lang": "js" } } }"#,
            r#"{ "$or": [ { "a": 1 }, { "$where": "true" } ] }"#,
        ] {
            let error = validate_mongo_filter(bad).expect_err(bad);
            assert_eq!(error.code(), Some(INVALID_FILTER));
        }
    }
}
