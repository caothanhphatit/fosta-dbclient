//! SQL statement classification with `sqlparser`, shared by the MariaDB/MySQL and SQL Server engines and the statement
//! gateway (`gateway::classify` re-exports it). SQL is parsed in the engine's dialect (MySQL for MariaDB/MySQL, MsSql
//! for SQL Server, Generic as a fallback) and every statement, including statements nested in CTEs, `EXPLAIN ANALYZE`
//! and control flow, gets a [`Category`].
//!
//! The classifier errs on the safe side: SQL it cannot parse is [`Category::Unknown`], which the rules treat like a
//! write, and MySQL `/*! … */` comments (which the server executes) are classified as code.

use std::ops::ControlFlow;

use sqlparser::{
    ast::{
        visit_relations, visit_statements, Expr, ObjectName, Query, Set, SetExpr, Statement, TableFactor, TransactionAccessMode,
        TransactionMode, Visit, Visitor,
    },
    dialect::{ClickHouseDialect, Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect},
    parser::Parser,
    tokenizer::{Token, Tokenizer},
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
///
/// With `NO_BACKSLASH_ESCAPES` in `sql_mode`, MariaDB/MySQL read `\` as an ordinary character, so
/// `SELECT 'a\'; DROP TABLE t; -- '` is one string by default but two statements on a server in that mode. Text with a
/// backslash is therefore also classified with every backslash taken literally (doubling each one gives that reading under
/// the default rules), and the stricter result wins.
pub fn classify_sql(engine: SqlDialectId, text: &str) -> Classification {
    let classification = classify_text(engine, text);
    if matches!(engine, SqlDialectId::MariaDb | SqlDialectId::MySql) && text.contains('\\') {
        let literal = classify_text(engine, &text.replace('\\', "\\\\"));
        let stricter = literal.worst() > classification.worst()
            || (literal.worst() == classification.worst() && literal.statements.len() > classification.statements.len());
        if stricter {
            return literal;
        }
    }
    classification
}

fn classify_text(engine: SqlDialectId, text: &str) -> Classification {
    if text.trim().is_empty() {
        return Classification::default();
    }
    match parse(engine, text) {
        Ok(statements) => Classification {
            statements: statements.iter().map(|statement| classify_statement(engine, statement)).collect(),
        },
        Err(error) => {
            let (category, why) = unparsed_category(engine, text);
            let reason = match why {
                None => format!("The statement could not be parsed, so it is treated like a write ({error})"),
                Some(why) => format!(
                    "The statement could not be parsed; {why}, so it is treated as {} ({error})",
                    category.label()
                ),
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

/// SQL the parser does not understand is at least [`Category::Unknown`]; its keywords raise it further (never lower):
/// administrative or destructive leading keywords, side-effect function calls, dynamic SQL, and DROP, TRUNCATE, RESTORE,
/// DBCC or SHUTDOWN anywhere outside strings and comments. Returns the category and why it is more than Unknown.
fn unparsed_category(engine: SqlDialectId, text: &str) -> (Category, Option<String>) {
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
    let mut why = None;
    let mut raise = |to: Category, reason: String| {
        if to > category {
            category = to;
            why = Some(reason);
        }
    };
    for chunk in text.split(';') {
        let words: Vec<String> = chunk.split_whitespace().take(2).map(str::to_uppercase).collect();
        let lead = words.join(" ");
        if ADMIN.iter().any(|keyword| lead.starts_with(keyword)) {
            raise(Category::Admin, format!("it starts with {lead}"));
        } else if DESTRUCTIVE.iter().any(|keyword| words.first().is_some_and(|word| word == keyword)) {
            raise(Category::Destructive, format!("it starts with {}", words[0]));
        }
    }
    let words = code_words(engine, text);
    if let Some(function) = side_effect_in_words(&words, false) {
        raise(Category::Admin, side_effect_reason(&function));
    }
    if words
        .iter()
        .any(|(word, next)| word.eq_ignore_ascii_case("READ") && next.eq_ignore_ascii_case("WRITE"))
    {
        raise(Category::Admin, "READ WRITE lifts the read-only session".to_owned());
    }
    for (index, (word, next)) in words.iter().enumerate() {
        let upper = word.to_uppercase();
        // `EXEC @proc_name` runs whatever procedure the variable names; `EXEC @rc = proc` only keeps its return code.
        let return_code = words
            .get(index + 1)
            .is_some_and(|(variable, after)| variable == next && after == "=");
        let dynamic = upper == "SP_EXECUTESQL"
            || (matches!(upper.as_str(), "EXEC" | "EXECUTE") && (next == "(" || (next.starts_with('@') && !return_code)))
            || (upper == "EXECUTE" && next.eq_ignore_ascii_case("IMMEDIATE"))
            || (upper == "PREPARE" && index == 0);
        if dynamic {
            raise(Category::Destructive, DYNAMIC_SQL.to_lowercase());
        } else if matches!(upper.as_str(), "DROP" | "TRUNCATE" | "RESTORE" | "DBCC" | "SHUTDOWN") {
            raise(Category::Destructive, format!("it contains {upper}"));
        }
    }
    (category, why)
}

/// The unquoted words of `text` outside strings and comments, each with the token after it (`(`, `.`, `@x`, a word, or
/// empty), for the rules on unparsed SQL. Falls back to splitting on non-word characters when the text does not tokenize.
fn code_words(engine: SqlDialectId, text: &str) -> Vec<(String, String)> {
    let text = normalise_executable_comments(engine, text);
    let dialect = dialect_for(engine);
    let Ok(tokens) = Tokenizer::new(dialect.as_ref(), &text).tokenize() else {
        let mut words = Vec::new();
        let mut rest = text.as_str();
        while let Some(start) = rest.find(|c: char| c.is_alphanumeric() || c == '_' || c == '@') {
            rest = &rest[start..];
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '@' || c == '$'))
                .unwrap_or(rest.len());
            let next = rest[end..].trim_start().chars().next().map(String::from).unwrap_or_default();
            words.push((rest[..end].to_owned(), next));
            rest = &rest[end..];
        }
        return words;
    };
    let significant: Vec<&Token> = tokens.iter().filter(|token| !matches!(token, Token::Whitespace(_))).collect();
    significant
        .iter()
        .enumerate()
        .filter_map(|(index, token)| match token {
            Token::Word(word) if word.quote_style.is_none() => {
                let next = significant.get(index + 1).map(|next| match next {
                    Token::Word(word) => word.value.clone(),
                    other => other.to_string(),
                });
                Some((word.value.clone(), next.unwrap_or_default()))
            }
            _ => None,
        })
        .collect()
}

fn classify_statement(engine: SqlDialectId, statement: &Statement) -> StatementInfo {
    let (mut category, mut reason) = own_category(engine, statement);
    // Writes hidden inside the statement: CTEs (`WITH … DELETE`), `EXPLAIN ANALYZE`, IF/WHILE blocks.
    let explain_only = matches!(statement, Statement::Explain { .. }) && !explain_runs(statement);
    if !explain_only {
        let _ = visit_statements(statement, |inner| {
            if !std::ptr::eq(inner, statement) {
                let (inner_category, inner_reason) = own_category(engine, inner);
                if inner_category > category {
                    category = inner_category;
                    reason = inner_reason.or_else(|| Some(format!("contains {}", kind_of(inner))));
                }
            }
            ControlFlow::<()>::Continue(())
        });
        if category < Category::Admin && !defines_only(statement) {
            if let Some(function) = side_effect_call(statement) {
                category = Category::Admin;
                reason = Some(side_effect_reason(&function));
            }
        }
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
fn own_category(engine: SqlDialectId, statement: &Statement) -> (Category, Option<String>) {
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
        Statement::Explain { .. } => (Category::Read, None),
        Statement::StartTransaction { modes, .. } if read_write(modes) => {
            (Category::Admin, Some("READ WRITE lifts the read-only session".to_owned()))
        }
        Statement::Execute { name, .. } => execute_category(engine, name.as_ref()),
        Statement::Prepare { .. } => (Category::Destructive, Some(DYNAMIC_SQL.to_owned())),
        Statement::Call(_) => (Category::Write, Some("A procedure call may change data".to_owned())),
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
    } else if matches!(name, "Raise" | "RaisError" | "Throw" | "WaitFor" | "Assert" | "Deallocate") {
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

/// Session variables that would switch off the app's own backstops or change how later statements are read: the
/// read-only and timeout backstops, binary logging, `sql_mode` (`NO_BACKSLASH_ESCAPES` moves where strings end) and the
/// session's identity. Setting one is administrative; the PostgreSQL/SQLite/ClickHouse/Oracle classifier uses it too.
pub const PROTECTED_VARIABLES: [&str; 12] = [
    "transaction_read_only",
    "tx_read_only",
    "default_transaction_read_only",
    "max_statement_time",
    "max_execution_time",
    "sql_log_bin",
    "read_only",
    "super_read_only",
    "sql_mode",
    "session_authorization",
    "role",
    "readonly",
];

/// Whether `name` (any case, quoted or not, `@@session.`/`session.` prefixes allowed) is a [`PROTECTED_VARIABLES`] entry.
pub fn is_protected_variable(name: &str) -> bool {
    let lower = name.to_lowercase();
    let bare: String = lower.chars().filter(|c| !matches!(c, '`' | '"' | '[' | ']' | '\'')).collect();
    let bare = bare.trim_start_matches("@@");
    let bare = bare
        .trim_start_matches("session.")
        .trim_start_matches("local.")
        .trim_start_matches("pg_catalog.");
    PROTECTED_VARIABLES.contains(&bare)
}

const DYNAMIC_SQL: &str = "Dynamic SQL may run any statement, including DROP or DELETE";

fn read_write(modes: &[TransactionMode]) -> bool {
    modes
        .iter()
        .any(|mode| matches!(mode, TransactionMode::AccessMode(TransactionAccessMode::ReadWrite)))
}

/// `EXECUTE stmt` runs a prepared statement (MariaDB/MySQL, PostgreSQL), and `EXEC ('…')`, `EXEC (@sql)`, `EXEC @name` and
/// `sp_executesql` run dynamic SQL (SQL Server): any of them may drop or delete. `EXEC proc` is a procedure call, and
/// `EXEC xp_cmdshell …` reaches the server's operating system.
fn execute_category(engine: SqlDialectId, name: Option<&ObjectName>) -> (Category, Option<String>) {
    let Some(name) = name else {
        return (Category::Destructive, Some(DYNAMIC_SQL.to_owned()));
    };
    let parts = name_parts(name);
    if let Some(function) = side_effect_function(&parts, false) {
        return (Category::Admin, Some(side_effect_reason(&function)));
    }
    let last = parts.last().map(String::as_str).unwrap_or_default();
    if engine != SqlDialectId::MsSql || last == "sp_executesql" || last.starts_with('@') {
        (Category::Destructive, Some(DYNAMIC_SQL.to_owned()))
    } else {
        (Category::Write, Some("A procedure call may change data".to_owned()))
    }
}

/// The parts of a name, unquoted and lower-case: `pg_catalog`, `pg_terminate_backend`.
fn name_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .map(|part| match part.as_ident() {
            Some(ident) => ident.value.to_lowercase(),
            None => part.to_string().to_lowercase(),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------------------
// Side-effect functions

/// Functions that change server state, sleep, take locks, touch server files or reach other servers: a SELECT that calls
/// one is administrative, and a grid filter may not call one. One list for every SQL family, matched on the last part of
/// the name (so `pg_catalog.pg_terminate_backend` too), case-insensitively.
const SIDE_EFFECT_FUNCTIONS: [&str; 27] = [
    // PostgreSQL
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "set_config",
    "lo_import",
    "lo_export",
    "lo_unlink",
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
    "nextval",
    "setval",
    // MariaDB / MySQL
    "load_file",
    "sleep",
    "benchmark",
    "get_lock",
    "release_all_locks",
    "master_pos_wait",
    "source_pos_wait",
    // SQL Server
    "openquery",
    "openrowset",
    "opendatasource",
    // Oracle
    "httpuritype",
    "dbms_pipe",
    // SQLite
    "load_extension",
];

/// Name prefixes of the same kind: `dblink_exec`, `pg_sleep_for`, `pg_advisory_lock`, `release_lock`, `xp_cmdshell`.
const SIDE_EFFECT_PREFIXES: [&str; 6] = ["dblink", "pg_sleep", "pg_advisory_", "pg_try_advisory_", "release_lock", "xp_"];

/// Oracle packages that reach the network, files, jobs or the scheduler: any `UTL_*` or `DBMS_*` package…
const SIDE_EFFECT_PACKAGES: [&str; 2] = ["utl_", "dbms_"];
/// …except these, which only format output, read metadata or make random numbers.
const HARMLESS_PACKAGES: [&str; 3] = ["dbms_output", "dbms_metadata", "dbms_random"];

/// ClickHouse table functions that read files or reach other servers; only in `FROM` (they are ordinary words elsewhere).
const SIDE_EFFECT_TABLE_FUNCTIONS: [&str; 16] = [
    "url",
    "urlcluster",
    "remote",
    "remotesecure",
    "s3",
    "s3cluster",
    "file",
    "hdfs",
    "hdfscluster",
    "mysql",
    "postgresql",
    "jdbc",
    "odbc",
    "mongodb",
    "sqlite",
    "executable",
];

/// The side-effect function a call to `parts` (unquoted, lower-case name parts) names, if any; `table` when it is called
/// as a table function in `FROM`.
pub fn side_effect_function(parts: &[String], table: bool) -> Option<String> {
    let last = parts.last()?.as_str();
    let listed = SIDE_EFFECT_FUNCTIONS.contains(&last)
        || SIDE_EFFECT_PREFIXES.iter().any(|prefix| last.starts_with(prefix))
        || (table && SIDE_EFFECT_TABLE_FUNCTIONS.contains(&last));
    if listed {
        return Some(last.to_owned());
    }
    let package = parts.iter().find(|part| is_side_effect_package(part))?;
    Some(if package == last {
        last.to_owned()
    } else {
        format!("{package}.{last}")
    })
}

fn is_side_effect_package(name: &str) -> bool {
    SIDE_EFFECT_PACKAGES.iter().any(|prefix| name.starts_with(prefix)) && !HARMLESS_PACKAGES.contains(&name)
}

/// Statements that only define code or defaults (`CREATE FUNCTION`, `CREATE VIEW`, `DEFAULT nextval(…)`) do not call
/// what they mention; `CREATE TABLE … AS SELECT` does.
pub fn defines_only(statement: &Statement) -> bool {
    if let Statement::CreateTable(create) = statement {
        return create.query.is_none();
    }
    let name = variant_name(&format!("{statement:?}"));
    name.starts_with("Create") || name.starts_with("Alter") || name == "Comment"
}

/// "pg_terminate_backend() changes server state".
pub fn side_effect_reason(function: &str) -> String {
    format!("{function}() changes server state")
}

/// The first side-effect function `node` calls, as a function or as a table function, anywhere inside it (subqueries
/// included).
pub fn side_effect_call<V: Visit>(node: &V) -> Option<String> {
    struct Finder;
    impl Visitor for Finder {
        type Break = String;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            match expr {
                Expr::Function(function) => match side_effect_function(&name_parts(&function.name), false) {
                    Some(found) => ControlFlow::Break(found),
                    None => ControlFlow::Continue(()),
                },
                _ => ControlFlow::Continue(()),
            }
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<String> {
            let name = match factor {
                TableFactor::Table { name, args: Some(_), .. } | TableFactor::Function { name, .. } => Some(name),
                TableFactor::TableFunction {
                    expr: Expr::Function(function),
                    ..
                } => Some(&function.name),
                _ => None,
            };
            match name.and_then(|name| side_effect_function(&name_parts(name), true)) {
                Some(found) => ControlFlow::Break(found),
                None => ControlFlow::Continue(()),
            }
        }
    }
    match node.visit(&mut Finder) {
        ControlFlow::Break(found) => Some(found),
        ControlFlow::Continue(()) => None,
    }
}

/// [`side_effect_call`] for text the parser did not understand: a listed name followed by `(`, or a `UTL_*`/`DBMS_*`
/// package followed by `.`. `words` are the unquoted words outside strings and comments, each with the token after it;
/// `table_functions` also matches the ClickHouse table functions.
pub fn side_effect_in_words(words: &[(String, String)], table_functions: bool) -> Option<String> {
    words.iter().find_map(|(word, next)| {
        let word = word.to_lowercase();
        match next.as_str() {
            "(" => side_effect_function(&[word], table_functions),
            "." if is_side_effect_package(&word) => Some(word),
            _ => None,
        }
    })
}

/// Whether an `EXPLAIN` runs its statement: `EXPLAIN ANALYZE`, or PostgreSQL's option list with `ANALYZE` or `ANALYSE`
/// (unless set to false/off/0).
pub fn explain_runs(statement: &Statement) -> bool {
    let Statement::Explain { analyze, options, .. } = statement else {
        return false;
    };
    *analyze
        || options.iter().flatten().any(|option| {
            let name = option.name.value.to_lowercase();
            let off = option
                .arg
                .as_ref()
                .is_some_and(|arg| matches!(arg.to_string().to_lowercase().as_str(), "false" | "off" | "0"));
            (name == "analyze" || name == "analyse") && !off
        })
}

fn set_category(set: &Set) -> (Category, Option<String>) {
    let variable_category = |name: &ObjectName, global: bool| -> (Category, Option<String>) {
        // Quotes and backticks are not part of the name: `transaction_read_only` is transaction_read_only.
        let lower = name_parts(name).join(".");
        let user_variable = lower.starts_with('@') && !lower.starts_with("@@");
        let bare = lower.trim_start_matches("@@");
        let global = global || bare.starts_with("global.") || bare.starts_with("persist");
        let bare = bare.trim_start_matches("session.").trim_start_matches("local.");
        if user_variable {
            (Category::Read, None)
        } else if global {
            (Category::Admin, Some("SET GLOBAL changes the whole server".to_owned()))
        } else if is_protected_variable(bare) {
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
            if read_write(modes) {
                (Category::Admin, Some("READ WRITE lifts the read-only session".to_owned()))
            } else {
                (Category::Transaction, None)
            }
        }
        Set::SetRole { .. } | Set::SetSessionAuthorization(_) => (Category::Admin, Some("changes the identity of the session".to_owned())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use Category::*;

    fn worst(engine: SqlDialectId, sql: &str) -> Option<Category> {
        classify_sql(engine, sql).worst()
    }

    fn check(engine: SqlDialectId, cases: &[(&str, Category)]) {
        let wrong: Vec<String> = cases
            .iter()
            .filter_map(|(sql, expected)| {
                let got = worst(engine, sql);
                (got != Some(*expected)).then(|| format!("{sql}: expected {expected:?}, got {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{engine:?}\n{}", wrong.join("\n"));
    }

    #[test]
    fn read_write_transactions_and_protected_settings_are_admin() {
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql] {
            check(
                engine,
                &[
                    ("START TRANSACTION", Transaction),
                    ("BEGIN", Transaction),
                    ("START TRANSACTION READ ONLY", Transaction),
                    ("START TRANSACTION READ WRITE", Admin),
                    ("START TRANSACTION WITH CONSISTENT SNAPSHOT, READ WRITE", Admin),
                    ("SET TRANSACTION READ WRITE", Admin),
                    ("SET SESSION TRANSACTION READ WRITE", Admin),
                    ("SET transaction_read_only = 0", Admin),
                    ("SET `transaction_read_only` = 0", Admin),
                    ("SET SESSION `tx_read_only` = OFF", Admin),
                    ("SET @@session.`transaction_read_only` = 0", Admin),
                    ("SET @@SESSION.TX_READ_ONLY = 0", Admin),
                    ("SET sql_mode = 'NO_BACKSLASH_ESCAPES'", Admin),
                    ("SET SESSION sql_mode = ''", Admin),
                    ("SET @@sql_mode = 'ANSI_QUOTES'", Admin),
                    ("SET `sql_mode` = ''", Admin),
                    ("SET ROLE ALL", Admin),
                    ("SET @x = 1", Read),
                    ("SET NAMES utf8mb4", Read),
                ],
            );
        }
    }

    #[test]
    fn backslash_smuggling_is_classified_both_ways() {
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql] {
            let smuggled = classify_sql(engine, "SELECT 'a\\'; DROP TABLE t; -- '");
            assert_eq!(smuggled.worst(), Some(Destructive), "{smuggled:?}");
            assert!(smuggled.statements.len() > 1, "{smuggled:?}");
            check(
                engine,
                &[
                    ("SELECT 'a\\'; DELETE FROM t WHERE id = 1; -- '", Write),
                    ("SELECT 'C:\\\\temp' AS path", Read),
                    ("SELECT * FROM t WHERE name LIKE 'a\\_b'", Read),
                    ("SELECT 'it''s'", Read),
                ],
            );
        }
        // SQL Server never escapes with a backslash.
        assert_eq!(worst(SqlDialectId::MsSql, "SELECT 'a\\'"), Some(Read));
    }

    #[test]
    fn side_effect_functions_are_admin() {
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql] {
            check(
                engine,
                &[
                    ("SELECT SLEEP(10)", Admin),
                    ("SELECT LOAD_FILE('/etc/passwd')", Admin),
                    ("SELECT BENCHMARK(1000000, MD5('x'))", Admin),
                    ("SELECT GET_LOCK('x', 10)", Admin),
                    ("SELECT RELEASE_LOCK('x')", Admin),
                    ("SELECT release_all_locks()", Admin),
                    ("SELECT MASTER_POS_WAIT('log', 4)", Admin),
                    ("SELECT * FROM t WHERE id = (SELECT sleep(1))", Admin),
                    ("SET @x = SLEEP(5)", Admin),
                    ("SELECT `sleep`(1)", Admin),
                    ("SELECT COUNT(*), NOW() FROM t", Read),
                    ("CREATE TABLE t (a INT DEFAULT (RAND()))", Ddl),
                ],
            );
        }
        let reason = classify_sql(SqlDialectId::MySql, "SELECT SLEEP(1)").statements[0].reason.clone();
        assert_eq!(reason.as_deref(), Some("sleep() changes server state"));
        check(
            SqlDialectId::MsSql,
            &[
                ("SELECT * FROM OPENQUERY(other, 'SELECT 1')", Admin),
                ("SELECT * FROM OPENROWSET('SQLNCLI', 'Server=x;', 'SELECT 1') AS r", Admin),
                ("SELECT * FROM OPENDATASOURCE('SQLNCLI', 'Data Source=x').db.dbo.t", Admin),
                ("EXEC xp_cmdshell 'dir'", Admin),
                ("EXEC master..xp_cmdshell 'dir'", Admin),
                ("EXEC master.dbo.xp_dirtree 'C:\\'", Admin),
                ("SELECT TOP 10 * FROM dbo.Orders", Read),
            ],
        );
    }

    #[test]
    fn dynamic_sql_is_destructive() {
        check(
            SqlDialectId::MsSql,
            &[
                ("EXEC sp_who", Write),
                ("EXEC dbo.usp_archive @days = 30", Write),
                // Not parsed by sqlparser, so Unknown (treated like a write), but not dynamic SQL.
                ("EXEC @rc = dbo.usp_archive @days = 30", Unknown),
                ("DECLARE @rc INT; EXEC @rc = dbo.usp_archive; SELECT @rc", Unknown),
                ("EXEC ('DROP TABLE t')", Destructive),
                ("EXEC (@sql)", Destructive),
                ("EXECUTE ('DELETE FROM t')", Destructive),
                ("EXEC @proc", Destructive),
                ("EXEC sp_executesql N'DROP TABLE t'", Destructive),
                ("EXEC sys.sp_executesql @stmt", Destructive),
                ("DECLARE @s NVARCHAR(100) = N'DROP TABLE t'; EXEC (@s)", Destructive),
            ],
        );
        for engine in [SqlDialectId::MariaDb, SqlDialectId::MySql] {
            check(
                engine,
                &[
                    ("PREPARE s FROM 'DROP TABLE t'", Destructive),
                    ("PREPARE s FROM @sql", Destructive),
                    ("EXECUTE s", Destructive),
                    ("EXECUTE s USING @a", Destructive),
                    ("CALL p()", Write),
                ],
            );
        }
    }

    #[test]
    fn unparsed_sql_with_drops_is_destructive() {
        check(
            SqlDialectId::MsSql,
            &[
                ("FROBNICATE the database", Unknown),
                ("FROBNICATE 'DROP TABLE t'", Unknown),
                ("/* c */ FROBNICATE; DROP TABLE t", Destructive),
                ("FROBNICATE -- DROP TABLE t", Unknown),
                ("RESTORE DATABASE d FROM DISK = 'x.bak' WITH REPLACE", Destructive),
                ("DBCC CHECKDB (d, REPAIR_ALLOW_DATA_LOSS)", Destructive),
                ("FROBNICATE SHUTDOWN", Destructive),
                ("SHUTDOWN WITH NOWAIT", Admin),
                ("FROBNICATE OPENQUERY(x, 'y')", Admin),
            ],
        );
    }

    #[test]
    fn explain_options_with_analyze_run_the_statement() {
        // MariaDB/MySQL fall back to the generic parser for PostgreSQL's option list; the server rejects it anyway, but the
        // classification must not call it a read.
        let explain = worst(SqlDialectId::MySql, "EXPLAIN (ANALYZE) DELETE FROM t");
        assert!(explain.is_some_and(|c| c > Read), "{explain:?}");
        assert_eq!(
            worst(SqlDialectId::MySql, "EXPLAIN ANALYZE DELETE FROM t WHERE id = 1"),
            Some(Write)
        );
        assert_eq!(worst(SqlDialectId::MySql, "EXPLAIN SELECT SLEEP(1)"), Some(Read));
    }
}
