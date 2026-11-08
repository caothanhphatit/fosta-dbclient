//! Splits a script into statements and finds the statement at a cursor.
//!
//! `;` ends a statement except inside strings, comments and quoted names, and except inside bodies that hold their own
//! `;`:
//! - PostgreSQL `CREATE FUNCTION/PROCEDURE ... BEGIN ATOMIC ... END`;
//! - SQLite `CREATE TRIGGER ... BEGIN ... END`;
//! - MySQL/MariaDB `CREATE PROCEDURE/FUNCTION/TRIGGER/EVENT ... BEGIN ... END` and MariaDB `BEGIN NOT ATOMIC`;
//!   a `DELIMITER x` line switches the terminator to `x` until the next `DELIMITER` line;
//! - SQL Server `BEGIN ... END` blocks (not `BEGIN TRAN`); `CREATE/ALTER PROCEDURE|FUNCTION|TRIGGER|VIEW` runs to
//!   the next `GO` line, which also ends any statement;
//! - Oracle PL/SQL (`DECLARE`, `BEGIN`, `CREATE PROCEDURE/FUNCTION/PACKAGE/TRIGGER/TYPE`) runs to a line holding only
//!   `/`, or to the end of the script (the SQL*Plus convention).
//!
//! Offsets are byte offsets into the source.

use serde::Serialize;

use super::{
    lexer::{tokenize, Token, TokenKind},
    Dialect,
};

/// One statement: its text without the terminating `;` (PL/SQL keeps its `END;`) and its byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    /// Plain statement: the terminator ends it.
    Plain,
    /// A body with its own `;`: count BEGIN/CASE against END.
    Counted,
    /// Oracle PL/SQL: only a `/` line (or the end) ends it.
    PlSql,
    /// SQL Server module definition: only `GO` (or the end) ends it.
    Batch,
}

/// Splits a script into statements. Empty statements (`;;`, comment-only parts) are dropped.
pub fn split(dialect: Dialect, src: &str) -> Vec<Statement> {
    split_ranges(dialect, src)
        .into_iter()
        .map(|(start, end)| Statement {
            text: src[start..end].to_owned(),
            start,
            end,
        })
        .collect()
}

/// The statement under `offset` (a byte offset, clamped to the text). When the cursor sits between statements it
/// picks the statement ending on the cursor's line, else one starting on it, else the nearest by lines (the previous
/// one on a tie). `None` when the script holds no statement.
pub fn statement_at(dialect: Dialect, src: &str, offset: usize) -> Option<Statement> {
    let mut offset = offset.min(src.len());
    while !src.is_char_boundary(offset) {
        offset -= 1;
    }
    let ranges = split_ranges(dialect, src);
    let pick = pick_range(src, &ranges, offset)?;
    let (start, end) = ranges[pick];
    Some(Statement {
        text: src[start..end].to_owned(),
        start,
        end,
    })
}

fn pick_range(src: &str, ranges: &[(usize, usize)], offset: usize) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    let term_end = |end: usize| {
        let rest = &src[end..];
        let skipped = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if rest[skipped..].starts_with(';') {
            end + skipped + 1
        } else {
            end
        }
    };
    if let Some(i) = ranges.iter().position(|&(s, e)| s <= offset && offset <= term_end(e)) {
        return Some(i);
    }
    let lines = |a: usize, b: usize| src[a.min(b)..a.max(b)].bytes().filter(|&c| c == b'\n').count();
    let prev = ranges.iter().rposition(|&(_, e)| e <= offset);
    let next = ranges.iter().position(|&(s, _)| s > offset);
    match (prev, next) {
        (Some(p), Some(n)) => {
            let dp = lines(term_end(ranges[p].1), offset);
            let dn = lines(offset, ranges[n].0);
            if dp == 0 || dp <= dn {
                Some(p)
            } else {
                Some(n)
            }
        }
        (p, n) => p.or(n),
    }
}

/// Byte ranges of the statements of `src`.
pub fn split_ranges(dialect: Dialect, src: &str) -> Vec<(usize, usize)> {
    let tokens: Vec<Token> = tokenize(dialect, src).into_iter().filter(|t| !t.is_trivia()).collect();
    let mut out = Vec::new();
    let mut stmt_start: Option<usize> = None; // index into tokens
    let mut block = Block::Plain;
    let mut depth: i32 = 0;
    let mut delimiter: Option<String> = None;
    let finish = |out: &mut Vec<(usize, usize)>, from: usize, end_byte: usize| {
        let start = tokens[from].start;
        if end_byte > start {
            out.push((start, end_byte));
        }
    };
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        match t.kind {
            TokenKind::SlashLine | TokenKind::BatchSeparator => {
                if let Some(s) = stmt_start.take() {
                    finish(&mut out, s, tokens[i - 1].end);
                }
                i += 1;
                continue;
            }
            TokenKind::ClientCommand => {
                if let Some(s) = stmt_start.take() {
                    finish(&mut out, s, tokens[i - 1].end);
                }
                let text = t.text(src);
                let delim = text[9..].trim();
                delimiter = (!delim.is_empty() && delim != ";").then(|| delim.to_owned());
                i += 1;
                continue;
            }
            _ => {}
        }
        // A custom MySQL delimiter ends a statement wherever it appears outside strings and comments.
        if let Some(delim) = delimiter.as_deref() {
            let found = if !matches!(t.kind, TokenKind::Word | TokenKind::Operator | TokenKind::Punct) {
                None
            } else if src[t.start..].starts_with(delim) {
                Some(0)
            } else {
                t.text(src).find(delim)
            };
            if let Some(pos) = found {
                let cut = t.start + pos;
                if let Some(s) = stmt_start.take() {
                    if pos > 0 {
                        finish(&mut out, s, cut);
                    } else if s < i {
                        finish(&mut out, s, tokens[i - 1].end);
                    }
                }
                // Skip the tokens the delimiter covers.
                let delim_end = cut + delim.len();
                while i < tokens.len() && tokens[i].end <= delim_end {
                    i += 1;
                }
                continue;
            }
            if stmt_start.is_none() {
                stmt_start = Some(i);
            }
            i += 1;
            continue;
        }
        if stmt_start.is_none() {
            if t.is_punct(src, ';') {
                i += 1;
                continue;
            }
            stmt_start = Some(i);
            block = detect_block(dialect, src, &tokens[i..]);
            depth = 0;
        }
        let is_semi = t.is_punct(src, ';');
        match block {
            Block::PlSql | Block::Batch => {}
            Block::Counted => {
                if t.kind == TokenKind::Word {
                    depth += depth_change(dialect, src, &tokens, i, depth);
                }
                if is_semi && depth <= 0 {
                    finish(&mut out, stmt_start.take().unwrap_or(i), tokens[i - 1].end);
                }
            }
            Block::Plain => {
                if is_semi {
                    let s = stmt_start.take().unwrap_or(i);
                    if s < i {
                        finish(&mut out, s, tokens[i - 1].end);
                    }
                }
            }
        }
        i += 1;
    }
    if let Some(s) = stmt_start {
        let last = tokens.len() - 1;
        // A plain statement's trailing `;` at the very end was consumed above; PL/SQL keeps its own.
        let end = if tokens[last].is_punct(src, ';') && !matches!(block, Block::PlSql | Block::Batch) && last > s {
            tokens[last - 1].end
        } else {
            tokens[last].end
        };
        finish(&mut out, s, end);
    }
    out
}

const MSSQL_BEGIN_WITHOUT_END: [&str; 5] = ["TRAN", "TRANSACTION", "DISTRIBUTED", "DIALOG", "CONVERSATION"];

/// How a word changes the BEGIN/END depth of a counted body.
fn depth_change(dialect: Dialect, src: &str, tokens: &[Token], i: usize, depth: i32) -> i32 {
    let t = tokens[i];
    let next = tokens.get(i + 1);
    let next_word = |w: &str| next.is_some_and(|n| n.is_word(src, w));
    let word = t.text(src).to_ascii_uppercase();
    match word.as_str() {
        "BEGIN" => match dialect {
            Dialect::Postgres => i32::from(next_word("ATOMIC")),
            Dialect::MsSql => {
                let opens = !next.is_some_and(|n| {
                    n.kind == TokenKind::Word && MSSQL_BEGIN_WITHOUT_END.contains(&n.text(src).to_ascii_uppercase().as_str())
                }) && !next.is_some_and(|n| n.is_punct(src, ';'));
                i32::from(opens)
            }
            d if d.is_mysql() => i32::from(!next_word("WORK") && !next.is_some_and(|n| n.is_punct(src, ';'))),
            _ => 1,
        },
        "CASE" if depth > 0 => {
            // MySQL `END CASE` closes a CASE statement that was counted when it opened.
            let prev_is_end = i > 0 && tokens[i - 1].is_word(src, "END");
            i32::from(!prev_is_end)
        }
        "END" if depth > 0 => {
            if dialect.is_mysql() && ["IF", "LOOP", "WHILE", "REPEAT"].iter().any(|w| next_word(w)) {
                0
            } else {
                -1
            }
        }
        _ => 0,
    }
}

fn detect_block(dialect: Dialect, src: &str, toks: &[Token]) -> Block {
    let words: Vec<String> = toks
        .iter()
        .take(16)
        .take_while(|t| !t.is_punct(src, '(') && !t.is_punct(src, ';'))
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(src).to_ascii_uppercase())
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    let first = w.first().copied();
    let creates = |kinds: &[&str], within: usize| first == Some("CREATE") && w.iter().skip(1).take(within).any(|x| kinds.contains(x));
    match dialect {
        Dialect::Oracle => {
            if matches!(first, Some("DECLARE" | "BEGIN")) {
                return Block::PlSql;
            }
            if first == Some("CREATE") {
                let rest: Vec<&str> = w[1..]
                    .iter()
                    .copied()
                    .filter(|x| !matches!(*x, "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE"))
                    .collect();
                if matches!(
                    rest.first(),
                    Some(&"PROCEDURE" | &"FUNCTION" | &"PACKAGE" | &"TRIGGER" | &"TYPE" | &"LIBRARY")
                ) {
                    return Block::PlSql;
                }
            }
            Block::Plain
        }
        Dialect::Sqlite if creates(&["TRIGGER"], 3) => Block::Counted,
        Dialect::Postgres if creates(&["FUNCTION", "PROCEDURE"], 3) => Block::Counted,
        Dialect::MySql | Dialect::MariaDb => {
            if creates(&["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"], 8)
                || (first == Some("BEGIN") && w.get(1) == Some(&"NOT") && w.get(2) == Some(&"ATOMIC"))
            {
                Block::Counted
            } else {
                Block::Plain
            }
        }
        Dialect::MsSql => {
            let module = ["PROC", "PROCEDURE", "FUNCTION", "TRIGGER", "VIEW"];
            let defines = match first {
                Some("CREATE") => {
                    let rest: Vec<&str> = w[1..].iter().copied().filter(|x| !matches!(*x, "OR" | "ALTER")).collect();
                    rest.first().is_some_and(|x| module.contains(x))
                }
                Some("ALTER") => w.get(1).is_some_and(|x| module.contains(x)),
                _ => false,
            };
            if defines {
                Block::Batch
            } else {
                Block::Counted
            }
        }
        _ => Block::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(d: Dialect, s: &str) -> Vec<String> {
        split(d, s).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn splits_on_semicolons_outside_quotes_and_comments() {
        let s = "select 'a;b', \"c;d\" from t; -- x;y\n/* p;q */ select 2;;";
        assert_eq!(texts(Dialect::Postgres, s), vec!["select 'a;b', \"c;d\" from t", "select 2"]);
        assert_eq!(texts(Dialect::Generic, ";;  ;"), Vec::<String>::new());
    }

    #[test]
    fn postgres_bodies() {
        let s = "create function f() returns int as $body$ select 1; $body$ language sql; /* a /* b; */ c; */ select $1;";
        let v = texts(Dialect::Postgres, s);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].ends_with("language sql"));
        assert_eq!(v[1], "select $1");
        let s = "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN true THEN 1 END; SELECT 2; END; SELECT 3";
        let v = texts(Dialect::Postgres, s);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].ends_with("END"));
        assert_eq!(texts(Dialect::Postgres, "BEGIN; SELECT 1; COMMIT").len(), 3);
    }

    #[test]
    fn sqlite_trigger_body() {
        let s =
            "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = CASE WHEN 1 THEN 2 END; DELETE FROM c; END; select [x;y] from `t;`";
        let v = texts(Dialect::Sqlite, s);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].ends_with("END"));
        assert_eq!(v[1], "select [x;y] from `t;`");
    }

    #[test]
    fn mysql_routines_and_delimiters() {
        let s = "CREATE DEFINER=`root`@`%` PROCEDURE p() BEGIN IF 1 THEN SELECT 1; END IF; WHILE 0 DO SELECT 2; END WHILE; END; SELECT 3;";
        let v = texts(Dialect::MySql, s);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].ends_with("END WHILE; END"));
        let s = "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND$$\nSELECT 2$$\nDELIMITER ;\nSELECT 3;";
        let v = texts(Dialect::MariaDb, s);
        assert_eq!(v, vec!["CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND", "SELECT 2", "SELECT 3"]);
        assert_eq!(texts(Dialect::MySql, "BEGIN; SELECT 1; COMMIT;").len(), 3);
        let v = texts(Dialect::MariaDb, "BEGIN NOT ATOMIC SELECT 1; SELECT 2; END; SELECT 3");
        assert_eq!(v.len(), 2, "{v:?}");
    }

    #[test]
    fn mssql_batches_and_blocks() {
        let s = "CREATE PROCEDURE p AS\nSELECT 1;\nSELECT 2;\nGO\nIF @x = 1 BEGIN SELECT 1; END ELSE BEGIN SELECT 2; END;\nBEGIN TRAN; UPDATE t SET a = [b;c];\nGO 2\nselect 4";
        let v = texts(Dialect::MsSql, s);
        assert_eq!(v.len(), 5, "{v:?}");
        assert!(v[0].starts_with("CREATE PROCEDURE") && v[0].ends_with("SELECT 2;"));
        assert!(v[1].starts_with("IF") && v[1].ends_with("END"));
        assert_eq!(v[2], "BEGIN TRAN");
        assert_eq!(v[3], "UPDATE t SET a = [b;c]");
        assert_eq!(v[4], "select 4");
        let v = texts(
            Dialect::MsSql,
            "BEGIN TRY SELECT 1/0; END TRY BEGIN CATCH SELECT 2; END CATCH; SELECT 3",
        );
        assert_eq!(v.len(), 2, "{v:?}");
    }

    #[test]
    fn clickhouse_backslash_escapes() {
        let v = texts(Dialect::ClickHouse, r"SELECT 'a\';b'; SELECT `x;y` FROM t");
        assert_eq!(v.len(), 2, "{v:?}");
    }

    #[test]
    fn oracle_plsql_blocks_and_slash_lines() {
        let s = "select 1 from dual;\nBEGIN\n  null;\n  dbms_output.put_line('a;b');\nEND;\n/\nCREATE OR REPLACE PROCEDURE p IS\nBEGIN\n  NULL;\nEND p;\n/\nselect q'[it's;]' from dual\n/\nselect 4 / 2 from dual";
        let v = texts(Dialect::Oracle, s);
        assert_eq!(v.len(), 5, "{v:?}");
        assert_eq!(v[0], "select 1 from dual");
        assert!(v[1].starts_with("BEGIN") && v[1].ends_with("END;"));
        assert!(v[2].ends_with("END p;"));
        assert_eq!(v[3], "select q'[it's;]' from dual");
        assert_eq!(v[4], "select 4 / 2 from dual");
    }

    #[test]
    fn statement_at_cursor() {
        let s = "select 1;\nselect 2; select 3;\n\n\nselect 4";
        let at = |o: usize| statement_at(Dialect::Generic, s, o).map(|st| st.text);
        assert_eq!(at(0).as_deref(), Some("select 1"));
        assert_eq!(at(9).as_deref(), Some("select 1")); // right after `;`
        assert_eq!(at(10).as_deref(), Some("select 2"));
        assert_eq!(at(19).as_deref(), Some("select 2")); // after `;` before space
        assert_eq!(at(21).as_deref(), Some("select 3"));
        assert_eq!(at(30).as_deref(), Some("select 3")); // the blank line just below
        assert_eq!(at(31).as_deref(), Some("select 4")); // the blank line just above
        assert_eq!(at(10_000).as_deref(), Some("select 4"));
        assert_eq!(statement_at(Dialect::Generic, "  -- only a comment\n", 3), None);
        // Multi-byte text and offsets inside a character.
        let s = "select 'é'; select 2";
        assert_eq!(statement_at(Dialect::Postgres, s, 9).unwrap().text, "select 'é'");
    }

    #[test]
    fn statement_at_in_dialect_bodies() {
        let s = "select 1;\nCREATE TRIGGER t AFTER INSERT ON a BEGIN\n  DELETE FROM c;\nEND;\nselect 2;";
        let pos = s.find("DELETE").unwrap();
        let st = statement_at(Dialect::Sqlite, s, pos).unwrap();
        assert!(st.text.starts_with("CREATE TRIGGER") && st.text.ends_with("END"));
        let s = "SELECT 1\nGO\nSELECT 2\nGO";
        assert_eq!(statement_at(Dialect::MsSql, s, s.len()).unwrap().text, "SELECT 2");
    }
}
