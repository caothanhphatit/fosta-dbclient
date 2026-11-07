//! A small SQL lexer that knows each dialect's quoting and comments, and splits a script into statements: `;` ends a statement
//! except inside strings, comments, dollar quotes, PostgreSQL `BEGIN ATOMIC` bodies, SQLite trigger bodies and Oracle PL/SQL
//! blocks (which end at a line holding only `/`, the SQL*Plus convention, or at the end of the script).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Dialect {
    Postgres,
    Sqlite,
    ClickHouse,
    Oracle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Unquoted identifier or keyword.
    Word,
    QuotedIdent,
    Str,
    Number,
    Punct(char),
    /// Oracle only: a line holding just `/`.
    SlashLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
    /// True when this is the unquoted keyword `kw` (case-insensitive).
    pub fn is_word(&self, src: &str, kw: &str) -> bool {
        self.kind == TokenKind::Word && self.text(src).eq_ignore_ascii_case(kw)
    }
}

/// Tokenizes `src`, skipping whitespace and comments. Unterminated strings and comments run to the end of the input.
pub fn tokenize(dialect: Dialect, src: &str) -> Vec<Token> {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        // Whitespace
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Line comment
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            i = memchr_newline(b, i);
            continue;
        }
        if c == b'#' && dialect == Dialect::ClickHouse && matches!(b.get(i + 1), Some(b' ') | Some(b'!')) {
            i = memchr_newline(b, i);
            continue;
        }
        // Block comment (nested in PostgreSQL)
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let nested = dialect == Dialect::Postgres;
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else if nested && b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Oracle: a line holding only '/'
        if c == b'/' && dialect == Dialect::Oracle && line_is_only_slash(b, i) {
            out.push(Token {
                kind: TokenKind::SlashLine,
                start: i,
                end: i + 1,
            });
            i += 1;
            continue;
        }
        let start = i;
        // Oracle alternative quoting q'[...]'
        if dialect == Dialect::Oracle && (c == b'q' || c == b'Q') && b.get(i + 1) == Some(&b'\'') && i + 2 < n && !prev_is_ident(b, i) {
            let open = b[i + 2];
            let close = match open {
                b'[' => b']',
                b'{' => b'}',
                b'(' => b')',
                b'<' => b'>',
                other => other,
            };
            i += 3;
            while i < n && !(b[i] == close && b.get(i + 1) == Some(&b'\'')) {
                i += 1;
            }
            i = (i + 2).min(n);
            out.push(Token {
                kind: TokenKind::Str,
                start,
                end: i,
            });
            continue;
        }
        if c == b'\'' {
            let backslash = dialect == Dialect::ClickHouse
                || (dialect == Dialect::Postgres
                    && start > 0
                    && (b[start - 1] == b'E' || b[start - 1] == b'e')
                    && !prev_is_ident(b, start - 1));
            i = skip_quoted(b, i + 1, b'\'', backslash);
            out.push(Token {
                kind: TokenKind::Str,
                start,
                end: i,
            });
            continue;
        }
        if c == b'"' || (c == b'`' && matches!(dialect, Dialect::ClickHouse | Dialect::Sqlite)) {
            i = skip_quoted(b, i + 1, c, dialect == Dialect::ClickHouse);
            out.push(Token {
                kind: TokenKind::QuotedIdent,
                start,
                end: i,
            });
            continue;
        }
        if c == b'[' && dialect == Dialect::Sqlite {
            while i < n && b[i] != b']' {
                i += 1;
            }
            i = (i + 1).min(n);
            out.push(Token {
                kind: TokenKind::QuotedIdent,
                start,
                end: i,
            });
            continue;
        }
        if c == b'$' && dialect == Dialect::Postgres && !prev_is_ident(b, i) {
            if let Some(tag_end) = dollar_tag_end(b, i) {
                let tag = &b[i..=tag_end];
                let mut j = tag_end + 1;
                let mut end = n;
                while j + tag.len() <= n {
                    if &b[j..j + tag.len()] == tag {
                        end = j + tag.len();
                        break;
                    }
                    j += 1;
                }
                i = end;
                out.push(Token {
                    kind: TokenKind::Str,
                    start,
                    end: i,
                });
                continue;
            }
        }
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < n
                && (b[i].is_ascii_alphanumeric()
                    || b[i] == b'_'
                    || b[i] == b'$'
                    || b[i] >= 0x80
                    || (b[i] == b'#' && dialect == Dialect::Oracle))
            {
                i += 1;
            }
            out.push(Token {
                kind: TokenKind::Word,
                start,
                end: i,
            });
            continue;
        }
        if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            out.push(Token {
                kind: TokenKind::Number,
                start,
                end: i,
            });
            continue;
        }
        // Multi-byte UTF-8 punctuation never reaches here (>= 0x80 is a word char), so this is one ASCII char.
        out.push(Token {
            kind: TokenKind::Punct(c as char),
            start,
            end: i + 1,
        });
        i += 1;
    }
    out
}

fn memchr_newline(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i] != b'\n' {
        i += 1;
    }
    i
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'$')
}

fn skip_quoted(b: &[u8], mut i: usize, quote: u8, backslash: bool) -> usize {
    while i < b.len() {
        if backslash && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// `$tag$` at `i`: returns the index of the closing `$` of the opening tag.
fn dollar_tag_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j < b.len() && b[j].is_ascii_digit() {
        return None; // $1 parameter
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] >= 0x80) {
        j += 1;
    }
    (j < b.len() && b[j] == b'$').then_some(j)
}

fn line_is_only_slash(b: &[u8], i: usize) -> bool {
    let mut s = i;
    while s > 0 && b[s - 1] != b'\n' {
        s -= 1;
        if !b[s].is_ascii_whitespace() {
            return false;
        }
    }
    let mut e = i + 1;
    while e < b.len() && b[e] != b'\n' {
        if !b[e].is_ascii_whitespace() {
            return false;
        }
        e += 1;
    }
    true
}

/// One statement of a script: its text without the terminating `;` (kept for PL/SQL blocks, which need it) and its byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    /// Plain statement: `;` ends it.
    None,
    /// SQLite trigger or PostgreSQL BEGIN ATOMIC body: count BEGIN/CASE vs END.
    Counted,
    /// Oracle PL/SQL: only a `/` line (or the end) ends it.
    PlSql,
}

/// Splits a script into statements. Empty statements (`;;`, comment-only parts) are dropped.
pub fn split(dialect: Dialect, src: &str) -> Vec<Statement> {
    let tokens = tokenize(dialect, src);
    let mut out = Vec::new();
    let mut stmt_start: Option<usize> = None; // index into tokens
    let mut block = Block::None;
    let mut depth: i32 = 0;
    let mut i = 0;
    let finish = |out: &mut Vec<Statement>, from: usize, to: usize| {
        if to > from {
            let (s, e) = (tokens[from].start, tokens[to - 1].end);
            out.push(Statement {
                text: src[s..e].to_owned(),
                start: s,
                end: e,
            });
        }
    };
    while i < tokens.len() {
        let t = tokens[i];
        if t.kind == TokenKind::SlashLine {
            if let Some(s) = stmt_start.take() {
                finish(&mut out, s, i);
            }
            block = Block::None;
            depth = 0;
            i += 1;
            continue;
        }
        if stmt_start.is_none() {
            if t.kind == TokenKind::Punct(';') {
                i += 1;
                continue;
            }
            stmt_start = Some(i);
            block = detect_block(dialect, src, &tokens[i..]);
            depth = 0;
        }
        match block {
            Block::PlSql => {}
            Block::Counted => {
                if t.kind == TokenKind::Word {
                    let w = t.text(src);
                    let next_is_atomic = tokens.get(i + 1).is_some_and(|n| n.is_word(src, "ATOMIC"));
                    if (w.eq_ignore_ascii_case("BEGIN") && (dialect == Dialect::Sqlite || next_is_atomic))
                        || (w.eq_ignore_ascii_case("CASE") && depth > 0)
                    {
                        depth += 1;
                    } else if w.eq_ignore_ascii_case("END") && depth > 0 {
                        depth -= 1;
                    }
                }
                if t.kind == TokenKind::Punct(';') && depth == 0 {
                    finish(&mut out, stmt_start.take().unwrap_or(i), i);
                }
            }
            Block::None => {
                if t.kind == TokenKind::Punct(';') {
                    finish(&mut out, stmt_start.take().unwrap_or(i), i);
                }
            }
        }
        i += 1;
    }
    if let Some(s) = stmt_start {
        // A trailing `;` of a plain statement at the very end was consumed above; PL/SQL keeps its own.
        finish(&mut out, s, tokens.len());
    }
    out
}

fn detect_block(dialect: Dialect, src: &str, toks: &[Token]) -> Block {
    let words: Vec<String> = toks
        .iter()
        .take(8)
        .take_while(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(src).to_ascii_uppercase())
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match dialect {
        Dialect::Oracle => {
            if matches!(w.first(), Some(&"DECLARE") | Some(&"BEGIN")) {
                return Block::PlSql;
            }
            if w.first() == Some(&"CREATE") {
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
            Block::None
        }
        Dialect::Sqlite => {
            let is_trigger = w.first() == Some(&"CREATE") && w[1..].iter().take(2).any(|x| *x == "TRIGGER");
            if is_trigger {
                Block::Counted
            } else {
                Block::None
            }
        }
        Dialect::Postgres => {
            let is_routine = w.first() == Some(&"CREATE") && w[1..].iter().take(3).any(|x| *x == "FUNCTION" || *x == "PROCEDURE");
            if is_routine {
                Block::Counted
            } else {
                Block::None
            }
        }
        Dialect::ClickHouse => Block::None,
    }
}

/// The unquoted words of a statement, upper-cased (strings, quoted names and comments skipped).
pub fn words(dialect: Dialect, src: &str) -> Vec<String> {
    tokenize(dialect, src)
        .iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(src).to_ascii_uppercase())
        .collect()
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
    }

    #[test]
    fn postgres_dollar_quotes_and_nested_comments() {
        let s = "create function f() returns int as $body$ select 1; $body$ language sql; /* a /* b; */ c; */ select $1;";
        let v = texts(Dialect::Postgres, s);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].ends_with("language sql"));
        assert_eq!(v[1], "select $1");
        let e = texts(Dialect::Postgres, r"select E'it\'s;'; select 2");
        assert_eq!(e.len(), 2, "{e:?}");
    }

    #[test]
    fn postgres_begin_atomic_body() {
        let s = "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN true THEN 1 END; SELECT 2; END; SELECT 3";
        let v = texts(Dialect::Postgres, s);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].ends_with("END"));
        // A plain BEGIN is a statement of its own.
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
    fn words_skip_strings_and_comments() {
        assert_eq!(
            words(Dialect::Postgres, "select 'delete' -- update\n from \"insert\""),
            vec!["SELECT", "FROM"]
        );
    }
}
