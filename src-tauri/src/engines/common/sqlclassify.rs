//! SQL statement classification with `sqlparser`, shared by the MariaDB/MySQL and SQL Server engines and the statement
//! gateway (`gateway::classify` re-exports it). SQL is parsed in the engine's dialect (MySQL for MariaDB/MySQL, MsSql
//! for SQL Server, Generic as a fallback) and every statement, including statements nested in CTEs, `EXPLAIN ANALYZE`
//! and control flow, gets a [`Category`].
//!
//! The classifier errs on the safe side: SQL it cannot parse is [`Category::Unknown`], which the rules treat like a
//! write, and MySQL `/*! … */` comments (which the server executes) are classified as code.

use std::ops::ControlFlow;

use sqlparser::{
    ast::{visit_relations, visit_statements, ObjectName, Query, Set, SetExpr, Statement, TransactionAccessMode, TransactionMode},
    dialect::{ClickHouseDialect, Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect},
    parser::Parser,
};

use crate::contract::SqlDialectId;

const EXCERPT_CHARS: usize = 240;

use crate::contract::classify::{Category, Classification, StatementInfo};

/// The sqlparser dialect for an engine's SQL; Oracle and anything else parse with the generic one.
pub fn dialect_for(dialect: SqlDialectId) -> Box<dyn Dialect> {
    match dialect {
        SqlDialectId::MariaDb | SqlDialectId::MySql => Box::new(MySqlDialect {}),
        SqlDialectId::MsSql => Box::new(MsSqlDialect {}),
        SqlDialectId::Postgres => Box::new(PostgreSqlDialect {}),
        SqlDialectId::Sqlite => Box::new(SQLiteDialect {}),
        SqlDialectId::ClickHouse => Box::new(ClickHouseDialect {}),
        SqlDialectId::Oracle | SqlDialectId::Generic => Box::new(GenericDialect {}),
    }
}

/// MariaDB's `/*M! … */` comments run like MySQL's `/*! … */`; the parser only knows the latter.
pub fn normalise_executable_comments(engine: SqlDialectId, text: &str) -> String {
    match engine {
        SqlDialectId::MariaDb | SqlDialectId::MySql => text.replace("/*M!", "/*!"),
        _ => text.to_owned(),
    }
}

/// Parses `text` in the engine's dialect, falling back to the generic one.
pub fn parse(engine: SqlDialectId, text: &str) -> Result<Vec<Statement>, String> {
    let text = normalise_executable_comments(engine, text);
    match Parser::parse_sql(dialect_for(engine).as_ref(), &text) {
        Ok(statements) => Ok(statements),
        Err(error) => Parser::parse_sql(&GenericDialect {}, &text).map_err(|_| error.to_string()),
    }
}

/// Classifies SQL typed in a query tab (or built by the app).
pub fn classify_sql(engine: SqlDialectId, text: &str) -> Classification {
    if text.trim().is_empty() {
        return Classification::default();
    }
    match parse(engine, text) {
        Ok(statements) => Classification {
            statements: statements.iter().map(classify_statement).collect(),
        },
        Err(error) => {
            let category = unparsed_category(text);
            let reason = if category == Category::Unknown {
                format!("The statement could not be parsed, so it is treated like a write ({error})")
            } else {
                format!(
                    "The statement could not be parsed; its keywords make it {} ({error})",
                    category.label()
                )
            };
            Classification::single(StatementInfo {
                category,
                kind: "unparsed".to_owned(),
                tables: Vec::new(),
                excerpt: excerpt(text),
                reason: Some(reason),
                refused: None,
            })
        }
    }
}

/// SQL the parser does not understand is at least [`Category::Unknown`]; leading keywords of
/// administrative or destructive statements raise it further (never lower).
fn unparsed_category(text: &str) -> Category {
    const ADMIN: [&str; 12] = [
        "CREATE USER",
        "ALTER USER",
        "DROP USER",
        "RENAME USER",
        "CREATE ROLE",
        "DROP ROLE",
        "GRANT",
        "REVOKE",
        "SET PASSWORD",
        "KILL",
        "SHUTDOWN",
        "INSTALL",
    ];
    const DESTRUCTIVE: [&str; 3] = ["DROP", "TRUNCATE", "DELETE"];
    let mut category = Category::Unknown;
    for chunk in text.split(';') {
        let words: Vec<String> = chunk.split_whitespace().take(2).map(str::to_uppercase).collect();
        let lead = words.join(" ");
        if ADMIN.iter().any(|keyword| lead.starts_with(keyword)) {
            category = category.max(Category::Admin);
        } else if DESTRUCTIVE.iter().any(|keyword| words.first().is_some_and(|word| word == keyword)) {
            category = category.max(Category::Destructive);
        }
    }
    category
}

fn classify_statement(statement: &Statement) -> StatementInfo {
    let (mut category, mut reason) = own_category(statement);
    // Writes hidden inside the statement: CTEs (`WITH … DELETE`), `EXPLAIN ANALYZE`, IF/WHILE blocks.
    let explain_only = matches!(statement, Statement::Explain { analyze: false, .. });
    if !explain_only {
        let _ = visit_statements(statement, |inner| {
            if !std::ptr::eq(inner, statement) {
                let (inner_category, inner_reason) = own_category(inner);
                if inner_category > category {
                    category = inner_category;
                    reason = inner_reason.or_else(|| Some(format!("contains {}", kind_of(inner))));
                }
            }
            ControlFlow::<()>::Continue(())
        });
    }
    StatementInfo {
        category,
        kind: kind_of(statement),
        tables: tables_of(statement),
        excerpt: excerpt(&statement.to_string()),
        reason,
        refused: None,
    }
}

/// The category of one statement node, ignoring nested statements.
fn own_category(statement: &Statement) -> (Category, Option<String>) {
    match statement {
        Statement::Query(query) => query_category(query),
        Statement::Insert(_) | Statement::Merge(_) => (Category::Write, None),
        Statement::Update(update) if update.selection.is_none() => {
            (Category::Destructive, Some("UPDATE without WHERE changes every row".to_owned()))
        }
        Statement::Delete(delete) if delete.selection.is_none() => {
            (Category::Destructive, Some("DELETE without WHERE removes every row".to_owned()))
        }
        Statement::Update(_) | Statement::Delete(_) => (Category::Write, None),
        Statement::Truncate(_) => (Category::Destructive, Some("TRUNCATE removes every row".to_owned())),
        Statement::AlterTable(alter) => {
            let drops = alter
                .operations
                .iter()
                .any(|operation| variant_name(&format!("{operation:?}")).starts_with("Drop"));
            if drops {
                (
                    Category::Destructive,
                    Some("ALTER TABLE … DROP removes columns, keys or partitions".to_owned()),
                )
            } else {
                (Category::Ddl, None)
            }
        }
        Statement::Set(set) => set_category(set),
        Statement::Explain { analyze: false, .. } => (Category::Read, None),
        Statement::Call(_) | Statement::Execute { .. } => (Category::Write, Some("A procedure call may change data".to_owned())),
        _ => by_name(&variant_name(&format!("{statement:?}"))),
    }
}

/// Everything not matched structurally, by the AST variant name.
fn by_name(name: &str) -> (Category, Option<String>) {
    const READ: [&str; 12] = [
        "Use",
        "Declare",
        "Print",
        "Fetch",
        "Close",
        "Open",
        "ExplainTable",
        "Explain",
        "If",
        "While",
        "Case",
        "Return",
    ];
    const TRANSACTION: [&str; 6] = [
        "StartTransaction",
        "Commit",
        "Rollback",
        "Savepoint",
        "ReleaseSavepoint",
        "UnlockTables",
    ];
    const ADMIN: [&str; 20] = [
        "Grant",
        "Revoke",
        "Deny",
        "Kill",
        "CreateUser",
        "AlterUser",
        "CreateRole",
        "AlterRole",
        "SetRole",
        "CreateServer",
        "CreateSecret",
        "AlterSession",
        "Flush",
        "Install",
        "Load",
        "Pragma",
        "Reset",
        "AttachDatabase",
        "DetachDuckDBDatabase",
        "AttachDuckDBDatabase",
    ];
    const WRITE: [&str; 7] = ["LoadData", "Copy", "CopyIntoSnowflake", "Lock", "LockTables", "Unload", "Put"];
    if name.starts_with("Show") || READ.contains(&name) {
        (Category::Read, None)
    } else if TRANSACTION.contains(&name) {
        (Category::Transaction, None)
    } else if ADMIN.contains(&name) || name.starts_with("DropUser") || name.starts_with("DropRole") {
        (Category::Admin, None)
    } else if name.starts_with("Drop") {
        (Category::Destructive, Some("DROP removes the object and its data".to_owned()))
    } else if WRITE.contains(&name) {
        (Category::Write, None)
    } else if name.starts_with("Create")
        || name.starts_with("Alter")
        || matches!(name, "RenameTable" | "Comment" | "OptimizeTable" | "Analyze")
    {
        (Category::Ddl, None)
    } else if matches!(
        name,
        "Raise" | "RaisError" | "Throw" | "WaitFor" | "Assert" | "Prepare" | "Deallocate"
    ) {
        (Category::Read, None)
    } else {
        (
            Category::Unknown,
            Some(format!("{name} statements are not classified, so they are treated like a write")),
        )
    }
}

fn query_category(query: &Query) -> (Category, Option<String>) {
    match select_into(&query.body) {
        None => (Category::Read, None),
        Some(targets) if targets.iter().all(|target| target.starts_with('@')) => (Category::Read, None),
        Some(targets) if targets.iter().all(|target| target.starts_with('#')) => {
            (Category::Write, Some("SELECT … INTO creates a temporary table".to_owned()))
        }
        Some(_) => (Category::Ddl, Some("SELECT … INTO creates a table or writes a file".to_owned())),
    }
}

/// The targets of a `SELECT … INTO` anywhere in the query body.
fn select_into(body: &SetExpr) -> Option<Vec<String>> {
    match body {
        SetExpr::Select(select) => select
            .into
            .as_ref()
            .map(|into| into.targets.iter().map(|target| target.to_string()).collect()),
        SetExpr::Query(query) => select_into(&query.body),
        SetExpr::SetOperation { left, right, .. } => select_into(left).or_else(|| select_into(right)),
        _ => None,
    }
}

/// Session variables that would switch off the app's own backstops.
const PROTECTED_VARIABLES: [&str; 7] = [
    "transaction_read_only",
    "tx_read_only",
    "max_statement_time",
    "max_execution_time",
    "sql_log_bin",
    "read_only",
    "super_read_only",
];

fn set_category(set: &Set) -> (Category, Option<String>) {
    let variable_category = |name: &ObjectName, global: bool| -> (Category, Option<String>) {
        let lower = name.to_string().to_lowercase();
        let user_variable = lower.starts_with('@') && !lower.starts_with("@@");
        let bare = lower.trim_start_matches("@@");
        let global = global || bare.starts_with("global.") || bare.starts_with("persist");
        let bare = bare.trim_start_matches("session.").trim_start_matches("local.");
        if user_variable {
            (Category::Read, None)
        } else if global {
            (Category::Admin, Some("SET GLOBAL changes the whole server".to_owned()))
        } else if PROTECTED_VARIABLES.contains(&bare) {
            (Category::Admin, Some(format!("{bare} protects this session")))
        } else if bare == "autocommit" {
            (Category::Transaction, None)
        } else {
            (Category::Read, None)
        }
    };
    let is_global = |scope: &Option<sqlparser::ast::ContextModifier>| matches!(scope, Some(sqlparser::ast::ContextModifier::Global));
    let worst = |items: Vec<(Category, Option<String>)>| {
        items
            .into_iter()
            .max_by_key(|(category, _)| *category)
            .unwrap_or((Category::Read, None))
    };
    match set {
        Set::SingleAssignment { scope, variable, .. } => variable_category(variable, is_global(scope)),
        Set::ParenthesizedAssignments { variables, .. } => {
            worst(variables.iter().map(|variable| variable_category(variable, false)).collect())
        }
        Set::MultipleAssignments { assignments } => worst(
            assignments
                .iter()
                .map(|assignment| variable_category(&assignment.name, is_global(&assignment.scope)))
                .collect(),
        ),
        Set::SetTransaction { modes, .. } => {
            if modes
                .iter()
                .any(|mode| matches!(mode, TransactionMode::AccessMode(TransactionAccessMode::ReadWrite)))
            {
                (Category::Admin, Some("READ WRITE lifts the read-only session".to_owned()))
            } else {
                (Category::Transaction, None)
            }
        }
        Set::SetRole { .. } | Set::SetSessionAuthorization(_) => (Category::Admin, None),
        _ => (Category::Read, None),
    }
}

/// `Drop`, `AlterTable`, `ShowTables`… from the `Debug` form of an AST node.
fn variant_name(debug: &str) -> String {
    debug
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// A short, upper-case statement kind for the UI: `DELETE`, `DROP TABLE`, `ALTER TABLE`…
fn kind_of(statement: &Statement) -> String {
    if let Statement::Insert(insert) = statement {
        return if insert.replace_into { "REPLACE" } else { "INSERT" }.to_owned();
    }
    let text = statement.to_string();
    let words: Vec<&str> = text.split_whitespace().take(3).collect();
    let two_words = matches!(
        words.first().map(|word| word.to_uppercase()).as_deref(),
        Some("CREATE" | "DROP" | "ALTER" | "SHOW" | "START" | "SET" | "TRUNCATE")
    );
    let count = if two_words { 2 } else { 1 };
    words
        .iter()
        .take(count)
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn tables_of(statement: &Statement) -> Vec<String> {
    let mut tables: Vec<String> = Vec::new();
    let mut push = |name: &ObjectName| {
        let text = name.to_string();
        if !tables.contains(&text) {
            tables.push(text);
        }
    };
    if let Statement::Drop { names, .. } = statement {
        names.iter().for_each(&mut push);
    }
    let _ = visit_relations(statement, |relation| {
        push(relation);
        ControlFlow::<()>::Continue(())
    });
    tables
}

pub fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        flat
    } else {
        let cut: String = flat.chars().take(EXCERPT_CHARS).collect();
        format!("{cut}…")
    }
}
