//! Keyword knowledge for the formatter: which words it may re-case and which start clauses.
//!
//! Re-casing a word is only safe when it cannot be an identifier, because ClickHouse identifiers (and SQL Server ones
//! under a case-sensitive collation) are case-sensitive. So a word is re-cased when [`is_keyword_at`] says so:
//! - a reserved word of the dialect, not next to a `.` and, when a `(` follows, only if it is not also a function name;
//! - a soft clause word (`LIMIT`, `OFFSET`, `RETURNING`...) in a clause position;
//! - a statement word (`BEGIN`, `COMMIT`, `EXEC`...) at the start of a statement;
//! - the second word of a known phrase (`PRIMARY KEY`, `NULLS FIRST`, `ON CONFLICT`, `CREATE VIEW`...).
//!
//! Function names, type names and every other word keep the case they were typed in.

use super::{
    lexer::{Token, TokenKind},
    Dialect,
};

const CORE_RESERVED: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "CHECK",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "FALSE",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "GRANT",
    "GROUP",
    "HAVING",
    "IN",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "NATURAL",
    "NOT",
    "NULL",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "REVOKE",
    "RIGHT",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "WITH",
];

const MYSQL_RESERVED: &[&str] = &[
    "ADD",
    "BEGIN",
    "BINARY",
    "CALL",
    "CASCADE",
    "CHANGE",
    "COLLATE",
    "COLUMN",
    "DATABASE",
    "DECLARE",
    "DESCRIBE",
    "DIV",
    "DO",
    "DUAL",
    "ELSEIF",
    "EXPLAIN",
    "FULLTEXT",
    "IF",
    "IGNORE",
    "INDEX",
    "INTERVAL",
    "ITERATE",
    "KEY",
    "KILL",
    "LEAVE",
    "LIMIT",
    "LOCK",
    "LOOP",
    "OVER",
    "PARTITION",
    "PROCEDURE",
    "RANGE",
    "RECURSIVE",
    "REGEXP",
    "RENAME",
    "REPEAT",
    "REPLACE",
    "RESTRICT",
    "RETURN",
    "RLIKE",
    "SCHEMA",
    "SHOW",
    "STRAIGHT_JOIN",
    "TRIGGER",
    "UNLOCK",
    "UNTIL",
    "USE",
    "WHILE",
    "WINDOW",
    "XOR",
];

const MSSQL_RESERVED: &[&str] = &[
    "ADD",
    "BEGIN",
    "BREAK",
    "CASCADE",
    "CLOSE",
    "COLUMN",
    "COMMIT",
    "CONTINUE",
    "CURSOR",
    "DATABASE",
    "DEALLOCATE",
    "DECLARE",
    "EXEC",
    "EXECUTE",
    "FETCH",
    "FUNCTION",
    "GOTO",
    "IF",
    "INDEX",
    "KEY",
    "MERGE",
    "OPEN",
    "OVER",
    "PIVOT",
    "PRINT",
    "PROC",
    "PROCEDURE",
    "RAISERROR",
    "RETURN",
    "ROLLBACK",
    "TOP",
    "TRAN",
    "TRANSACTION",
    "TRIGGER",
    "TRUNCATE",
    "UNPIVOT",
    "USE",
    "VIEW",
    "WAITFOR",
    "WHILE",
];

const POSTGRES_RESERVED: &[&str] = &[
    "ANALYZE",
    "ARRAY",
    "COLLATE",
    "DO",
    "FETCH",
    "ILIKE",
    "LATERAL",
    "LIMIT",
    "OFFSET",
    "ONLY",
    "RETURNING",
    "SIMILAR",
    "VARIADIC",
    "WINDOW",
];

const SQLITE_RESERVED: &[&str] = &[
    "AUTOINCREMENT",
    "BEGIN",
    "GLOB",
    "IF",
    "INDEX",
    "LIMIT",
    "OFFSET",
    "PRAGMA",
    "TRIGGER",
    "VIEW",
];

const CLICKHOUSE_RESERVED: &[&str] = &["ARRAY", "FINAL", "GLOBAL", "ILIKE", "LIMIT", "PREWHERE", "SAMPLE"];

const ORACLE_RESERVED: &[&str] = &[
    "BEGIN",
    "CONNECT",
    "DECLARE",
    "ELSIF",
    "EXCEPTION",
    "EXIT",
    "IF",
    "INDEX",
    "LOOP",
    "MERGE",
    "MINUS",
    "OF",
    "PRIOR",
    "PROCEDURE",
    "FUNCTION",
    "RAISE",
    "RETURN",
    "START",
    "TRIGGER",
    "VIEW",
    "WHILE",
];

const GENERIC_RESERVED: &[&str] = &["FETCH", "LIMIT", "OFFSET"];

/// Clause words that are not reserved everywhere; they are clauses (and re-cased) only in a clause position.
pub const SOFT_CLAUSE: &[&str] = &[
    "FETCH",
    "FORMAT",
    "LIMIT",
    "OFFSET",
    "OUTPUT",
    "PREWHERE",
    "QUALIFY",
    "RETURNING",
    "SAMPLE",
    "SETTINGS",
    "WINDOW",
];

/// Words re-cased when they start a statement.
const STATEMENT_WORDS: &[&str] = &[
    "ANALYZE",
    "ATTACH",
    "BEGIN",
    "CALL",
    "CLOSE",
    "COMMENT",
    "COMMIT",
    "COPY",
    "DEALLOCATE",
    "DECLARE",
    "DESCRIBE",
    "DETACH",
    "DO",
    "EXEC",
    "EXECUTE",
    "EXPLAIN",
    "FETCH",
    "IF",
    "KILL",
    "LOCK",
    "LOOP",
    "MERGE",
    "OPEN",
    "OPTIMIZE",
    "PRAGMA",
    "PREPARE",
    "PRINT",
    "RAISERROR",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "RESET",
    "RETURN",
    "ROLLBACK",
    "SAVEPOINT",
    "SHOW",
    "START",
    "SYSTEM",
    "THROW",
    "TRUNCATE",
    "UNLOCK",
    "USE",
    "VACUUM",
    "WHILE",
];

/// (previous word, word): the word is re-cased right after the previous one.
const PHRASES: &[(&str, &[&str])] = &[
    ("PRIMARY", &["KEY"]),
    ("FOREIGN", &["KEY"]),
    ("UNIQUE", &["KEY", "INDEX"]),
    ("DUPLICATE", &["KEY"]),
    ("ON", &["CONFLICT", "DUPLICATE", "DELETE", "UPDATE"]),
    ("DO", &["NOTHING", "UPDATE"]),
    ("NULLS", &["FIRST", "LAST"]),
    ("FETCH", &["FIRST", "NEXT"]),
    ("FIRST", &["ROW", "ROWS"]),
    ("NEXT", &["ROW", "ROWS"]),
    ("ROW", &["ONLY", "FETCH"]),
    ("ROWS", &["ONLY", "BETWEEN", "FETCH"]),
    ("CROSS", &["APPLY"]),
    ("OUTER", &["APPLY"]),
    ("WHEN", &["MATCHED"]),
    ("NOT", &["MATCHED", "ATOMIC"]),
    ("WITHIN", &["GROUP"]),
    ("RANGE", &["BETWEEN"]),
    ("UNBOUNDED", &["PRECEDING", "FOLLOWING"]),
    ("CURRENT", &["ROW"]),
    (
        "BEGIN",
        &["ATOMIC", "TRAN", "TRANSACTION", "TRY", "CATCH", "WORK", "DISTRIBUTED", "NOT"],
    ),
    ("END", &["TRY", "CATCH", "IF", "LOOP", "WHILE", "REPEAT"]),
    ("COMMIT", &["TRAN", "TRANSACTION", "WORK"]),
    ("ROLLBACK", &["TRAN", "TRANSACTION", "WORK"]),
    (
        "CREATE",
        &[
            "TEMPORARY",
            "TEMP",
            "MATERIALIZED",
            "VIEW",
            "INDEX",
            "DATABASE",
            "SCHEMA",
            "SEQUENCE",
            "TRIGGER",
            "FUNCTION",
            "PROCEDURE",
            "PROC",
            "TYPE",
            "EXTENSION",
            "USER",
            "ROLE",
            "PACKAGE",
            "EVENT",
            "OR",
        ],
    ),
    ("OR", &["REPLACE", "ALTER"]),
    ("REPLACE", &["VIEW", "FUNCTION", "PROCEDURE", "PACKAGE", "TRIGGER", "TYPE"]),
    (
        "ALTER",
        &[
            "VIEW",
            "INDEX",
            "DATABASE",
            "SCHEMA",
            "SEQUENCE",
            "FUNCTION",
            "PROCEDURE",
            "PROC",
            "TYPE",
            "USER",
            "ROLE",
            "COLUMN",
        ],
    ),
    (
        "DROP",
        &[
            "VIEW",
            "INDEX",
            "DATABASE",
            "SCHEMA",
            "SEQUENCE",
            "TRIGGER",
            "FUNCTION",
            "PROCEDURE",
            "PROC",
            "TYPE",
            "COLUMN",
            "CONSTRAINT",
        ],
    ),
    ("TRUNCATE", &["TABLE"]),
    ("WITH", &["RECURSIVE", "TIES", "ROLLUP", "CUBE"]),
    ("UNION", &["ALL"]),
    ("ADD", &["COLUMN", "CONSTRAINT"]),
    ("RENAME", &["TO", "COLUMN"]),
    ("PARTITION", &["BY"]),
    ("INSERT", &["IGNORE"]),
    ("TEMPORARY", &["TABLE", "VIEW"]),
    ("TEMP", &["TABLE", "VIEW"]),
    ("MATERIALIZED", &["VIEW"]),
    ("IS", &["DISTINCT"]),
    ("LEFT", &["ARRAY"]),
    ("ARRAY", &["JOIN"]),
];

/// Phrase starters re-cased when the next word completes the phrase (`NULLS FIRST`, `ROWS ONLY`, `CURRENT ROW`).
const LEAD_ANCHORS: &[&str] = &[
    "NULLS",
    "UNBOUNDED",
    "ROW",
    "ROWS",
    "RANGE",
    "CURRENT",
    "BEGIN",
    "PARTITION",
    "WITHIN",
];

/// Words that are keywords right after a closing parenthesis of a call.
const AFTER_CALL: &[&str] = &["OVER", "FILTER", "WITHIN"];

/// Reserved words that keep their keyword meaning before `(`; any other reserved word before `(` may be a function
/// (`LEFT(`, `REPLACE(`, `IF(`) and keeps its case.
pub const PAREN_KEYWORDS: &[&str] = &[
    "ALL",
    "AND",
    "ANY",
    "AS",
    "BY",
    "CHECK",
    "DEFAULT",
    "ELSE",
    "EXISTS",
    "FROM",
    "IN",
    "INTO",
    "JOIN",
    "KEY",
    "NOT",
    "ON",
    "OR",
    "OVER",
    "SELECT",
    "SOME",
    "TABLE",
    "THEN",
    "UNION",
    "UNIQUE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "WITH",
    "RETURNING",
    "LIMIT",
    "CASE",
    "BETWEEN",
    "LIKE",
    "IS",
    "INTERSECT",
    "EXCEPT",
    "MINUS",
    "SET",
    "LATERAL",
    "ARRAY",
    "END",
    "REFERENCES",
    "TO",
    "INSERT",
];

fn dialect_reserved(d: Dialect) -> &'static [&'static str] {
    match d {
        Dialect::MySql | Dialect::MariaDb => MYSQL_RESERVED,
        Dialect::MsSql => MSSQL_RESERVED,
        Dialect::Postgres => POSTGRES_RESERVED,
        Dialect::Sqlite => SQLITE_RESERVED,
        Dialect::ClickHouse => CLICKHOUSE_RESERVED,
        Dialect::Oracle => ORACLE_RESERVED,
        Dialect::Generic => GENERIC_RESERVED,
    }
}

/// True when `upper` (an upper-cased word) is a reserved word of the dialect.
pub fn is_reserved(d: Dialect, upper: &str) -> bool {
    CORE_RESERVED.contains(&upper) || dialect_reserved(d).contains(&upper)
}

pub fn is_soft_clause(upper: &str) -> bool {
    SOFT_CLAUSE.contains(&upper)
}

pub fn is_statement_word(upper: &str) -> bool {
    STATEMENT_WORDS.contains(&upper)
}

pub fn is_phrase(prev_upper: &str, upper: &str) -> bool {
    PHRASES.iter().any(|(p, ws)| *p == prev_upper && ws.contains(&upper))
}

/// True when a token ends a value (so a following soft clause word is a clause, and a following `-` is binary).
pub fn ends_value(d: Dialect, src: &str, t: &Token) -> bool {
    match t.kind {
        TokenKind::Number | TokenKind::Str | TokenKind::QuotedIdent | TokenKind::Param | TokenKind::Variable => true,
        TokenKind::Punct => matches!(t.text(src), ")" | "]"),
        TokenKind::Word => {
            let upper = t.text(src).to_ascii_uppercase();
            !is_reserved(d, &upper) || matches!(upper.as_str(), "END" | "NULL" | "TRUE" | "FALSE" | "ASC" | "DESC")
        }
        _ => false,
    }
}

/// Whether the significant token `i` is a keyword whose case the formatter may change. `sig` holds the significant
/// tokens (no whitespace or comments). The answer depends only on the tokens, so the verifier gets the same one.
pub fn is_keyword_at(d: Dialect, src: &str, sig: &[Token], i: usize) -> bool {
    let t = &sig[i];
    if t.kind != TokenKind::Word {
        return false;
    }
    let text = t.text(src);
    if !text.bytes().all(|c| c.is_ascii_alphabetic() || c == b'_') {
        return false;
    }
    let prev = i.checked_sub(1).map(|p| &sig[p]);
    let next = sig.get(i + 1);
    if prev.is_some_and(|p| p.is_punct(src, '.')) || next.is_some_and(|n| n.is_punct(src, '.')) {
        return false;
    }
    let upper = text.to_ascii_uppercase();
    let before_paren = next.is_some_and(|n| n.is_punct(src, '('));
    if is_reserved(d, &upper) {
        return !before_paren || PAREN_KEYWORDS.contains(&upper.as_str());
    }
    if prev.is_some_and(|p| p.kind == TokenKind::Word && is_phrase(&p.text(src).to_ascii_uppercase(), &upper)) {
        return true;
    }
    if LEAD_ANCHORS.contains(&upper.as_str())
        && next.is_some_and(|n| n.kind == TokenKind::Word && is_phrase(&upper, &n.text(src).to_ascii_uppercase()))
    {
        return true;
    }
    // Window and aggregate modifiers right after a call: count(*) FILTER (...) OVER (...), WITHIN GROUP (...).
    if AFTER_CALL.contains(&upper.as_str()) && prev.is_some_and(|p| p.is_punct(src, ')')) {
        return true;
    }
    if before_paren {
        return false;
    }
    if is_statement_word(&upper) && starts_statement(src, prev) {
        return true;
    }
    is_soft_clause(&upper) && soft_clause_position(d, src, prev, next)
}

/// True when a token after `prev` starts a statement.
pub fn starts_statement(src: &str, prev: Option<&Token>) -> bool {
    match prev {
        None => true,
        Some(p) => {
            matches!(p.kind, TokenKind::SlashLine | TokenKind::BatchSeparator | TokenKind::ClientCommand)
                || p.is_punct(src, ';')
                || (p.kind == TokenKind::Word
                    && matches!(
                        p.text(src).to_ascii_uppercase().as_str(),
                        "BEGIN" | "THEN" | "ELSE" | "LOOP" | "AS" | "IS" | "DO"
                    ))
        }
    }
}

/// A soft clause word is a clause when a value ends before it and no identifier use follows it.
pub fn soft_clause_position(d: Dialect, src: &str, prev: Option<&Token>, next: Option<&Token>) -> bool {
    let after_value = prev.is_some_and(|p| ends_value(d, src, p));
    let ident_use = next.is_none_or(|n| {
        matches!(n.kind, TokenKind::Operator) && n.text(src) != "-"
            || (n.kind == TokenKind::Punct && matches!(n.text(src), "," | ")" | ";" | "."))
    });
    after_value && !ident_use
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::lexer::significant;

    fn recased(d: Dialect, s: &str) -> Vec<String> {
        let sig = significant(d, s);
        (0..sig.len())
            .filter(|&i| is_keyword_at(d, s, &sig, i))
            .map(|i| sig[i].text(s).to_owned())
            .collect()
    }

    #[test]
    fn only_keywords_in_keyword_positions() {
        assert_eq!(
            recased(
                Dialect::ClickHouse,
                "select first, offset, t.from, left(x, 1) from t limit 5 offset 2"
            ),
            ["select", "from", "limit", "offset"]
        );
        assert_eq!(
            recased(Dialect::Postgres, "create table t (id int primary key) ; begin; commit"),
            ["create", "table", "primary", "key", "begin", "commit"]
        );
        assert_eq!(
            recased(Dialect::MySql, "insert into t values (1) on duplicate key update a = values(a)"),
            ["insert", "into", "values", "on", "duplicate", "key", "update", "values"]
        );
        assert_eq!(
            recased(Dialect::Postgres, "select a from t order by a nulls first"),
            ["select", "from", "order", "by", "nulls", "first"]
        );
    }
}
