//! A lossless SQL lexer: every byte of the input belongs to exactly one token, whitespace and comments included, so
//! the formatter can rebuild the text and the verifier can compare two texts token by token.
//!
//! Dialect rules it knows:
//! - comments: `--` (MySQL/MariaDB only when followed by whitespace), `#` (MySQL always, ClickHouse before a space
//!   or `!`), `/* */` (nested in PostgreSQL and SQL Server);
//! - strings: `'...'` with doubled quotes; backslash escapes in MySQL, ClickHouse and PostgreSQL `E'...'`; prefixes
//!   `N'` `X'` `B'`, MySQL `_charset'`, PostgreSQL `U&'`, Oracle `q'[...]'`; dollar quotes in PostgreSQL and
//!   ClickHouse; `"..."` is a string in MySQL;
//! - quoted names: `"..."`, backticks (MySQL, SQLite, ClickHouse), `[...]` (SQL Server, SQLite);
//! - parameters and variables: `?`, `?1`, `:name`, `$1`, SQLite `@name`/`$name`, ClickHouse `{name:Type}`,
//!   SQL Server and MySQL `@var`/`@@var`;
//! - script lines: SQL Server `GO [n]`, Oracle `/` alone on a line, MySQL `DELIMITER x`.

use serde::Serialize;

use super::Dialect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    /// Unquoted identifier or keyword.
    Word,
    QuotedIdent,
    Str,
    Number,
    /// A run of operator characters (`>=`, `::`, `||`, `@>`...). A trailing `+`/`-` is split off unless the run holds
    /// one of `~!@#%^&|` `` ` `` `?` (PostgreSQL's rule), so `=-1` lexes as `=` `-` `1`.
    Operator,
    /// One of `( ) [ ] { } , ; .`.
    Punct,
    Param,
    Variable,
    /// Oracle: a line holding only `/` (SQL*Plus runs the buffer).
    SlashLine,
    /// SQL Server: a line holding only `GO` or `GO <count>`.
    BatchSeparator,
    /// MySQL/MariaDB: a `DELIMITER x` line.
    ClientCommand,
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

    /// True for whitespace and comments.
    pub fn is_trivia(&self) -> bool {
        matches!(self.kind, TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment)
    }

    pub fn is_comment(&self) -> bool {
        matches!(self.kind, TokenKind::LineComment | TokenKind::BlockComment)
    }

    /// True when this is the unquoted word `kw` (ASCII case-insensitive).
    pub fn is_word(&self, src: &str, kw: &str) -> bool {
        self.kind == TokenKind::Word && self.text(src).eq_ignore_ascii_case(kw)
    }

    pub fn is_punct(&self, src: &str, c: char) -> bool {
        self.kind == TokenKind::Punct && self.text(src).starts_with(c)
    }
}

/// Tokenizes `src` without losing a byte. Unterminated strings and comments run to the end of the input.
pub fn tokenize(dialect: Dialect, src: &str) -> Vec<Token> {
    Lexer {
        d: dialect,
        b: src.as_bytes(),
        out: Vec::new(),
    }
    .run()
}

/// The tokens that are neither whitespace nor comments.
pub fn significant(dialect: Dialect, src: &str) -> Vec<Token> {
    tokenize(dialect, src).into_iter().filter(|t| !t.is_trivia()).collect()
}

struct Lexer<'a> {
    d: Dialect,
    b: &'a [u8],
    out: Vec<Token>,
}

const OPERATOR_CHARS: &[u8] = b"+-*/<>=~!@#%^&|?:$\\`";
/// PostgreSQL keeps a trailing `+`/`-` in an operator only when the operator holds one of these.
const KEEPS_TRAILING_SIGN: &[u8] = b"~!@#%^&|`?";

impl Lexer<'_> {
    fn run(mut self) -> Vec<Token> {
        let n = self.b.len();
        let mut i = 0;
        while i < n {
            let start = i;
            let (kind, end) = self.next_token(i);
            debug_assert!(end > start, "lexer made no progress at {start}");
            let end = end.max(start + 1).min(n);
            self.push(kind, start, end);
            i = end;
        }
        self.out
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.out.push(Token { kind, start, end });
    }

    fn at(&self, i: usize) -> u8 {
        self.b.get(i).copied().unwrap_or(0)
    }

    fn next_token(&mut self, i: usize) -> (TokenKind, usize) {
        let b = self.b;
        let n = b.len();
        let d = self.d;
        let c = b[i];
        if c.is_ascii_whitespace() {
            let mut j = i;
            while j < n && b[j].is_ascii_whitespace() {
                j += 1;
            }
            return (TokenKind::Whitespace, j);
        }
        if let Some(end) = self.line_comment(i) {
            return (TokenKind::LineComment, end);
        }
        if c == b'/' && self.at(i + 1) == b'*' {
            return (TokenKind::BlockComment, self.block_comment(i));
        }
        if let Some(t) = self.script_line(i) {
            return t;
        }
        // Prefixed and plain strings
        if let Some(end) = self.prefixed_string(i) {
            return (TokenKind::Str, end);
        }
        if c == b'\'' {
            let backslash = d.is_mysql() || d == Dialect::ClickHouse;
            return (TokenKind::Str, skip_quoted(b, i + 1, b'\'', backslash));
        }
        if c == b'"' {
            if d.is_mysql() {
                return (TokenKind::Str, skip_quoted(b, i + 1, b'"', true));
            }
            return (TokenKind::QuotedIdent, skip_quoted(b, i + 1, b'"', d == Dialect::ClickHouse));
        }
        if c == b'`' && matches!(d, Dialect::MySql | Dialect::MariaDb | Dialect::Sqlite | Dialect::ClickHouse) {
            return (TokenKind::QuotedIdent, skip_quoted(b, i + 1, b'`', d == Dialect::ClickHouse));
        }
        if c == b'[' && matches!(d, Dialect::MsSql | Dialect::Sqlite) {
            return (TokenKind::QuotedIdent, skip_quoted(b, i + 1, b']', false));
        }
        if c == b'$' && matches!(d, Dialect::Postgres | Dialect::ClickHouse) && !prev_is_ident(b, i) {
            if let Some(end) = dollar_quote(b, i) {
                return (TokenKind::Str, end);
            }
        }
        if let Some(t) = self.param_or_variable(i) {
            return t;
        }
        if c.is_ascii_digit() || (c == b'.' && self.at(i + 1).is_ascii_digit() && !prev_is_ident(b, i) && !prev_closes(b, i)) {
            return (TokenKind::Number, number_end(b, i));
        }
        if is_word_start(d, c) {
            return (TokenKind::Word, self.word_end(i));
        }
        if b"()[]{},;.".contains(&c) {
            return (TokenKind::Punct, i + 1);
        }
        if OPERATOR_CHARS.contains(&c) {
            return (TokenKind::Operator, self.operator_end(i));
        }
        // Anything else is one character of its own.
        (TokenKind::Operator, char_end(b, i))
    }

    fn line_comment(&self, i: usize) -> Option<usize> {
        let b = self.b;
        let c = b[i];
        let comment = match c {
            b'-' if self.at(i + 1) == b'-' => {
                // MySQL needs whitespace (or the end) after `--`.
                !self.d.is_mysql() || i + 2 >= b.len() || b[i + 2].is_ascii_whitespace() || b[i + 2].is_ascii_control()
            }
            b'#' if self.d.is_mysql() => true,
            b'#' if self.d == Dialect::ClickHouse => matches!(self.at(i + 1), b' ' | b'!'),
            _ => false,
        };
        comment.then(|| line_end(b, i))
    }

    fn block_comment(&self, i: usize) -> usize {
        let b = self.b;
        let n = b.len();
        let nested = matches!(self.d, Dialect::Postgres | Dialect::MsSql);
        let mut depth = 1;
        let mut j = i + 2;
        while j < n && depth > 0 {
            if b[j] == b'*' && self.at(j + 1) == b'/' {
                depth -= 1;
                j += 2;
            } else if nested && b[j] == b'/' && self.at(j + 1) == b'*' {
                depth += 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        j.min(n)
    }

    /// `GO` (SQL Server), `/` (Oracle) and `DELIMITER` (MySQL) lines.
    fn script_line(&self, i: usize) -> Option<(TokenKind, usize)> {
        let b = self.b;
        if !line_start_before(b, i) {
            return None;
        }
        match self.d {
            Dialect::Oracle if b[i] == b'/' && rest_of_line_blank(b, i + 1) => Some((TokenKind::SlashLine, i + 1)),
            Dialect::MsSql if b[i..].len() >= 2 && b[i..i + 2].eq_ignore_ascii_case(b"GO") => {
                let mut j = i + 2;
                if j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') && !b[j].is_ascii_digit() {
                    return None;
                }
                let mut k = j;
                while k < b.len() && matches!(b[k], b' ' | b'\t') {
                    k += 1;
                }
                if k > j && k < b.len() && b[k].is_ascii_digit() {
                    while k < b.len() && b[k].is_ascii_digit() {
                        k += 1;
                    }
                    j = k;
                } else if j < b.len() && b[j].is_ascii_digit() {
                    return None;
                }
                rest_of_line_blank(b, j).then_some((TokenKind::BatchSeparator, j))
            }
            Dialect::MySql | Dialect::MariaDb
                if b[i..].len() > 10 && b[i..i + 9].eq_ignore_ascii_case(b"DELIMITER") && matches!(b[i + 9], b' ' | b'\t') =>
            {
                let mut end = line_end(b, i);
                while end > i && b[end - 1].is_ascii_whitespace() {
                    end -= 1;
                }
                Some((TokenKind::ClientCommand, end))
            }
            _ => None,
        }
    }

    fn prefixed_string(&self, i: usize) -> Option<usize> {
        let b = self.b;
        let d = self.d;
        if prev_is_ident(b, i) {
            return None;
        }
        let c = b[i].to_ascii_uppercase();
        // Oracle alternative quoting: q'[...]', nq'{...}'
        if d == Dialect::Oracle {
            let q = if c == b'Q' && self.at(i + 1) == b'\'' {
                Some(i + 1)
            } else if c == b'N' && self.at(i + 1).eq_ignore_ascii_case(&b'q') && self.at(i + 2) == b'\'' {
                Some(i + 2)
            } else {
                None
            };
            if let Some(q) = q {
                if q + 1 < b.len() {
                    let open = b[q + 1];
                    let close = match open {
                        b'[' => b']',
                        b'{' => b'}',
                        b'(' => b')',
                        b'<' => b'>',
                        other => other,
                    };
                    let mut j = q + 2;
                    while j < b.len() && !(b[j] == close && self.at(j + 1) == b'\'') {
                        j += 1;
                    }
                    return Some((j + 2).min(b.len()));
                }
            }
        }
        // PostgreSQL U&'...' (string) is handled here; U&"..." is a quoted name and left to the operator path.
        if d == Dialect::Postgres && c == b'U' && self.at(i + 1) == b'&' && self.at(i + 2) == b'\'' {
            return Some(skip_quoted(b, i + 3, b'\'', false));
        }
        // One-letter prefixes and MySQL charset introducers.
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        if j >= b.len() || b[j] != b'\'' || j == i {
            return None;
        }
        let word = &b[i..j];
        let upper = word.to_ascii_uppercase();
        let backslash = match upper.as_slice() {
            b"N" | b"X" | b"B" => d.is_mysql() || d == Dialect::ClickHouse,
            b"E" if d == Dialect::Postgres => true,
            _ if d.is_mysql() && word[0] == b'_' && word.len() > 1 => true,
            _ => return None,
        };
        Some(skip_quoted(b, j + 1, b'\'', backslash))
    }

    fn param_or_variable(&self, i: usize) -> Option<(TokenKind, usize)> {
        let b = self.b;
        let d = self.d;
        let c = b[i];
        let ident_end = |mut j: usize| {
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$' || b[j] >= 0x80) {
                j += 1;
            }
            j
        };
        match c {
            b'?' if d != Dialect::Postgres => {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                Some((TokenKind::Param, j))
            }
            b':' if d != Dialect::MsSql
                && !d.is_mysql()
                && (self.at(i + 1).is_ascii_alphanumeric() || self.at(i + 1) == b'_')
                && (i == 0 || b[i - 1] != b':')
                && !prev_is_ident_or_number(b, i, d) =>
            {
                Some((TokenKind::Param, ident_end(i + 1)))
            }
            b'$' if (d == Dialect::Postgres || d == Dialect::Sqlite) && self.at(i + 1).is_ascii_digit() => {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                Some((TokenKind::Param, j))
            }
            b'$' | b'@' if d == Dialect::Sqlite && (self.at(i + 1).is_ascii_alphabetic() || self.at(i + 1) == b'_') => {
                Some((TokenKind::Param, ident_end(i + 1)))
            }
            b'@' if d == Dialect::MsSql || d.is_mysql() => {
                let mut j = i + 1;
                if self.at(j) == b'@' {
                    j += 1;
                }
                if d.is_mysql() && matches!(self.at(j), b'\'' | b'"' | b'`') {
                    let q = b[j];
                    return Some((TokenKind::Variable, skip_quoted(b, j + 1, q, true)));
                }
                let mut k = j;
                while k < b.len()
                    && (b[k].is_ascii_alphanumeric()
                        || matches!(b[k], b'_' | b'$')
                        || b[k] >= 0x80
                        || (matches!(b[k], b'#' | b'@') && d == Dialect::MsSql))
                {
                    k += 1;
                }
                (k > j).then_some((TokenKind::Variable, k))
            }
            b'{' if d == Dialect::ClickHouse => {
                // {name:Type}
                let mut j = i + 1;
                while j < b.len() && matches!(b[j], b' ' | b'\t') {
                    j += 1;
                }
                let name_start = j;
                j = ident_end(j);
                if j == name_start {
                    return None;
                }
                while j < b.len() && matches!(b[j], b' ' | b'\t') {
                    j += 1;
                }
                if self.at(j) != b':' {
                    return None;
                }
                let mut depth = 0;
                while j < b.len() && b[j] != b'\n' {
                    match b[j] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        b'}' if depth == 0 => return Some((TokenKind::Param, j + 1)),
                        _ => {}
                    }
                    j += 1;
                }
                None
            }
            _ => None,
        }
    }

    fn word_end(&self, i: usize) -> usize {
        let b = self.b;
        let d = self.d;
        let mut j = i;
        while j < b.len() {
            let c = b[j];
            let ok = c.is_ascii_alphanumeric()
                || c == b'_'
                || c >= 0x80
                || (c == b'$' && d != Dialect::ClickHouse)
                || (c == b'#' && matches!(d, Dialect::Oracle | Dialect::MsSql))
                || (c == b'@' && d == Dialect::MsSql);
            if !ok {
                break;
            }
            j += 1;
        }
        j
    }

    fn operator_end(&self, i: usize) -> usize {
        let b = self.b;
        let d = self.d;
        let mut j = i;
        while j < b.len() && OPERATOR_CHARS.contains(&b[j]) {
            let c = b[j];
            if j > i {
                // Stop where a comment, a parameter or a variable starts.
                if (c == b'/' && self.at(j + 1) == b'*')
                    || self.line_comment(j).is_some()
                    || (c == b'?' && d != Dialect::Postgres)
                    || (c == b'@' && (d == Dialect::MsSql || d.is_mysql() || d == Dialect::Sqlite))
                    || c == b'$'
                    || is_word_start(d, c)
                    || (c == b'`' && matches!(d, Dialect::MySql | Dialect::MariaDb | Dialect::Sqlite | Dialect::ClickHouse))
                    || (c == b':' && b[j - 1] != b':' && (self.at(j + 1).is_ascii_alphabetic() || self.at(j + 1) == b'_'))
                {
                    break;
                }
            }
            j += 1;
        }
        // Peel a trailing + or - (PostgreSQL's rule; harmless elsewhere, and keeps MySQL `--1` apart).
        let run = &b[i..j];
        if run.len() > 1 && !run.iter().any(|c| KEEPS_TRAILING_SIGN.contains(c)) {
            let mut k = j;
            while k > i + 1 && matches!(b[k - 1], b'+' | b'-') {
                k -= 1;
            }
            return k;
        }
        j
    }
}

fn is_word_start(d: Dialect, c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || (matches!(c, b'#' | b'$') && d == Dialect::MsSql)
}

fn char_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() && (b[j] & 0xC0) == 0x80 {
        j += 1;
    }
    j
}

fn line_end(b: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < b.len() && b[j] != b'\n' {
        j += 1;
    }
    // Leave a `\r` of a CRLF line ending to the whitespace token.
    if j > i && b[j - 1] == b'\r' {
        j -= 1;
    }
    j.max(i + 1)
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'$' || b[i - 1] >= 0x80)
}

fn prev_is_ident_or_number(b: &[u8], i: usize, d: Dialect) -> bool {
    // `a[1:n]` slices in PostgreSQL; Oracle and SQLite binds always follow an operator, comma or paren.
    d == Dialect::Postgres && i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b']')
}

fn prev_closes(b: &[u8], i: usize) -> bool {
    i > 0 && matches!(b[i - 1], b')' | b']' | b'"' | b'`' | b'\'')
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

/// `$tag$...$tag$` starting at `i`: the end of the closing tag, or the end of the input when it is missing.
fn dollar_quote(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j < b.len() && b[j].is_ascii_digit() {
        return None; // $1 parameter
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] >= 0x80) {
        j += 1;
    }
    if j >= b.len() || b[j] != b'$' {
        return None;
    }
    let tag = &b[i..=j];
    let mut k = j + 1;
    while k + tag.len() <= b.len() {
        if &b[k..k + tag.len()] == tag {
            return Some(k + tag.len());
        }
        k += 1;
    }
    Some(b.len())
}

fn number_end(b: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < b.len() {
        let c = b[j];
        if c.is_ascii_alphanumeric() || c == b'_' {
            j += 1;
            // Exponent sign: 1e-5, 2.5E+3 (not hex digits like 0x1e).
            if (c == b'e' || c == b'E')
                && matches!(b.get(j), Some(b'+') | Some(b'-'))
                && b.get(j + 1).is_some_and(u8::is_ascii_digit)
                && !(b[i] == b'0' && matches!(b.get(i + 1), Some(b'x') | Some(b'X')))
            {
                j += 2;
            }
        } else if c == b'.' {
            // `1..10` (PL/SQL ranges) stays apart: stop before a second dot.
            if b.get(j + 1) == Some(&b'.') {
                break;
            }
            j += 1;
        } else {
            break;
        }
    }
    j
}

/// True when only spaces or tabs precede `i` on its line.
fn line_start_before(b: &[u8], i: usize) -> bool {
    let mut s = i;
    while s > 0 && b[s - 1] != b'\n' {
        s -= 1;
        if !matches!(b[s], b' ' | b'\t' | b'\r') {
            return false;
        }
    }
    true
}

fn rest_of_line_blank(b: &[u8], mut i: usize) -> bool {
    while i < b.len() && b[i] != b'\n' {
        if !b[i].is_ascii_whitespace() {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: Dialect, s: &str) -> Vec<(TokenKind, String)> {
        significant(d, s).iter().map(|t| (t.kind, t.text(s).to_owned())).collect()
    }

    fn assert_lossless(d: Dialect, s: &str) {
        let toks = tokenize(d, s);
        let mut rebuilt = String::new();
        let mut pos = 0;
        for t in &toks {
            assert_eq!(t.start, pos, "gap before {t:?} in {s:?}");
            rebuilt.push_str(t.text(s));
            pos = t.end;
        }
        assert_eq!(rebuilt, s);
    }

    #[test]
    fn lossless_on_odd_inputs() {
        let samples = [
            "select 'unterminated",
            "/* open comment",
            "select \"a\"\"b\", `c``d`, [e]]f] from t -- tail",
            "select $$a$$, $x$b$x$, $1, E'\\'', U&'d\\0061t' from t",
            "select q'[it's]', nq'{x}', :bind from dual\n/\n",
            "SELECT 1\r\nGO\r\nselect N'é€', @@ROWCOUNT, #tmp.x",
            "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$\nDELIMITER ;",
            "select {id:UInt32}, x'1F', 1e-5, .5, 0x1e-1 # comment\n",
            "é ü 漢字 'ü' \u{a0}",
            "",
            "-",
            "a--1",
        ];
        for d in Dialect::ALL {
            for s in samples {
                assert_lossless(d, s);
            }
        }
    }

    #[test]
    fn mysql_comments_and_strings() {
        let v = kinds(Dialect::MySql, "select a--1, \"x\\\"y\" # c\n-- d\nfrom t");
        let texts: Vec<&str> = v.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["select", "a", "-", "-", "1", ",", "\"x\\\"y\"", "from", "t"]);
        assert_eq!(v[6].0, TokenKind::Str);
        assert_eq!(kinds(Dialect::MySql, "_utf8mb4'x' @v @@global.x")[0].0, TokenKind::Str);
        let vars = kinds(Dialect::MySql, "@v @@global.x");
        assert_eq!(vars[0], (TokenKind::Variable, "@v".into()));
        assert_eq!(vars[1], (TokenKind::Variable, "@@global".into()));
    }

    #[test]
    fn postgres_operators_and_dollar_quotes() {
        let v = kinds(Dialect::Postgres, "select a::int, b #> '{x}', c @> d, x=-1, $f$ ; $f$ from t");
        let texts: Vec<&str> = v.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            [
                "select",
                "a",
                "::",
                "int",
                ",",
                "b",
                "#>",
                "'{x}'",
                ",",
                "c",
                "@>",
                "d",
                ",",
                "x",
                "=",
                "-",
                "1",
                ",",
                "$f$ ; $f$",
                "from",
                "t"
            ]
        );
        // Nested comments
        assert_eq!(kinds(Dialect::Postgres, "/* a /* b */ c */ x").len(), 1);
    }

    #[test]
    fn mssql_go_lines_brackets_and_variables() {
        let s = "select [a]]b], @x, @@rowcount from #t\nGO\nselect 1\n  go 3  \nselect go from t";
        let v = kinds(Dialect::MsSql, s);
        assert!(v.contains(&(TokenKind::QuotedIdent, "[a]]b]".into())));
        assert!(v.contains(&(TokenKind::Variable, "@x".into())));
        assert!(v.contains(&(TokenKind::Variable, "@@rowcount".into())));
        assert!(v.contains(&(TokenKind::Word, "#t".into())));
        let gos: Vec<_> = v.iter().filter(|(k, _)| *k == TokenKind::BatchSeparator).collect();
        assert_eq!(gos.len(), 2, "{v:?}");
        assert_eq!(gos[1].1, "go 3");
    }

    #[test]
    fn oracle_slash_lines_and_q_quotes() {
        let v = kinds(Dialect::Oracle, "select q'[a';b]' x, 4 / 2 from dual\n /\nbegin null; end;");
        assert!(v.contains(&(TokenKind::Str, "q'[a';b]'".into())));
        assert_eq!(v.iter().filter(|(k, _)| *k == TokenKind::SlashLine).count(), 1);
        assert!(v.contains(&(TokenKind::Operator, "/".into())));
        assert!(kinds(Dialect::Oracle, "for i in 1..10 loop").contains(&(TokenKind::Number, "1".into())));
    }

    #[test]
    fn clickhouse_params_and_comments() {
        let v = kinds(Dialect::ClickHouse, "select {id:UInt32}, 'a\\'b', x # c\n#!shebang\nfrom t");
        assert_eq!(v[1], (TokenKind::Param, "{id:UInt32}".into()));
        assert_eq!(v[3].0, TokenKind::Str);
        assert_eq!(v.last().unwrap().1, "t");
        assert_eq!(v.len(), 8, "{v:?}");
    }

    #[test]
    fn numbers_keep_exponents() {
        let v = kinds(Dialect::Generic, "1e-5 + 2.5E+3 - .5");
        let texts: Vec<&str> = v.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["1e-5", "+", "2.5E+3", "-", ".5"]);
    }
}
