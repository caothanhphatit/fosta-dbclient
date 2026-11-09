//! Statement classification for the statement gateway: read, write, DDL, destructive, admin (plus transaction control and
//! session settings). Each statement of a script is parsed with the engine's sqlparser dialect; when the parser does not know the
//! syntax (PL/SQL, ClickHouse `SYSTEM`, vendor options) a keyword rule decides instead, and the result says which path was used.

use super::split::{self, Dialect, TokenKind};
use super::sqlclassify;
use serde::Serialize;
use sqlparser::ast::{CopyTarget, ObjectType, SetExpr, Statement as Ast};
use sqlparser::dialect::{ClickHouseDialect, GenericDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

pub type ClassifyDialect = Dialect;

const DYNAMIC_SQL: &str = "dynamic SQL may run any statement, including DROP or DELETE";

/// Ordered by how careful the gateway must be: the overall class of a script is its most severe statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StatementKind {
    /// Reads data or metadata only (SELECT, SHOW, EXPLAIN without ANALYZE, read-only PRAGMA).
    Read,
    /// Changes session settings only (SET, USE, RESET).
    Session,
    /// BEGIN / COMMIT / ROLLBACK / SAVEPOINT.
    Transaction,
    /// Changes rows (INSERT, UPDATE/DELETE with WHERE, MERGE, CALL, PL/SQL blocks).
    Write,
    /// Not recognised; treat like a write.
    Unknown,
    /// Creates or alters objects (CREATE, ALTER, COMMENT, RENAME).
    Ddl,
    /// Privileges, users, maintenance and server-level operations (GRANT, CREATE USER, VACUUM, KILL, SYSTEM, ATTACH, COPY to a file).
    Admin,
    /// Loses data: DROP, TRUNCATE, UPDATE/DELETE without WHERE, ALTER ... DROP.
    Destructive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifiedStatement {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub kind: StatementKind,
    /// The leading keyword(s), e.g. `DELETE`, `CREATE TABLE`, `ALTER SYSTEM`.
    pub keyword: String,
    /// Why it is destructive or admin, when that is not obvious from the keyword.
    pub reason: Option<String>,
    /// True when sqlparser understood the statement; false when the keyword rules decided.
    pub parsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub statements: Vec<ClassifiedStatement>,
    /// The most severe kind (Read for an empty script).
    pub kind: StatementKind,
    /// More than one statement.
    pub multi: bool,
}

/// Classifies every statement of `sql`.
pub fn classify(dialect: Dialect, sql: &str) -> Classification {
    let statements: Vec<ClassifiedStatement> = split::split(dialect, sql).into_iter().map(|s| classify_one(dialect, s)).collect();
    let kind = statements.iter().map(|s| s.kind).max().unwrap_or(StatementKind::Read);
    Classification {
        multi: statements.len() > 1,
        kind,
        statements,
    }
}

fn classify_one(dialect: Dialect, s: split::Statement) -> ClassifiedStatement {
    let words = split::words(dialect, &s.text);
    let keyword = leading_keyword(&words);
    let (kind, reason, parsed) = classify_text(dialect, &s.text);
    ClassifiedStatement {
        text: s.text,
        start: s.start,
        end: s.end,
        kind,
        keyword,
        reason,
        parsed,
    }
}

/// The kind of one statement, why, and whether sqlparser understood it.
fn classify_text(dialect: Dialect, text: &str) -> (StatementKind, Option<String>, bool) {
    use StatementKind::*;
    let words = split::words(dialect, text);
    if words.first().is_some_and(|w| w == "EXPLAIN") {
        // Only the analysing forms run their statement; they are what that statement is.
        return match explain_target(dialect, text) {
            Some(start) => {
                let (k, r, parsed) = classify_text(dialect, &text[start..]);
                let reason = (k > Read).then(|| match r {
                    Some(r) => format!("EXPLAIN ANALYZE runs the statement: {r}"),
                    None => "EXPLAIN ANALYZE runs the statement".to_owned(),
                });
                (k.max(Read), reason, parsed)
            }
            None => (Read, None, parse(dialect, text).is_some()),
        };
    }
    let ast = parse(dialect, text);
    let (mut kind, mut reason, parsed) = match &ast {
        Some(ast) => {
            let (k, r) = from_ast(dialect, ast, text);
            // The AST can miss vendor details the keywords catch (ClickHouse ALTER ... DELETE, data-modifying CTEs).
            let (kk, kr) = from_keywords(dialect, text, &words);
            if kk > k && kk != Unknown {
                (kk, kr, true)
            } else {
                (k, r, true)
            }
        }
        None => {
            let (k, r) = from_keywords(dialect, text, &words);
            (k, r, false)
        }
    };
    // Functions that change server state, sleep, lock, touch files or reach other servers (G5).
    if kind < Admin {
        let found = match &ast {
            Some(ast) if !sqlclassify::defines_only(ast) => sqlclassify::side_effect_call(ast),
            Some(_) => None,
            None if matches!(words.first().map(String::as_str), Some("CREATE" | "ALTER")) => None,
            None => sqlclassify::side_effect_in_words(&code_words(dialect, text), dialect == Dialect::ClickHouse),
        };
        if let Some(function) = found {
            kind = Admin;
            reason = Some(sqlclassify::side_effect_reason(&function));
        }
    }
    // Unrecognised text that drops, truncates, restores or shuts down is at least that bad.
    if kind == Unknown {
        if let Some(word) = words
            .iter()
            .find(|w| matches!(w.as_str(), "DROP" | "TRUNCATE" | "RESTORE" | "DBCC" | "SHUTDOWN"))
        {
            kind = Destructive;
            reason = Some(format!("not recognised, and it contains {word}"));
        }
    }
    (kind, reason, parsed)
}

/// Where the statement of an analysing `EXPLAIN` starts: `EXPLAIN ANALYZE|ANALYSE [VERBOSE] stmt` or
/// `EXPLAIN (ANALYZE [true|on|1], BUFFERS, …) stmt`. `None` for an EXPLAIN that only plans.
fn explain_target(dialect: Dialect, text: &str) -> Option<usize> {
    let tokens = split::tokenize(dialect, text);
    let is_analyze = |i: usize| {
        tokens
            .get(i)
            .is_some_and(|t| t.is_word(text, "ANALYZE") || t.is_word(text, "ANALYSE"))
    };
    let mut analyze = false;
    let mut i = 1;
    if tokens.get(1).is_some_and(|t| t.kind == TokenKind::Punct('(')) {
        let mut depth = 0;
        while let Some(token) = tokens.get(i) {
            match token.kind {
                TokenKind::Punct('(') => depth += 1,
                TokenKind::Punct(')') => depth -= 1,
                _ if depth == 1 && is_analyze(i) => {
                    let off = tokens
                        .get(i + 1)
                        .is_some_and(|next| matches!(next.text(text).to_ascii_uppercase().as_str(), "FALSE" | "OFF" | "0"));
                    analyze |= !off;
                }
                _ => {}
            }
            i += 1;
            if depth == 0 {
                break;
            }
        }
    } else {
        while tokens.get(i).is_some_and(|t| is_analyze(i) || t.is_word(text, "VERBOSE")) {
            analyze |= is_analyze(i);
            i += 1;
        }
    }
    tokens.get(i).filter(|_| analyze).map(|t| t.start)
}

/// The unquoted words outside strings and comments, each with the token after it (for function-call detection).
fn code_words(dialect: Dialect, text: &str) -> Vec<(String, String)> {
    let tokens = split::tokenize(dialect, text);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TokenKind::Word)
        .map(|(i, t)| {
            let next = tokens.get(i + 1).map(|n| n.text(text).to_owned()).unwrap_or_default();
            (t.text(text).to_owned(), next)
        })
        .collect()
}

/// `SET`, `RESET`, `DISCARD`, `BEGIN` and `START` forms that lift the read-only backstop or change who the session is:
/// READ WRITE transactions, the protected settings (quotes ignored), `SET ROLE`, `SET SESSION AUTHORIZATION`, `RESET ALL`,
/// `DISCARD ALL`.
fn lifts_protection(dialect: Dialect, text: &str, words: &[String]) -> Option<String> {
    let first = words.first().map(String::as_str)?;
    let read_write = words.windows(2).any(|w| w[0] == "READ" && w[1] == "WRITE");
    let authorization = words.windows(2).any(|w| w[0] == "SESSION" && w[1] == "AUTHORIZATION");
    match first {
        "BEGIN" | "START" if dialect != Dialect::Oracle && read_write => Some("READ WRITE lifts the read-only session".into()),
        "SET" | "RESET" => {
            if read_write {
                return Some("READ WRITE lifts the read-only session".into());
            }
            if authorization {
                return Some("changes the identity of the session".into());
            }
            // The setting's name: the first word or quoted identifier after SET/RESET [SESSION|LOCAL].
            let name = split::tokenize(dialect, text)
                .into_iter()
                .skip(1)
                .find(|t| !(t.is_word(text, "SESSION") || t.is_word(text, "LOCAL")))
                .filter(|t| matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent))
                .map(|t| t.text(text).to_owned())
                .unwrap_or_default();
            if first == "RESET" && name.eq_ignore_ascii_case("ALL") {
                Some("RESET ALL resets the read-only session".into())
            } else if sqlclassify::is_protected_variable(&name) {
                Some(format!(
                    "{} protects this session",
                    name.trim_matches(|c| c == '"' || c == '`').to_lowercase()
                ))
            } else {
                None
            }
        }
        "DISCARD" if words.get(1).is_some_and(|w| w == "ALL") => Some("DISCARD ALL resets the read-only session".into()),
        _ => None,
    }
}

fn parse(dialect: Dialect, text: &str) -> Option<Ast> {
    let result = match dialect {
        Dialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, text),
        Dialect::Sqlite => Parser::parse_sql(&SQLiteDialect {}, text),
        Dialect::ClickHouse => Parser::parse_sql(&ClickHouseDialect {}, text),
        Dialect::Oracle => Parser::parse_sql(&GenericDialect {}, text),
    };
    let mut stmts = result.ok()?;
    (stmts.len() == 1).then(|| stmts.remove(0))
}

fn leading_keyword(words: &[String]) -> String {
    match words.first().map(String::as_str) {
        Some("CREATE" | "ALTER" | "DROP") => {
            // CREATE OR REPLACE [TEMP] VIEW -> "CREATE VIEW"
            let obj = words[1..].iter().find(|w| {
                !matches!(
                    w.as_str(),
                    "OR" | "REPLACE"
                        | "TEMP"
                        | "TEMPORARY"
                        | "UNIQUE"
                        | "GLOBAL"
                        | "LOCAL"
                        | "UNLOGGED"
                        | "EDITIONABLE"
                        | "NONEDITIONABLE"
                        | "IF"
                        | "NOT"
                        | "EXISTS"
                        | "MATERIALIZED"
                        | "PUBLIC"
                        | "VIRTUAL"
                )
            });
            let mut kw = words[0].clone();
            if words.iter().any(|w| w == "MATERIALIZED") {
                kw.push_str(" MATERIALIZED");
            }
            if let Some(o) = obj {
                kw.push(' ');
                kw.push_str(o);
            }
            kw
        }
        _ => words.first().cloned().unwrap_or_default(),
    }
}

fn from_ast(dialect: Dialect, ast: &Ast, text: &str) -> (StatementKind, Option<String>) {
    use StatementKind::*;
    match ast {
        Ast::Query(q) => {
            if let SetExpr::Select(sel) = q.body.as_ref() {
                if sel.into.is_some() {
                    return (Ddl, Some("SELECT INTO creates a table".into()));
                }
            }
            (Read, None)
        }
        // PostgreSQL's EXECUTE runs a prepared statement and Oracle's EXECUTE IMMEDIATE runs a string: either may drop
        // or delete anything. PREPARE readies such a statement.
        Ast::Execute { immediate, .. } if *immediate || dialect == Dialect::Postgres => (Destructive, Some(DYNAMIC_SQL.into())),
        Ast::Prepare { .. } => (Destructive, Some(DYNAMIC_SQL.into())),
        Ast::Insert(_)
        | Ast::Merge(_)
        | Ast::Call(_)
        | Ast::Execute { .. }
        | Ast::LoadData { .. }
        | Ast::Lock(_)
        | Ast::LockTables { .. } => (Write, None),
        Ast::Update(u) => {
            if u.selection.is_none() && u.from.is_none() {
                (Destructive, Some("UPDATE without WHERE changes every row".into()))
            } else {
                (Write, None)
            }
        }
        Ast::Delete(d) => {
            if d.selection.is_none() && d.using.is_none() {
                (Destructive, Some("DELETE without WHERE removes every row".into()))
            } else {
                (Write, None)
            }
        }
        Ast::Truncate(_) => (Destructive, Some("TRUNCATE removes every row".into())),
        Ast::Drop { object_type, .. } => match object_type {
            ObjectType::Role | ObjectType::User => (Admin, Some("drops a role or user".into())),
            _ => (Destructive, Some("DROP removes the object and its data".into())),
        },
        Ast::DropFunction(_)
        | Ast::DropDomain(_)
        | Ast::DropProcedure { .. }
        | Ast::DropTrigger(_)
        | Ast::DropExtension(_)
        | Ast::DropPolicy(_)
        | Ast::DropOperator(_)
        | Ast::DropOperatorFamily(_)
        | Ast::DropOperatorClass(_) => (Destructive, Some("DROP removes the object".into())),
        Ast::AlterTable(at) => {
            if at.operations.iter().any(|op| format!("{op:?}").starts_with("Drop")) {
                (Destructive, Some("ALTER TABLE drops a column, constraint or partition".into()))
            } else {
                (Ddl, None)
            }
        }
        Ast::CreateRole(_) | Ast::AlterRole { .. } | Ast::CreateUser(_) | Ast::AlterUser(_) => {
            (Admin, Some("changes roles or users".into()))
        }
        Ast::Grant(_) | Ast::Revoke(_) | Ast::Deny(_) => (Admin, Some("changes privileges".into())),
        Ast::Kill { .. } => (Admin, Some("kills a session or query".into())),
        Ast::Vacuum(_) | Ast::Analyze(_) | Ast::OptimizeTable { .. } | Ast::Flush { .. } => (Admin, Some("maintenance".into())),
        Ast::AttachDatabase { .. } | Ast::AttachDuckDBDatabase { .. } | Ast::DetachDuckDBDatabase { .. } => {
            (Admin, Some("attaches or detaches a database file".into()))
        }
        Ast::Copy { to, target, .. } => match target {
            CopyTarget::Stdin | CopyTarget::Stdout => (if *to { Read } else { Write }, None),
            _ => (Admin, Some("COPY reads or writes a server file or program".into())),
        },
        Ast::Pragma { value, is_eq, .. } => {
            if value.is_some() && (*is_eq || is_pragma_setter(text)) {
                (Admin, Some("PRAGMA changes a database setting".into()))
            } else {
                (Read, None)
            }
        }
        Ast::Explain { statement, .. } => {
            if sqlclassify::explain_runs(ast) {
                let (k, r) = from_ast(dialect, statement, text);
                (k.max(Read), r.map(|r| format!("EXPLAIN ANALYZE runs the statement: {r}")))
            } else {
                (Read, None)
            }
        }
        Ast::ExplainTable { .. }
        | Ast::ShowFunctions { .. }
        | Ast::ShowVariable { .. }
        | Ast::ShowStatus { .. }
        | Ast::ShowVariables { .. }
        | Ast::ShowCreate { .. }
        | Ast::ShowColumns { .. }
        | Ast::ShowCatalogs { .. }
        | Ast::ShowDatabases { .. }
        | Ast::ShowProcessList { .. }
        | Ast::ShowSchemas { .. }
        | Ast::ShowCharset(_)
        | Ast::ShowObjects(_)
        | Ast::ShowTables { .. }
        | Ast::ShowViews { .. }
        | Ast::ShowCollation { .. }
        | Ast::Fetch { .. }
        | Ast::Close { .. } => (Read, None),
        Ast::StartTransaction { .. } | Ast::Commit { .. } | Ast::Rollback { .. } | Ast::Savepoint { .. } | Ast::ReleaseSavepoint { .. } => {
            (Transaction, None)
        }
        Ast::Set(_) | Ast::Use(_) | Ast::Reset(_) | Ast::Discard { .. } | Ast::Deallocate { .. } | Ast::Declare { .. } => (Session, None),
        Ast::Comment { .. } | Ast::RenameTable(_) => (Ddl, None),
        other => {
            let name = format!("{other:?}");
            if name.starts_with("Create") || name.starts_with("Alter") {
                // CREATE/ALTER of databases, views, indexes, functions, sequences, types, extensions, ...
                if name.starts_with("AlterSession") {
                    (Session, None)
                } else {
                    (Ddl, None)
                }
            } else if name.starts_with("Drop") {
                (Destructive, Some("DROP removes the object".into()))
            } else {
                (Unknown, None)
            }
        }
    }
}

/// Keyword rules: used when sqlparser cannot parse the statement, and to catch vendor details the AST does not show.
fn from_keywords(dialect: Dialect, text: &str, words: &[String]) -> (StatementKind, Option<String>) {
    use StatementKind::*;
    let first = words.first().map(String::as_str).unwrap_or("");
    let second = words.get(1).map(String::as_str).unwrap_or("");
    let has = |w: &str| words.iter().any(|x| x == w);
    let starts_with_paren = split::tokenize(dialect, text)
        .first()
        .is_some_and(|t| t.kind == TokenKind::Punct('('));
    if let Some(reason) = lifts_protection(dialect, text, words) {
        return (Admin, Some(reason));
    }
    match first {
        "" if starts_with_paren => (Read, None),
        "SELECT" | "VALUES" | "TABLE" | "SHOW" | "DESCRIBE" | "DESC" | "EXISTS" | "FETCH" | "WATCH" => {
            if first == "SELECT" && dialect == Dialect::Postgres && has("INTO") && !has("FROM") {
                (Ddl, Some("SELECT INTO creates a table".into()))
            } else {
                (Read, None)
            }
        }
        "WITH" => {
            if has("INSERT") || has("UPDATE") || has("DELETE") || has("MERGE") {
                (Write, Some("the WITH query changes rows".into()))
            } else {
                (Read, None)
            }
        }
        // classify_text classifies the statement an analysing EXPLAIN runs.
        "EXPLAIN" => (Read, None),
        "EXECUTE" | "EXEC" if dialect == Dialect::Postgres || second == "IMMEDIATE" => (Destructive, Some(DYNAMIC_SQL.into())),
        "PREPARE" if second != "TRANSACTION" => (Destructive, Some(DYNAMIC_SQL.into())),
        "DO" if text.to_ascii_uppercase().contains("EXECUTE") => (
            Destructive,
            Some("the DO block runs dynamic SQL, which may drop or delete anything".into()),
        ),
        "INSERT" | "UPSERT" | "MERGE" | "REPLACE" | "CALL" | "EXEC" | "EXECUTE" | "DO" | "LOCK" | "LOAD" => (Write, None),
        "UPDATE" | "DELETE" => {
            if has("WHERE") {
                (Write, None)
            } else {
                (Destructive, Some(format!("{first} without WHERE affects every row")))
            }
        }
        "DECLARE" | "BEGIN" if dialect == Dialect::Oracle && words.windows(2).any(|w| w[0] == "EXECUTE" && w[1] == "IMMEDIATE") => (
            Destructive,
            Some("the PL/SQL block runs dynamic SQL (EXECUTE IMMEDIATE), which may drop or delete anything".into()),
        ),
        "DECLARE" if dialect == Dialect::Oracle => (Write, Some("PL/SQL block".into())),
        "BEGIN" if dialect == Dialect::Oracle => (Write, Some("PL/SQL block".into())),
        "BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "ABORT" | "PREPARE"
            if first != "PREPARE" || second == "TRANSACTION" =>
        {
            (Transaction, None)
        }
        "SET" if second == "TRANSACTION" => (Transaction, None),
        "SET" | "RESET" | "USE" | "DISCARD" | "LISTEN" | "UNLISTEN" | "NOTIFY" => (Session, None),
        "DROP" | "TRUNCATE" | "PURGE" => {
            if matches!(second, "USER" | "ROLE" | "PROFILE") {
                (Admin, Some("drops a user or role".into()))
            } else {
                (Destructive, Some(format!("{first} removes data")))
            }
        }
        "FLASHBACK" => (Destructive, Some("FLASHBACK rewinds a table".into())),
        "CREATE" | "ALTER" => {
            let target = words[1..]
                .iter()
                .find(|w| {
                    !matches!(
                        w.as_str(),
                        "OR" | "REPLACE" | "PUBLIC" | "GLOBAL" | "TEMP" | "TEMPORARY" | "UNIQUE" | "IF" | "NOT" | "EXISTS"
                    )
                })
                .map(String::as_str)
                .unwrap_or("");
            if matches!(
                target,
                "USER" | "ROLE" | "PROFILE" | "SYSTEM" | "TABLESPACE" | "QUOTA" | "SETTINGS" | "ROW" | "RESOURCE" | "DIRECTORY"
            ) {
                return (Admin, Some(format!("{first} {target} changes server administration")));
            }
            if first == "ALTER" && target == "SESSION" {
                return (Session, None);
            }
            if first == "ALTER" && dialect == Dialect::ClickHouse && (has("DELETE") || has("UPDATE")) && !has("DROP") {
                return (Write, Some("ClickHouse mutation".into()));
            }
            if first == "ALTER" && (has("DROP") || has("TRUNCATE") || has("DETACH")) {
                return (Destructive, Some("ALTER drops a column, constraint or partition".into()));
            }
            (Ddl, None)
        }
        "COMMENT" | "RENAME" | "REFRESH" | "REINDEX" if first != "REINDEX" => (Ddl, None),
        "GRANT" | "REVOKE" | "DENY" => (Admin, Some("changes privileges".into())),
        "VACUUM" | "ANALYZE" | "REINDEX" | "CLUSTER" | "CHECKPOINT" | "OPTIMIZE" | "SYSTEM" | "KILL" | "ATTACH" | "DETACH" | "SHUTDOWN"
        | "STARTUP" | "AUDIT" | "NOAUDIT" | "BACKUP" | "RESTORE" | "SECURITY" | "UNDROP" | "EXCHANGE" | "MOVE" => {
            (Admin, Some(format!("{first} is a server or maintenance operation")))
        }
        "PRAGMA" => {
            if text.contains('=') || text.contains('(') && !text.trim_end().ends_with("()") && is_pragma_setter(text) {
                (Admin, Some("PRAGMA changes a database setting".into()))
            } else {
                (Read, None)
            }
        }
        "COPY" => {
            if has("STDIN") || has("STDOUT") {
                (Write, None)
            } else {
                (Admin, Some("COPY reads or writes a server file or program".into()))
            }
        }
        _ => (Unknown, None),
    }
}

/// `PRAGMA x(arg)` reads for table-valued pragmas (table_info(t)) but sets for value pragmas (cache_size(100)).
fn is_pragma_setter(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ![
        "table_info",
        "table_xinfo",
        "index_list",
        "index_info",
        "index_xinfo",
        "foreign_key_list",
        "foreign_key_check",
        "integrity_check",
        "quick_check",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::StatementKind::*;
    use super::*;

    fn kind(d: Dialect, sql: &str) -> StatementKind {
        classify(d, sql).kind
    }

    #[test]
    fn classification_table_postgres() {
        let cases: &[(&str, StatementKind)] = &[
            ("SELECT * FROM orders WHERE id = 1", Read),
            ("WITH x AS (SELECT 1) SELECT * FROM x", Read),
            ("WITH d AS (DELETE FROM orders WHERE id = 1 RETURNING *) SELECT * FROM d", Write),
            ("SELECT * INTO backup FROM orders", Ddl),
            ("EXPLAIN SELECT 1", Read),
            ("EXPLAIN ANALYZE DELETE FROM orders WHERE id = 1", Write),
            ("INSERT INTO t VALUES (1)", Write),
            ("UPDATE t SET a = 1 WHERE id = 2", Write),
            ("UPDATE t SET a = 1", Destructive),
            ("DELETE FROM t", Destructive),
            ("DELETE FROM t USING u WHERE t.id = u.id", Write),
            ("TRUNCATE t", Destructive),
            ("DROP TABLE t", Destructive),
            ("DROP ROLE bob", Admin),
            ("CREATE TABLE t (id int)", Ddl),
            ("CREATE INDEX i ON t (a)", Ddl),
            ("CREATE OR REPLACE VIEW v AS SELECT 1", Ddl),
            ("ALTER TABLE t ADD COLUMN b int", Ddl),
            ("ALTER TABLE t DROP COLUMN b", Destructive),
            ("GRANT SELECT ON t TO bob", Admin),
            ("CREATE ROLE bob", Admin),
            ("ALTER SYSTEM SET work_mem = '64MB'", Admin),
            ("VACUUM ANALYZE t", Admin),
            ("COPY t TO '/tmp/x.csv'", Admin),
            ("COPY t FROM STDIN", Write),
            ("BEGIN", Transaction),
            ("COMMIT", Transaction),
            ("ROLLBACK TO SAVEPOINT a", Transaction),
            ("SET search_path = sales", Session),
            ("SHOW search_path", Read),
            ("CALL do_it()", Write),
            ("DO $$ BEGIN PERFORM 1; END $$", Write),
            ("REFRESH MATERIALIZED VIEW mv", Ddl),
            ("COMMENT ON TABLE t IS 'x'", Ddl),
            ("frobnicate the database", Unknown),
        ];
        for (sql, expected) in cases {
            assert_eq!(kind(Dialect::Postgres, sql), *expected, "{sql}");
        }
    }

    #[test]
    fn classification_table_sqlite() {
        let cases: &[(&str, StatementKind)] = &[
            ("SELECT * FROM t", Read),
            ("PRAGMA table_info(t)", Read),
            ("PRAGMA foreign_keys", Read),
            ("PRAGMA journal_mode = WAL", Admin),
            ("ATTACH DATABASE 'x.db' AS x", Admin),
            ("DETACH DATABASE x", Admin),
            ("INSERT OR REPLACE INTO t VALUES (1)", Write),
            ("REPLACE INTO t VALUES (1)", Write),
            ("BEGIN IMMEDIATE", Transaction),
            ("END", Transaction),
            ("CREATE TRIGGER tr AFTER INSERT ON t BEGIN DELETE FROM u; END", Ddl),
            ("DROP VIEW v", Destructive),
            ("VACUUM", Admin),
        ];
        for (sql, expected) in cases {
            assert_eq!(kind(Dialect::Sqlite, sql), *expected, "{sql}");
        }
    }

    #[test]
    fn classification_table_clickhouse() {
        let cases: &[(&str, StatementKind)] = &[
            ("SELECT count() FROM system.numbers LIMIT 10", Read),
            ("SHOW TABLES FROM shop", Read),
            ("EXISTS TABLE shop.orders", Read),
            ("INSERT INTO t FORMAT JSONEachRow", Write),
            ("ALTER TABLE t DELETE WHERE id = 1", Write),
            ("ALTER TABLE t UPDATE a = 1 WHERE id = 1", Write),
            ("ALTER TABLE t DROP PARTITION 202501", Destructive),
            ("DELETE FROM t WHERE id = 1", Write),
            ("TRUNCATE TABLE t", Destructive),
            ("DROP DICTIONARY d", Destructive),
            ("SYSTEM RELOAD DICTIONARIES", Admin),
            ("KILL QUERY WHERE query_id = 'x'", Admin),
            ("OPTIMIZE TABLE t FINAL", Admin),
            ("CREATE MATERIALIZED VIEW mv TO t AS SELECT 1", Ddl),
            ("CREATE USER bob IDENTIFIED BY 'x'", Admin),
            ("SET max_threads = 2", Session),
        ];
        for (sql, expected) in cases {
            assert_eq!(kind(Dialect::ClickHouse, sql), *expected, "{sql}");
        }
    }

    #[test]
    fn classification_table_oracle() {
        let cases: &[(&str, StatementKind)] = &[
            ("SELECT * FROM dual", Read),
            ("SELECT * FROM t FETCH FIRST 10 ROWS ONLY", Read),
            ("BEGIN\n  UPDATE t SET a = 1;\nEND;", Write),
            ("DECLARE x NUMBER; BEGIN x := 1; END;", Write),
            ("CREATE OR REPLACE PROCEDURE p IS BEGIN NULL; END;", Ddl),
            ("CREATE OR REPLACE PACKAGE BODY pk AS END;", Ddl),
            (
                "MERGE INTO t USING s ON (t.id = s.id) WHEN MATCHED THEN UPDATE SET t.a = s.a",
                Write,
            ),
            ("DROP TABLE t PURGE", Destructive),
            ("PURGE RECYCLEBIN", Destructive),
            ("FLASHBACK TABLE t TO BEFORE DROP", Destructive),
            ("ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD'", Session),
            ("ALTER SYSTEM KILL SESSION '1,2'", Admin),
            ("CREATE USER bob IDENTIFIED BY x", Admin),
            ("GRANT CREATE SESSION TO bob", Admin),
            ("CREATE SYNONYM s FOR t", Ddl),
            ("COMMIT", Transaction),
            ("SAVEPOINT a", Transaction),
        ];
        for (sql, expected) in cases {
            assert_eq!(kind(Dialect::Oracle, sql), *expected, "{sql}");
        }
    }

    #[test]
    fn multi_statement_detection_and_overall_kind() {
        let c = classify(Dialect::Postgres, "SELECT 1; DELETE FROM t; -- trailing\n");
        assert!(c.multi);
        assert_eq!(c.statements.len(), 2);
        assert_eq!(c.kind, Destructive);
        assert_eq!(c.statements[1].keyword, "DELETE");
        assert!(c.statements[0].parsed);
        let one = classify(Dialect::Postgres, "select 1;");
        assert!(!one.multi);
        let plsql = classify(Dialect::Oracle, "BEGIN NULL; END;\n/\nSELECT 1 FROM dual");
        assert!(plsql.multi);
        assert!(!plsql.statements[0].parsed);
        assert_eq!(
            classify(Dialect::Postgres, "CREATE OR REPLACE VIEW v AS SELECT 1").statements[0].keyword,
            "CREATE VIEW"
        );
        assert_eq!(classify(Dialect::Postgres, "  ").kind, Read);
    }

    fn check(d: Dialect, cases: &[(&str, StatementKind)]) {
        let wrong: Vec<String> = cases
            .iter()
            .filter_map(|(sql, expected)| {
                let got = kind(d, sql);
                (got != *expected).then(|| format!("{sql}: expected {expected:?}, got {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn explain_analyse_runs_its_statement() {
        check(
            Dialect::Postgres,
            &[
                ("EXPLAIN ANALYZE DELETE FROM t WHERE id = 1", Write),
                ("EXPLAIN ANALYSE DELETE FROM t WHERE id = 1", Write),
                ("explain analyse delete from t", Destructive),
                ("EXPLAIN ANALYZE VERBOSE UPDATE t SET a = 1", Destructive),
                ("EXPLAIN (ANALYZE) DELETE FROM t WHERE id = 1", Write),
                ("EXPLAIN (ANALYZE) DELETE FROM t", Destructive),
                ("EXPLAIN (ANALYSE) DELETE FROM t", Destructive),
                ("EXPLAIN (ANALYSE, BUFFERS) INSERT INTO t VALUES (1)", Write),
                ("EXPLAIN (FORMAT JSON, ANALYZE true, BUFFERS) DELETE FROM t", Destructive),
                ("EXPLAIN (ANALYZE) EXECUTE p", Destructive),
                ("EXPLAIN ANALYZE EXECUTE p(1)", Destructive),
                (
                    "EXPLAIN (ANALYZE) WITH d AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM d",
                    Write,
                ),
                ("EXPLAIN ANALYZE SELECT pg_terminate_backend(42)", Admin),
                ("EXPLAIN ANALYZE SELECT * FROM t", Read),
                ("EXPLAIN (ANALYZE false) DELETE FROM t", Read),
                ("EXPLAIN (ANALYZE off, COSTS) DELETE FROM t", Read),
                ("EXPLAIN (COSTS, VERBOSE) DELETE FROM t", Read),
                ("EXPLAIN DELETE FROM t", Read),
                ("EXPLAIN SELECT pg_sleep(10)", Read),
            ],
        );
        let c = classify(Dialect::Postgres, "EXPLAIN (ANALYSE) DELETE FROM t");
        assert!(c.statements[0].reason.as_deref().unwrap().contains("EXPLAIN ANALYZE runs"), "{c:?}");
    }

    #[test]
    fn statements_that_lift_the_read_only_session_are_admin() {
        check(
            Dialect::Postgres,
            &[
                ("BEGIN", Transaction),
                ("BEGIN ISOLATION LEVEL SERIALIZABLE", Transaction),
                ("START TRANSACTION", Transaction),
                ("START TRANSACTION READ ONLY", Transaction),
                ("BEGIN READ ONLY", Transaction),
                ("BEGIN READ WRITE", Admin),
                ("BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED, READ WRITE", Admin),
                ("START TRANSACTION READ WRITE", Admin),
                ("SET TRANSACTION READ WRITE", Admin),
                ("SET TRANSACTION READ ONLY", Transaction),
                ("SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE", Admin),
                ("SET default_transaction_read_only = off", Admin),
                ("SET SESSION default_transaction_read_only TO false", Admin),
                ("SET LOCAL transaction_read_only = off", Admin),
                ("SET \"default_transaction_read_only\" = off", Admin),
                ("set Default_Transaction_Read_Only = 'off'", Admin),
                ("RESET ALL", Admin),
                ("RESET default_transaction_read_only", Admin),
                ("RESET ROLE", Admin),
                ("RESET SESSION AUTHORIZATION", Admin),
                ("RESET search_path", Session),
                ("DISCARD ALL", Admin),
                ("DISCARD PLANS", Session),
                ("SET ROLE admin", Admin),
                ("SET SESSION ROLE admin", Admin),
                ("SET SESSION AUTHORIZATION bob", Admin),
                ("SET search_path = sales", Session),
                ("SELECT set_config('default_transaction_read_only', 'off', false)", Admin),
            ],
        );
    }

    #[test]
    fn side_effect_functions_are_admin() {
        check(
            Dialect::Postgres,
            &[
                ("SELECT pg_terminate_backend(pid) FROM pg_stat_activity", Admin),
                ("select PG_CATALOG.PG_TERMINATE_BACKEND(42)", Admin),
                ("SELECT \"pg_cancel_backend\"(42)", Admin),
                ("SELECT pg_reload_conf()", Admin),
                ("SELECT pg_sleep(5)", Admin),
                ("SELECT pg_sleep_for('5 minutes')", Admin),
                ("SELECT * FROM dblink('host=x', 'DELETE FROM t') AS r(a int)", Admin),
                ("SELECT dblink_exec('host=x', 'DROP TABLE t')", Admin),
                ("SELECT lo_import('/etc/passwd')", Admin),
                ("SELECT pg_read_file('postgresql.conf')", Admin),
                ("SELECT * FROM pg_ls_dir('.')", Admin),
                ("SELECT nextval('s')", Admin),
                ("SELECT pg_advisory_lock(1)", Admin),
                ("SELECT * FROM t WHERE id IN (SELECT pg_sleep(1)::int)", Admin),
                ("WITH x AS (SELECT setval('s', 1)) SELECT * FROM x", Admin),
                ("SELECT pg_size_pretty(pg_database_size('x'))", Read),
                ("CREATE TABLE t (id int DEFAULT nextval('s'))", Ddl),
                ("CREATE VIEW v AS SELECT pg_sleep(1)", Ddl),
            ],
        );
        let c = classify(Dialect::Postgres, "SELECT pg_catalog.pg_terminate_backend(42)");
        assert_eq!(
            c.statements[0].reason.as_deref(),
            Some("pg_terminate_backend() changes server state")
        );
        check(
            Dialect::ClickHouse,
            &[
                ("SELECT * FROM url('http://x/a.csv', CSV, 'a String')", Admin),
                ("SELECT * FROM remote('other:9000', db.t)", Admin),
                ("SELECT * FROM remoteSecure('other:9440', db.t)", Admin),
                ("SELECT * FROM s3('https://b/x.csv')", Admin),
                ("SELECT * FROM file('x.csv')", Admin),
                ("SELECT * FROM mysql('h:3306', 'db', 't', 'u', 'p')", Admin),
                ("SELECT * FROM postgresql('h:5432', 'db', 't', 'u', 'p')", Admin),
                ("SELECT count() FROM numbers(10)", Read),
                ("SELECT url FROM t", Read),
                (
                    "SELECT * FROM t WHERE id IN (SELECT id FROM url('http://x', CSV, 'id UInt64'))",
                    Admin,
                ),
            ],
        );
        check(
            Dialect::Oracle,
            &[
                ("SELECT UTL_HTTP.REQUEST('http://x') FROM dual", Admin),
                ("SELECT sys.utl_inaddr.get_host_address('x') FROM dual", Admin),
                ("SELECT HTTPURITYPE('http://x').getclob() FROM dual", Admin),
                ("SELECT DBMS_PIPE.RECEIVE_MESSAGE('x', 10) FROM dual", Admin),
                ("SELECT DBMS_METADATA.GET_DDL('TABLE', 'T') FROM dual", Read),
                ("BEGIN UTL_FILE.FREMOVE('DIR', 'x'); END;", Admin),
                ("BEGIN DBMS_OUTPUT.PUT_LINE('x'); END;", Write),
            ],
        );
        check(Dialect::Sqlite, &[("SELECT load_extension('evil')", Admin)]);
    }

    #[test]
    fn dynamic_sql_and_unknown_drops_are_destructive() {
        check(
            Dialect::Postgres,
            &[
                ("EXECUTE p", Destructive),
                ("EXECUTE p(1, 'a')", Destructive),
                ("PREPARE p AS DELETE FROM t WHERE id = $1", Destructive),
                ("PREPARE p (int) AS SELECT $1", Destructive),
                ("PREPARE TRANSACTION 'x'", Transaction),
                ("DO $$ BEGIN EXECUTE 'DROP TABLE t'; END $$", Destructive),
                ("DO $$ BEGIN PERFORM 1; END $$", Write),
                ("/* c */ DROP TABLE t", Destructive),
                ("FROBNICATE TABLE t; ", Unknown),
                ("FROBNICATE then DROP t", Destructive),
                ("FROBNICATE 'DROP'", Unknown),
            ],
        );
        check(
            Dialect::Oracle,
            &[
                ("BEGIN EXECUTE IMMEDIATE 'DROP TABLE t'; END;", Destructive),
                ("DECLARE s VARCHAR2(100) := 'x'; BEGIN EXECUTE IMMEDIATE s; END;", Destructive),
                ("BEGIN UPDATE t SET a = 1 WHERE id = 2; END;", Write),
                ("SHUTDOWN IMMEDIATE", Admin),
            ],
        );
    }
}
