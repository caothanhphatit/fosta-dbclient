//! Grid filters. A SQL filter is spliced into `SELECT … WHERE <filter>`, so it must be one
//! boolean expression and nothing else: it is parsed as `SELECT 1 WHERE <filter>` and must
//! come back as exactly that one statement, without comments, semicolons, set operations,
//! `INTO`, `LIMIT`, locking clauses or functions that sleep, lock or read files.
//! A MongoDB filter must be a JSON query document without server-side JavaScript.

use std::ops::ControlFlow;

use serde::Serialize;
use sqlparser::{
    ast::{visit_expressions, visit_statements, Expr, SetExpr, Statement},
    tokenizer::{Token, Tokenizer, Whitespace},
};

use super::classify::{dialect_for, parse};
use crate::{
    drivers::Engine,
    error::{AppError, AppResult},
};

pub const INVALID_FILTER: &str = "INVALID_FILTER";

/// Functions a filter has no business calling: they stall the server, take locks or read files.
const DENIED_FUNCTIONS: [&str; 12] = [
    "sleep",
    "benchmark",
    "load_file",
    "get_lock",
    "release_lock",
    "release_all_locks",
    "master_pos_wait",
    "source_pos_wait",
    "openrowset",
    "opendatasource",
    "openquery",
    "xp_cmdshell",
];

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

/// Checks a grid filter for a SQL engine; `Ok` means it is one boolean expression.
pub fn validate_sql_filter(engine: Engine, filter: &str) -> AppResult<()> {
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
    let denied = visit_expressions(selection, |expr| match expr {
        Expr::Function(function) => {
            let name = function.name.to_string().to_lowercase();
            let short = name
                .rsplit('.')
                .next()
                .unwrap_or(&name)
                .trim_matches(|c| c == '`' || c == '[' || c == ']' || c == '"');
            if DENIED_FUNCTIONS.contains(&short) {
                ControlFlow::Break(short.to_owned())
            } else {
                ControlFlow::Continue(())
            }
        }
        _ => ControlFlow::Continue(()),
    });
    if let ControlFlow::Break(function) = denied {
        return Err(reject(filter, format!("{function}() is not allowed in a filter.")));
    }
    Ok(())
}

/// Comments and `;` are never part of a condition. MySQL runs the inside of `/*! … */`, which the
/// tokenizer expands, so those are caught on the raw text.
fn reject_comments_and_separators(engine: Engine, text: &str) -> Result<(), String> {
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

pub fn validate_filter(engine: Engine, filter: Option<&str>) -> AppResult<()> {
    match (engine, filter) {
        (_, None) => Ok(()),
        (Engine::MongoDb, Some(filter)) => validate_mongo_filter(filter),
        (engine, Some(filter)) => validate_sql_filter(engine, filter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQL_ENGINES: [Engine; 3] = [Engine::MariaDb, Engine::MySql, Engine::SqlServer];

    fn ok(engine: Engine, filter: &str) {
        if let Err(error) = validate_sql_filter(engine, filter) {
            panic!("{engine:?} rejected {filter:?}: {error}");
        }
    }

    fn rejected(engine: Engine, filter: &str) -> String {
        match validate_sql_filter(engine, filter) {
            Ok(()) => panic!("{engine:?} accepted {filter:?}"),
            Err(error) => {
                assert_eq!(error.code(), Some(INVALID_FILTER));
                error.to_string()
            }
        }
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
        ok(Engine::MariaDb, "`category_id` = 3 AND JSON_VALUE(attributes, '$.color') = 'oak'");
        ok(Engine::SqlServer, "[Status] = N'paid' AND YEAR(InvoiceDate) = 2026");
        // What the grid's column filters build for SQL Server (src/core/sql.ts, tsql dialect).
        ok(
            Engine::SqlServer,
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
        for engine in [Engine::MariaDb, Engine::MySql] {
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
