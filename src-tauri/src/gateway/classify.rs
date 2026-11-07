//! Statement classification. SQL is parsed with `sqlparser` in the engine's dialect
//! (MySQL for MariaDB/MySQL, MsSql for SQL Server, Generic as a fallback) and every
//! statement — including statements nested in CTEs, `EXPLAIN ANALYZE` and control flow —
//! gets a [`Category`]. MongoDB database commands are classified by their name.
//!
//! The classifier errs on the safe side: SQL it cannot parse is [`Category::Unknown`],
//! which the rules treat like a write, and MySQL `/*! … */` comments (which the server
//! executes) are classified as code.

use std::{collections::BTreeSet, ops::ControlFlow};

use serde::Serialize;
use sqlparser::{
    ast::{visit_relations, visit_statements, ObjectName, Query, Set, SetExpr, Statement, TransactionAccessMode, TransactionMode},
    dialect::{Dialect, GenericDialect, MsSqlDialect, MySqlDialect},
    parser::Parser,
};

use crate::drivers::{types::RowChange, Engine};

const EXCERPT_CHARS: usize = 240;

/// What a statement can do, from harmless to dangerous. `Ord` follows that severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// SELECT, SHOW, EXPLAIN, USE, session SET, MongoDB find/aggregate/count…
    Read,
    /// BEGIN / COMMIT / ROLLBACK / SAVEPOINT / autocommit: always allowed.
    Transaction,
    /// INSERT, UPDATE, DELETE (with WHERE), MERGE, REPLACE, procedure calls.
    Write,
    /// SQL that could not be parsed; treated like a write.
    Unknown,
    /// CREATE / ALTER / RENAME (without dropping anything).
    Ddl,
    /// DROP, TRUNCATE, UPDATE/DELETE without WHERE, ALTER … DROP.
    Destructive,
    /// Users, grants, KILL, server-wide settings.
    Admin,
}

impl Category {
    #[cfg(test)]
    pub const ALL: [Category; 7] = [
        Category::Read,
        Category::Transaction,
        Category::Write,
        Category::Unknown,
        Category::Ddl,
        Category::Destructive,
        Category::Admin,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Read => "Read",
            Category::Transaction => "Transaction",
            Category::Write => "Write",
            Category::Unknown => "Unparsed",
            Category::Ddl => "DDL",
            Category::Destructive => "Destructive",
            Category::Admin => "Administrative",
        }
    }
}

/// One classified statement, as shown in the confirmation dialog and written to the audit log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementInfo {
    pub category: Category,
    /// Short statement kind, e.g. `DELETE`, `DROP TABLE`, `find`.
    pub kind: String,
    /// Tables or collections it touches, as written.
    pub tables: Vec<String>,
    pub excerpt: String,
    /// Why it got this category when that is not obvious ("DELETE without WHERE").
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub statements: Vec<StatementInfo>,
}

impl Classification {
    pub fn single(info: StatementInfo) -> Self {
        Self { statements: vec![info] }
    }

    /// Distinct categories, most severe last.
    pub fn categories(&self) -> Vec<Category> {
        let set: BTreeSet<Category> = self.statements.iter().map(|statement| statement.category).collect();
        set.into_iter().collect()
    }

    pub fn tables(&self) -> Vec<String> {
        let mut tables: Vec<String> = Vec::new();
        for table in self.statements.iter().flat_map(|statement| &statement.tables) {
            if !tables.contains(table) {
                tables.push(table.clone());
            }
        }
        tables
    }
}

pub fn dialect_for(engine: Engine) -> Box<dyn Dialect> {
    match engine {
        Engine::MariaDb | Engine::MySql => Box::new(MySqlDialect {}),
        Engine::SqlServer => Box::new(MsSqlDialect {}),
        Engine::MongoDb => Box::new(GenericDialect {}),
    }
}

/// MariaDB's `/*M! … */` comments run like MySQL's `/*! … */`; the parser only knows the latter.
pub(super) fn normalise_executable_comments(engine: Engine, text: &str) -> String {
    match engine {
        Engine::MariaDb | Engine::MySql => text.replace("/*M!", "/*!"),
        _ => text.to_owned(),
    }
}

/// Parses `text` in the engine's dialect, falling back to the generic one.
pub(super) fn parse(engine: Engine, text: &str) -> Result<Vec<Statement>, String> {
    let text = normalise_executable_comments(engine, text);
    match Parser::parse_sql(dialect_for(engine).as_ref(), &text) {
        Ok(statements) => Ok(statements),
        Err(error) => Parser::parse_sql(&GenericDialect {}, &text).map_err(|_| error.to_string()),
    }
}

/// Classifies SQL typed in a query tab (or built by the app).
pub fn classify_sql(engine: Engine, text: &str) -> Classification {
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

pub(super) fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        flat
    } else {
        let cut: String = flat.chars().take(EXCERPT_CHARS).collect();
        format!("{cut}…")
    }
}

/// Grid saves and imports: the app builds `INSERT` / `UPDATE … WHERE key LIMIT 1` /
/// `DELETE … WHERE key LIMIT 1` from these, so they are writes to one table.
pub fn classify_changes(table: &str, changes: &[RowChange]) -> Classification {
    let count = |wanted: fn(&RowChange) -> bool| changes.iter().filter(|change| wanted(change)).count();
    let parts = [
        ("INSERT", count(|change| matches!(change, RowChange::Insert { .. }))),
        ("UPDATE", count(|change| matches!(change, RowChange::Update { .. }))),
        ("DELETE", count(|change| matches!(change, RowChange::Delete { .. }))),
    ];
    Classification {
        statements: parts
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(kind, count)| StatementInfo {
                category: Category::Write,
                kind: (*kind).to_owned(),
                tables: vec![table.to_owned()],
                excerpt: format!("{kind} {count} row(s) of {table}, each matched by its key"),
                reason: None,
            })
            .collect(),
    }
}

/// MongoDB database command names by category; anything else is [`Category::Unknown`].
const MONGO_READ: [&str; 31] = [
    "find",
    "aggregate",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "dbStats",
    "collStats",
    "dataSize",
    "explain",
    "getMore",
    "killCursors",
    "buildInfo",
    "hello",
    "isMaster",
    "ismaster",
    "ping",
    "serverStatus",
    "connectionStatus",
    "hostInfo",
    "validate",
    "getParameter",
    "currentOp",
    "listCommands",
    "replSetGetStatus",
    "getLog",
    "top",
    "whatsmyuri",
    "usersInfo",
    "rolesInfo",
];
const MONGO_WRITE: [&str; 5] = ["insert", "update", "delete", "findAndModify", "bulkWrite"];
const MONGO_DESTRUCTIVE: [&str; 5] = ["drop", "dropDatabase", "renameCollection", "dropIndexes", "convertToCapped"];
const MONGO_DDL: [&str; 5] = ["create", "createIndexes", "collMod", "dropSearchIndex", "createSearchIndexes"];
const MONGO_TRANSACTION: [&str; 3] = ["startSession", "commitTransaction", "abortTransaction"];

/// Classifies a MongoDB database command written as JSON (the query tab's input).
pub fn classify_mongo(text: &str) -> Classification {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return Classification::single(StatementInfo {
                category: Category::Unknown,
                kind: "unparsed".to_owned(),
                tables: Vec::new(),
                excerpt: excerpt(text),
                reason: Some(format!("Not a JSON command ({error})")),
            })
        }
    };
    let Some((name, target)) = value.as_object().and_then(|object| object.iter().next()) else {
        return Classification::single(StatementInfo {
            category: Category::Unknown,
            kind: "unparsed".to_owned(),
            tables: Vec::new(),
            excerpt: excerpt(text),
            reason: Some("A MongoDB command is a JSON object whose first key is the command name".to_owned()),
        });
    };
    let tables = target.as_str().map(|collection| vec![collection.to_owned()]).unwrap_or_default();
    let (category, reason) = mongo_category(name, &value);
    Classification::single(StatementInfo {
        category,
        kind: name.clone(),
        tables,
        excerpt: excerpt(text),
        reason,
    })
}

fn mongo_category(name: &str, command: &serde_json::Value) -> (Category, Option<String>) {
    if name == "aggregate" {
        let writes = command
            .get("pipeline")
            .and_then(|pipeline| pipeline.as_array())
            .is_some_and(|stages| {
                stages
                    .iter()
                    .any(|stage| stage.get("$out").is_some() || stage.get("$merge").is_some())
            });
        return if writes {
            (Category::Write, Some("$out / $merge write the result into a collection".to_owned()))
        } else {
            (Category::Read, None)
        };
    }
    if name == "delete" && mongo_matches_everything(command, "deletes") {
        return (Category::Destructive, Some("An empty filter deletes every document".to_owned()));
    }
    if name == "update" && mongo_updates_everything(command) {
        return (
            Category::Destructive,
            Some("An empty filter with multi updates every document".to_owned()),
        );
    }
    if name == "explain" {
        // Explain never applies writes, whatever it explains.
        return (Category::Read, None);
    }
    if MONGO_READ.contains(&name) {
        (Category::Read, None)
    } else if MONGO_WRITE.contains(&name) {
        (Category::Write, None)
    } else if MONGO_DESTRUCTIVE.contains(&name) {
        (Category::Destructive, Some(format!("{name} removes data")))
    } else if MONGO_DDL.contains(&name) {
        (Category::Ddl, None)
    } else if MONGO_TRANSACTION.contains(&name) {
        (Category::Transaction, None)
    } else if name.contains("User") || name.contains("Role") || name.starts_with("replSet") || ADMIN_MONGO.contains(&name) {
        (Category::Admin, None)
    } else {
        (
            Category::Unknown,
            Some(format!("{name} is not a known command, so it is treated like a write")),
        )
    }
}

const ADMIN_MONGO: [&str; 8] = [
    "killOp",
    "shutdown",
    "setParameter",
    "fsync",
    "fsyncUnlock",
    "logRotate",
    "compact",
    "setFeatureCompatibilityVersion",
];

fn mongo_matches_everything(command: &serde_json::Value, list: &str) -> bool {
    command.get(list).and_then(|items| items.as_array()).is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("q").and_then(|q| q.as_object()).is_some_and(|q| q.is_empty()))
    })
}

fn mongo_updates_everything(command: &serde_json::Value) -> bool {
    command.get("updates").and_then(|items| items.as_array()).is_some_and(|items| {
        items.iter().any(|item| {
            let empty = item.get("q").and_then(|q| q.as_object()).is_some_and(|q| q.is_empty());
            let multi = item.get("multi").and_then(|multi| multi.as_bool()).unwrap_or(false);
            empty && multi
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn categories(engine: Engine, sql: &str) -> Vec<Category> {
        classify_sql(engine, sql)
            .statements
            .iter()
            .map(|statement| statement.category)
            .collect()
    }

    fn one(engine: Engine, sql: &str) -> StatementInfo {
        let classification = classify_sql(engine, sql);
        assert_eq!(classification.statements.len(), 1, "{sql}: {classification:?}");
        classification.statements[0].clone()
    }

    #[test]
    fn reads_on_every_dialect() {
        for engine in [Engine::MariaDb, Engine::MySql, Engine::SqlServer] {
            assert_eq!(categories(engine, "SELECT * FROM orders WHERE id = 1"), [Category::Read]);
            assert_eq!(
                categories(engine, "SELECT o.id FROM orders o JOIN customers c ON c.id = o.customer_id"),
                [Category::Read]
            );
        }
        assert_eq!(categories(Engine::MariaDb, "SHOW TABLES"), [Category::Read]);
        assert_eq!(categories(Engine::MariaDb, "EXPLAIN SELECT 1"), [Category::Read]);
        assert_eq!(categories(Engine::MariaDb, "USE shop"), [Category::Read]);
        assert_eq!(
            categories(Engine::SqlServer, "SELECT TOP 5 * FROM sales.Invoices"),
            [Category::Read]
        );
        assert_eq!(categories(Engine::SqlServer, "SET NOCOUNT ON"), [Category::Read]);
    }

    #[test]
    fn writes_ddl_destructive_and_admin() {
        let maria = Engine::MariaDb;
        assert_eq!(one(maria, "INSERT INTO t (a) VALUES (1)").category, Category::Write);
        let replace = one(maria, "REPLACE INTO t (a) VALUES (1)");
        assert_eq!((replace.category, replace.kind.as_str()), (Category::Write, "REPLACE"));
        assert_eq!(one(maria, "UPDATE t SET a = 1 WHERE id = 2").category, Category::Write);
        assert_eq!(one(maria, "DELETE FROM t WHERE id = 2").category, Category::Write);
        let delete_all = one(maria, "DELETE FROM orders");
        assert_eq!(delete_all.category, Category::Destructive);
        assert_eq!(delete_all.tables, ["orders"]);
        assert!(delete_all.reason.unwrap().contains("without WHERE"));
        assert_eq!(one(maria, "UPDATE t SET a = 1").category, Category::Destructive);
        let drop = one(maria, "DROP TABLE shop.orders");
        assert_eq!((drop.category, drop.kind.as_str()), (Category::Destructive, "DROP TABLE"));
        assert_eq!(drop.tables, ["shop.orders"]);
        assert_eq!(one(maria, "TRUNCATE TABLE orders").category, Category::Destructive);
        assert_eq!(one(maria, "ALTER TABLE t DROP COLUMN a").category, Category::Destructive);
        assert_eq!(one(maria, "ALTER TABLE t ADD COLUMN b INT").category, Category::Ddl);
        assert_eq!(one(maria, "CREATE TABLE t (id INT PRIMARY KEY)").category, Category::Ddl);
        assert_eq!(one(maria, "CREATE INDEX i ON t (a)").category, Category::Ddl);
        assert_eq!(one(maria, "GRANT SELECT ON shop.* TO 'app'@'%'").category, Category::Admin);
        assert_eq!(one(maria, "KILL 42").category, Category::Admin);
        assert_eq!(one(maria, "SET GLOBAL max_connections = 10").category, Category::Admin);
        assert_eq!(one(maria, "CREATE USER 'x'@'%' IDENTIFIED BY 'y'").category, Category::Admin);
        assert_eq!(one(Engine::SqlServer, "DROP TABLE dbo.Orders").category, Category::Destructive);
        assert_eq!(
            one(
                Engine::SqlServer,
                "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET t.a = s.a;"
            )
            .category,
            Category::Write
        );
        assert_eq!(one(Engine::SqlServer, "EXEC sp_who").category, Category::Write);
    }

    #[test]
    fn session_settings_and_transactions() {
        let maria = Engine::MariaDb;
        assert_eq!(one(maria, "SET @x = 1").category, Category::Read);
        assert_eq!(one(maria, "SET NAMES utf8mb4").category, Category::Read);
        assert_eq!(one(maria, "SET autocommit = 0").category, Category::Transaction);
        assert_eq!(one(maria, "SET SESSION max_statement_time = 0").category, Category::Admin);
        assert_eq!(one(maria, "SET SESSION TRANSACTION READ WRITE").category, Category::Admin);
        assert_eq!(
            one(maria, "SET TRANSACTION ISOLATION LEVEL READ COMMITTED").category,
            Category::Transaction
        );
        for sql in ["START TRANSACTION", "BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT a"] {
            assert_eq!(one(maria, sql).category, Category::Transaction, "{sql}");
        }
        assert_eq!(categories(Engine::SqlServer, "BEGIN TRANSACTION"), [Category::Transaction]);
        assert_eq!(categories(Engine::SqlServer, "COMMIT TRANSACTION"), [Category::Transaction]);
    }

    #[test]
    fn comments_multi_statements_and_executable_comments() {
        let maria = Engine::MariaDb;
        assert_eq!(categories(maria, "-- just a note\nSELECT 1 /* inline */"), [Category::Read]);
        assert_eq!(categories(maria, "# hash comment\nSELECT 1"), [Category::Read]);
        assert_eq!(
            categories(maria, "SELECT 1; DELETE FROM t WHERE id = 1; DROP TABLE t"),
            [Category::Read, Category::Write, Category::Destructive]
        );
        // The server runs the inside of /*! … */ (and MariaDB's /*M! … */); so does the classifier.
        assert!(categories(maria, "SELECT 1 /*! ; DROP TABLE t */").contains(&Category::Destructive));
        assert!(categories(maria, "SELECT 1 /*M!100100 ; DROP TABLE t */").contains(&Category::Destructive));
        assert!(categories(maria, "/*!40101 DELETE FROM t */").contains(&Category::Destructive));
        // A plain comment that mentions DROP is still a comment.
        assert_eq!(categories(maria, "SELECT 1 /* DROP TABLE t */"), [Category::Read]);
    }

    #[test]
    fn ctes_and_select_into() {
        let mssql = Engine::SqlServer;
        let cte_delete = one(
            mssql,
            "WITH old AS (SELECT id FROM t WHERE y < 2000) DELETE FROM t WHERE id IN (SELECT id FROM old)",
        );
        assert!(cte_delete.category >= Category::Write, "{cte_delete:?}");
        assert_eq!(
            one(Engine::MariaDb, "WITH x AS (SELECT 1 AS a) SELECT * FROM x").category,
            Category::Read
        );
        assert_eq!(one(mssql, "SELECT * INTO archive FROM orders").category, Category::Ddl);
        assert_eq!(one(mssql, "SELECT * INTO #tmp FROM orders").category, Category::Write);
        assert_eq!(one(Engine::MariaDb, "SELECT COUNT(*) INTO @n FROM orders").category, Category::Read);
        let outfile = one(Engine::MariaDb, "SELECT * FROM orders INTO OUTFILE '/tmp/x.csv'");
        assert!(outfile.category >= Category::Write, "{outfile:?}");
        // EXPLAIN only plans; EXPLAIN ANALYZE runs the statement.
        assert_eq!(one(Engine::MariaDb, "EXPLAIN DELETE FROM t").category, Category::Read);
        assert_eq!(
            one(Engine::MariaDb, "EXPLAIN ANALYZE DELETE FROM t").category,
            Category::Destructive
        );
    }

    #[test]
    fn unparseable_sql_is_unknown() {
        let unknown = one(Engine::MariaDb, "FROBNICATE the database");
        assert_eq!(unknown.category, Category::Unknown);
        assert!(unknown.reason.unwrap().contains("treated like a write"));
        assert!(classify_sql(Engine::MariaDb, "  ").statements.is_empty());
    }

    #[test]
    fn mongo_commands_by_name() {
        let category = |text: &str| classify_mongo(text).statements[0].category;
        assert_eq!(category(r#"{ "find": "products", "filter": { "stock": 0 } }"#), Category::Read);
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$match": {} } ], "cursor": {} }"#),
            Category::Read
        );
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$out": "copy" } ], "cursor": {} }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$merge": { "into": "x" } } ], "cursor": {} }"#),
            Category::Write
        );
        assert_eq!(category(r#"{ "count": "c" }"#), Category::Read);
        assert_eq!(category(r#"{ "listCollections": 1 }"#), Category::Read);
        assert_eq!(category(r#"{ "insert": "c", "documents": [ { "a": 1 } ] }"#), Category::Write);
        assert_eq!(
            category(r#"{ "update": "c", "updates": [ { "q": { "a": 1 }, "u": { "$set": { "b": 2 } } } ] }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "update": "c", "updates": [ { "q": {}, "u": { "$set": { "b": 2 } }, "multi": true } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": { "a": 1 }, "limit": 1 } ] }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": {}, "limit": 0 } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "findAndModify": "c", "query": {}, "remove": true }"#),
            Category::Write
        );
        assert_eq!(category(r#"{ "drop": "c" }"#), Category::Destructive);
        assert_eq!(category(r#"{ "dropDatabase": 1 }"#), Category::Destructive);
        assert_eq!(category(r#"{ "renameCollection": "a.b", "to": "a.c" }"#), Category::Destructive);
        assert_eq!(category(r#"{ "createIndexes": "c", "indexes": [] }"#), Category::Ddl);
        assert_eq!(category(r#"{ "collMod": "c" }"#), Category::Ddl);
        assert_eq!(category(r#"{ "createUser": "x", "pwd": "y", "roles": [] }"#), Category::Admin);
        assert_eq!(category(r#"{ "frobnicate": 1 }"#), Category::Unknown);
        assert_eq!(category("not json"), Category::Unknown);
        assert_eq!(classify_mongo(r#"{ "drop": "events" }"#).statements[0].tables, ["events"]);
    }

    #[test]
    fn grid_changes_are_writes_to_one_table() {
        use crate::drivers::types::CellValue;
        let cell = |value: &str| CellValue {
            column: "id".into(),
            value: Some(value.into()),
        };
        let changes = vec![
            RowChange::Insert { values: vec![cell("1")] },
            RowChange::Delete { key: vec![cell("2")] },
            RowChange::Delete { key: vec![cell("3")] },
        ];
        let classification = classify_changes("orders", &changes);
        assert_eq!(classification.categories(), [Category::Write]);
        assert_eq!(classification.statements.len(), 2);
        assert_eq!(classification.tables(), ["orders"]);
    }
}
