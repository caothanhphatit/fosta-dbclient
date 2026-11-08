//! A SQL formatter that changes nothing but whitespace and the case of keywords.
//!
//! It works on the lossless token stream of [`super::lexer`], so strings, quoted names, comments and dialect script
//! lines (`GO`, `/`, `DELIMITER`) come out byte for byte. Layout:
//! - each clause (`SELECT`, `FROM`, `WHERE`, joins, `GROUP BY`...) starts a line; a clause whose body does not fit in
//!   `max_line` breaks after each top-level comma (list clauses) or before each top-level `AND`/`OR`/`ON` (predicate
//!   and join clauses);
//! - a parenthesized group stays on one line when it fits; otherwise a subquery is laid out as a statement one level
//!   deeper and any other group puts one item per line (simple value lists are packed);
//! - `CASE` breaks before each `WHEN`/`ELSE` when it does not fit;
//! - procedural blocks (`BEGIN ... END`, PL/SQL `DECLARE`/`IS`, `IF ... THEN ... ELSE ... END IF`, loops) indent their
//!   bodies;
//! - comments stay where they were relative to the code, blank lines between statements are kept (at most one).
//!
//! Every result is verified: re-lexing it must give the same tokens as the input, up to whitespace and keyword case.
//! [`format_sql`] returns the input unchanged if that check ever fails; [`try_format`] reports it.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::{
    keywords::{ends_value, is_keyword_at, PAREN_KEYWORDS},
    lexer::{tokenize, Token, TokenKind},
    Dialect,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum KeywordCase {
    #[default]
    Upper,
    Lower,
    Preserve,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FormatOptions {
    /// Spaces per indent level (ignored when `use_tabs`).
    pub indent: usize,
    pub use_tabs: bool,
    pub keyword_case: KeywordCase,
    /// Lines longer than this are broken where the layout allows (0 = never break for length).
    pub max_line: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent: 2,
            use_tabs: false,
            keyword_case: KeywordCase::Upper,
            max_line: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The formatted text would not lex to the same tokens; carries a description of the first difference.
    Changed(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Changed(what) => write!(f, "formatting would change the statement: {what}"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Formats `text`; returns it unchanged when the verified result would differ in anything but whitespace and keyword
/// case (which should not happen).
pub fn format_sql(dialect: Dialect, text: &str, options: &FormatOptions) -> String {
    try_format(dialect, text, options).unwrap_or_else(|_| text.to_owned())
}

/// Formats `text` and verifies the result.
pub fn try_format(dialect: Dialect, text: &str, options: &FormatOptions) -> Result<String, FormatError> {
    if text.trim().is_empty() {
        return Ok(text.to_owned());
    }
    let out = Formatter::new(dialect, text, options).run();
    verify(dialect, text, &out)?;
    Ok(out)
}

/// The layout without the verification (tests use it to show what went wrong).
#[cfg(test)]
pub(super) fn format_unchecked(dialect: Dialect, text: &str, options: &FormatOptions) -> String {
    Formatter::new(dialect, text, options).run()
}

/// The token signature two texts must share: kinds and texts, without whitespace, with keywords upper-cased and line
/// comments right-trimmed.
pub fn signature(dialect: Dialect, text: &str) -> Vec<(TokenKind, String)> {
    let tokens = tokenize(dialect, text);
    let sig: Vec<Token> = tokens.iter().copied().filter(|t| !t.is_trivia()).collect();
    let mut si = 0;
    let mut out = Vec::new();
    for t in &tokens {
        match t.kind {
            TokenKind::Whitespace => {}
            TokenKind::LineComment => out.push((t.kind, t.text(text).trim_end().to_owned())),
            TokenKind::BlockComment => out.push((t.kind, t.text(text).to_owned())),
            _ => {
                let s = if is_keyword_at(dialect, text, &sig, si) {
                    t.text(text).to_ascii_uppercase()
                } else {
                    t.text(text).to_owned()
                };
                out.push((t.kind, s));
                si += 1;
            }
        }
    }
    out
}

/// Checks that `after` lexes to the same tokens as `before`, up to whitespace and keyword case.
pub fn verify(dialect: Dialect, before: &str, after: &str) -> Result<(), FormatError> {
    let a = signature(dialect, before);
    let b = signature(dialect, after);
    if a == b {
        return Ok(());
    }
    let pos = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    Err(FormatError::Changed(format!(
        "token {pos}: {:?} became {:?}",
        a.get(pos).map(|t| &t.1),
        b.get(pos).map(|t| &t.1)
    )))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    /// Breaks after top-level commas (SELECT, FROM, GROUP BY, ORDER BY, SET, VALUES, RETURNING).
    List,
    /// Breaks before top-level AND/OR (WHERE, HAVING, QUALIFY, PREWHERE, CONNECT BY).
    Predicate,
    /// Breaks before ON and top-level AND/OR.
    Join,
    /// Common table expressions: breaks after commas at the clause level.
    With,
    /// Never breaks inside (LIMIT, INSERT INTO, UPDATE...).
    Simple,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameKind {
    Begin,
    Then,
    Loop,
    Declare,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    kind: FrameKind,
    base: usize,
}

#[derive(Clone, Copy, Debug)]
struct Clause {
    style: Style,
    broken: bool,
}

struct Item {
    tok: Token,
    /// Newlines in the whitespace before this item.
    nl_before: usize,
    /// Whitespace (or a comment) directly before this item in the source.
    ws_before: bool,
    /// Index into the significant tokens (None for comments).
    sig: Option<usize>,
}

const MAX_DEPTH: usize = 200;

struct Formatter<'a> {
    d: Dialect,
    src: &'a str,
    opts: &'a FormatOptions,
    items: Vec<Item>,
    sig: Vec<Token>,
    /// Item index of each significant token.
    sig_item: Vec<usize>,
    kw: Vec<bool>,
    upper: Vec<String>,
    /// For `(`, `[`, `{` and keyword `CASE`: the item index of the matching closer.
    matching: Vec<Option<usize>>,
    out: String,
    line_len: usize,
    line_indent: usize,
    pending: Option<usize>,
    last: Option<usize>,
    /// Set after a statement ends, so a blank line before the next one is kept.
    boundary: bool,
    /// The MySQL `DELIMITER` in effect, when it is not `;`.
    delimiter: Option<String>,
}

impl<'a> Formatter<'a> {
    fn new(d: Dialect, src: &'a str, opts: &'a FormatOptions) -> Self {
        let tokens = tokenize(d, src);
        let mut items = Vec::new();
        let mut sig = Vec::new();
        let mut sig_item = Vec::new();
        let mut nl = 0;
        let mut ws = false;
        for t in tokens {
            if t.kind == TokenKind::Whitespace {
                nl += t.text(src).bytes().filter(|&c| c == b'\n').count();
                ws = true;
                continue;
            }
            let s = if t.is_comment() {
                None
            } else {
                sig.push(t);
                sig_item.push(items.len());
                Some(sig.len() - 1)
            };
            items.push(Item {
                tok: t,
                nl_before: nl,
                ws_before: ws || items.last().is_some_and(|p: &Item| p.tok.is_comment()),
                sig: s,
            });
            nl = 0;
            ws = false;
        }
        let kw: Vec<bool> = items
            .iter()
            .map(|it| it.sig.is_some_and(|s| is_keyword_at(d, src, &sig, s)))
            .collect();
        let upper: Vec<String> = items
            .iter()
            .map(|it| {
                if it.tok.kind == TokenKind::Word {
                    it.tok.text(src).to_ascii_uppercase()
                } else {
                    String::new()
                }
            })
            .collect();
        let mut f = Self {
            d,
            src,
            opts,
            items,
            sig,
            sig_item,
            kw,
            upper,
            matching: Vec::new(),
            out: String::new(),
            line_len: 0,
            line_indent: 0,
            pending: None,
            last: None,
            boundary: false,
            delimiter: None,
        };
        f.matching = f.compute_matching();
        f
    }

    fn run(mut self) -> String {
        let n = self.items.len();
        self.stmt(0, n, 0, 0);
        // Whitespace is only ever written before a token, so the output ends with the last token's own text (which
        // may end in whitespace when it is an unterminated string or comment).
        let mut out = self.out;
        let ends_inside_token = self.items.last().is_some_and(|it| it.tok.end == self.src.len());
        if self.src.ends_with('\n') && !ends_inside_token {
            out.push('\n');
        }
        out
    }

    // ----- token helpers -----

    fn text(&self, i: usize) -> &'a str {
        self.items[i].tok.text(self.src)
    }

    fn kind(&self, i: usize) -> TokenKind {
        self.items[i].tok.kind
    }

    fn is_punct(&self, i: usize, p: &str) -> bool {
        self.kind(i) == TokenKind::Punct && self.text(i) == p
    }

    /// Keyword `w` (upper case) at item `i`.
    fn is_kw(&self, i: usize, w: &str) -> bool {
        i < self.items.len() && self.kw[i] && self.upper[i] == w
    }

    fn is_word(&self, i: usize, w: &str) -> bool {
        i < self.items.len() && self.kind(i) == TokenKind::Word && self.upper[i] == w
    }

    /// The next significant item after `i`.
    fn next_sig(&self, i: usize) -> Option<usize> {
        let s = match self.items.get(i)?.sig {
            Some(s) => s + 1,
            None => self.items[i..].iter().find_map(|it| it.sig)?,
        };
        self.sig_item.get(s).copied()
    }

    /// The previous significant item before `i`.
    fn prev_sig(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&j| self.items[j].sig.is_some())
    }

    fn render(&self, i: usize) -> Cow<'a, str> {
        let text = self.text(i);
        match self.kind(i) {
            TokenKind::LineComment => Cow::Borrowed(text.trim_end()),
            TokenKind::Word if self.kw[i] => match self.opts.keyword_case {
                KeywordCase::Upper => Cow::Owned(text.to_ascii_uppercase()),
                KeywordCase::Lower => Cow::Owned(text.to_ascii_lowercase()),
                KeywordCase::Preserve => Cow::Borrowed(text),
            },
            _ => Cow::Borrowed(text),
        }
    }

    fn compute_matching(&self) -> Vec<Option<usize>> {
        let n = self.items.len();
        let mut m = vec![None; n];
        // Openers: ( [ { CASE, and BEGIN blocks (which only stop CASE from matching their END).
        let mut stack: Vec<(usize, &str)> = Vec::new();
        for i in 0..n {
            if self.items[i].sig.is_none() {
                continue;
            }
            match self.kind(i) {
                TokenKind::Punct => {
                    let t = self.text(i);
                    match t {
                        "(" | "[" | "{" => stack.push((i, t)),
                        ")" | "]" | "}" => {
                            let open = match t {
                                ")" => "(",
                                "]" => "[",
                                _ => "{",
                            };
                            if let Some(pos) = stack.iter().rposition(|(_, o)| *o == open) {
                                let (j, _) = stack[pos];
                                stack.truncate(pos);
                                m[j] = Some(i);
                            }
                        }
                        _ => {}
                    }
                }
                TokenKind::Word if self.kw[i] => match self.upper[i].as_str() {
                    "CASE" if !self.prev_sig(i).is_some_and(|p| self.is_word(p, "END")) => stack.push((i, "CASE")),
                    "BEGIN" if self.begin_opens_block(i) => stack.push((i, "BEGIN")),
                    "END" => {
                        let next = self.next_sig(i);
                        if next.is_some_and(|j| ["IF", "LOOP", "WHILE", "REPEAT"].iter().any(|w| self.is_word(j, w))) {
                            continue;
                        }
                        // Close the innermost CASE or BEGIN, never across a bracket.
                        if let Some(pos) = stack.iter().rposition(|(_, o)| matches!(*o, "CASE" | "BEGIN" | "(" | "[" | "{")) {
                            let (j, o) = stack[pos];
                            if o == "CASE" {
                                stack.truncate(pos);
                                m[j] = Some(i);
                            } else if o == "BEGIN" {
                                stack.truncate(pos);
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        m
    }

    /// True when the BEGIN at `i` opens a block rather than a transaction.
    fn begin_opens_block(&self, i: usize) -> bool {
        let Some(n) = self.next_sig(i) else {
            return false;
        };
        if self.is_punct(n, ";") {
            return false;
        }
        const TX: [&str; 10] = [
            "TRAN",
            "TRANSACTION",
            "WORK",
            "DISTRIBUTED",
            "DIALOG",
            "CONVERSATION",
            "DEFERRED",
            "IMMEDIATE",
            "EXCLUSIVE",
            "ISOLATION",
        ];
        if TX.iter().any(|w| self.is_word(n, w)) {
            return false;
        }
        match self.d {
            Dialect::Postgres => self.is_word(n, "ATOMIC"),
            Dialect::Sqlite | Dialect::MySql | Dialect::MariaDb => {
                // At the start of a script statement BEGIN is a transaction, except MariaDB's BEGIN NOT ATOMIC.
                let prev = self.prev_sig(i);
                let at_start = prev.is_none_or(|p| {
                    self.is_punct(p, ";")
                        || matches!(
                            self.kind(p),
                            TokenKind::ClientCommand | TokenKind::SlashLine | TokenKind::BatchSeparator
                        )
                });
                !at_start || self.is_word(n, "NOT")
            }
            _ => true,
        }
    }

    // ----- output -----

    fn indent_str(&self, level: usize) -> String {
        if self.opts.use_tabs {
            "\t".repeat(level)
        } else {
            " ".repeat(self.opts.indent * level)
        }
    }

    fn newline(&mut self, level: usize) {
        if self.out.is_empty() {
            return;
        }
        self.pending = Some(level);
    }

    fn emit(&mut self, i: usize) {
        let text = self.render(i);
        // A due line break waits for a later token when it would split glued tokens (`all:p`) or put a
        // line-sensitive word at the start of a line (`GO`, `/`, `DELIMITER`).
        let hold = self.pending.is_some()
            && self.last.is_some_and(|p| {
                !matches!(
                    self.kind(p),
                    TokenKind::LineComment | TokenKind::SlashLine | TokenKind::BatchSeparator | TokenKind::ClientCommand
                ) && ((p + 1 == i && !self.items[i].ws_before && !self.can_separate(p, i))
                    || self.must_not_start_line(i)
                    || (!matches!(self.kind(p), TokenKind::SlashLine | TokenKind::BatchSeparator) && self.line_is_script_like()))
            });
        // MySQL: a line holding just the word DELIMITER must not get more text, or it becomes a client command.
        if self.pending.is_none() && self.d.is_mysql() && self.current_line().trim().eq_ignore_ascii_case("DELIMITER") {
            self.pending = Some(self.line_indent);
        }
        let glue = self.glue_required();
        let held = if hold || glue { self.pending.take() } else { None };
        if glue {
            // Nothing between the previous token and this one.
        } else if let Some(level) = self.pending.take() {
            let trimmed = self.out.trim_end_matches([' ', '\t']).len();
            self.out.truncate(trimmed);
            self.out.push('\n');
            let item = &self.items[i];
            if item.nl_before >= 2 && (self.boundary || item.tok.is_comment()) {
                self.out.push('\n');
            }
            let ind = self.indent_str(level);
            self.out.push_str(&ind);
            self.line_len = self.opts.indent * level;
            self.line_indent = level;
        } else if let Some(prev) = self.last {
            if self.space_between(prev, i) {
                self.out.push(' ');
                self.line_len += 1;
            }
        }
        self.out.push_str(&text);
        if let Some(nl) = text.rfind('\n') {
            self.line_len = text[nl + 1..].chars().count();
        } else {
            self.line_len += text.chars().count();
        }
        self.last = Some(i);
        if !self.items[i].tok.is_comment() {
            self.boundary = false;
        }
        if self.kind(i) == TokenKind::LineComment {
            self.pending = Some(held.unwrap_or(self.line_indent));
        } else if held.is_some() {
            self.pending = held;
        }
    }

    /// Tokens that would lex differently at the start of a line: a word `GO` (SQL Server batch separator), a `/`
    /// operator (Oracle slash line), a word `DELIMITER` (MySQL client command).
    fn must_not_start_line(&self, i: usize) -> bool {
        match (self.d, self.kind(i)) {
            (Dialect::MsSql, TokenKind::Word) => self.upper[i] == "GO",
            (Dialect::Oracle, TokenKind::Operator) => self.text(i) == "/",
            (Dialect::MySql | Dialect::MariaDb, TokenKind::Word) => self.upper[i] == "DELIMITER",
            _ => false,
        }
    }

    /// True when no whitespace may follow the output so far: MySQL `--` and ClickHouse `#` would start a comment.
    fn glue_required(&self) -> bool {
        let Some(p) = self.last else { return false };
        if self.kind(p) != TokenKind::Operator {
            return false;
        }
        (self.d.is_mysql() && self.out.ends_with("--")) || (self.d == Dialect::ClickHouse && self.out.ends_with('#'))
    }

    fn current_line(&self) -> &str {
        &self.out[self.out.rfind('\n').map_or(0, |p| p + 1)..]
    }

    /// True when ending the current line here would leave a script line behind: Oracle `/`, SQL Server `GO [n]`.
    fn line_is_script_like(&self) -> bool {
        let line = self.current_line().trim();
        match self.d {
            Dialect::Oracle => line == "/",
            Dialect::MsSql => {
                line.len() >= 2 && line[..2].eq_ignore_ascii_case("GO") && {
                    let rest = line[2..].trim_start();
                    (rest.is_empty() || rest.bytes().all(|c| c.is_ascii_digit()))
                        && (line.len() == 2 || line.as_bytes()[2].is_ascii_whitespace())
                }
            }
            _ => false,
        }
    }

    /// Emits a comment, on its own line when it was on its own line in the source.
    fn comment(&mut self, i: usize) {
        if self.last.is_none() {
            self.emit(i);
        } else if self.items[i].nl_before == 0 {
            // A trailing comment stays on its line, even when a line break is already due after it.
            let saved = self.pending.take();
            self.emit(i);
            if saved.is_some() {
                self.pending = saved;
            }
        } else {
            let level = self.pending.unwrap_or(self.line_indent);
            self.newline(level);
            self.emit(i);
        }
        // A block comment that ended its line keeps the next token on a new line.
        if self.kind(i) == TokenKind::BlockComment && self.pending.is_none() && self.items.get(i + 1).is_some_and(|n| n.nl_before > 0) {
            let level = self.line_indent;
            self.newline(level);
        }
    }

    /// Whether a space goes between two items printed on the same line.
    fn space_between(&self, a: usize, b: usize) -> bool {
        let (ka, kb) = (self.kind(a), self.kind(b));
        if self.items[a].tok.is_comment() || self.items[b].tok.is_comment() {
            return true;
        }
        let ta = self.text(a);
        let tb = self.text(b);
        let adjacent = b == a + 1 && !self.items[b].ws_before;
        let punct_a = ka == TokenKind::Punct;
        let punct_b = kb == TokenKind::Punct;
        if punct_b && matches!(tb, "," | ";" | ")" | "]") {
            return false;
        }
        if punct_a && matches!(ta, "(" | "[") {
            return false;
        }
        if (punct_a && ta == "{") || (punct_b && matches!(tb, "{" | "}")) || (punct_b && tb == "[") {
            return !adjacent;
        }
        if punct_a && ta == "," {
            return true;
        }
        let wordish = |k: TokenKind| matches!(k, TokenKind::Word | TokenKind::QuotedIdent | TokenKind::Variable);
        if (punct_a && ta == ".") || (punct_b && tb == ".") {
            if adjacent {
                return false;
            }
            let other_ok = if punct_a { wordish(kb) || tb == "*" } else { wordish(ka) };
            return !other_ok;
        }
        if ta == "::" || tb == "::" {
            // Only glue `::` to tokens that cannot extend an operator run (`#tmp`, `:p`, `@v` could).
            let plain = |t: &str| {
                t.bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'"' | b'`' | b'[' | b'\'') || c >= 0x80)
            };
            let value = |k: TokenKind, t: &str| {
                (matches!(k, TokenKind::Word | TokenKind::QuotedIdent | TokenKind::Str | TokenKind::Number) && plain(t)) || t == ")"
            };
            let other_ok = if ta == "::" { value(kb, tb) } else { value(ka, ta) };
            return !(adjacent || other_ok);
        }
        if punct_b && tb == "(" {
            if !adjacent {
                return true;
            }
            return (ka == TokenKind::Operator && self.operator_is_binary(a))
                || (self.kw[a] && PAREN_KEYWORDS.contains(&self.upper[a].as_str()));
        }
        let op_a = ka == TokenKind::Operator;
        let op_b = kb == TokenKind::Operator;
        if op_a || op_b {
            if !adjacent {
                return true;
            }
            // ClickHouse reads `# ` as a comment, so a `#` operator never gets a space after it; a stray `$` stays
            // attached, since a space before it could open a dollar quote.
            if (self.d == Dialect::ClickHouse && op_a && ta.ends_with('#')) || (op_a && ta.contains('$')) || (op_b && tb.contains('$')) {
                return false;
            }
            // Slices and map entries keep their colon attached: arr[1:2], {'k': 1}.
            if ta == ":" || tb == ":" {
                return false;
            }
            if op_a && op_b {
                // A sign after a binary operator gets a space (`a = -1`, MySQL `a - -1`); other runs stay glued.
                return matches!(tb, "-" | "+") && self.operator_is_binary(a) && ta != "::";
            }
            // Oracle attribute and database-link markers stay attached: emp%ROWTYPE, t@link.
            if self.d == Dialect::Oracle && (matches!(ta, "%" | "@") || matches!(tb, "%" | "@")) {
                return false;
            }
            if op_b {
                return true;
            }
            return self.operator_is_binary(a);
        }
        if punct_a && matches!(ta, ")" | "]" | "}") && adjacent && punct_b {
            return false;
        }
        true
    }

    /// Whether any whitespace (a line break included) may go between two items that touch in the source.
    fn can_separate(&self, a: usize, b: usize) -> bool {
        let (ka, kb) = (self.kind(a), self.kind(b));
        let (ta, tb) = (self.text(a), self.text(b));
        let punct = |k: TokenKind, t: &str, p: &[&str]| k == TokenKind::Punct && p.contains(&t);
        if punct(kb, tb, &["("]) {
            return !matches!(ka, TokenKind::Word | TokenKind::QuotedIdent | TokenKind::Variable) || self.kw[a];
        }
        if punct(ka, ta, &["{", "[", "."]) || punct(kb, tb, &["[", "{", "}", "."]) {
            return false;
        }
        let op_a = ka == TokenKind::Operator;
        let op_b = kb == TokenKind::Operator;
        if [ta, tb].iter().any(|t| matches!(*t, "::" | ":")) {
            return false;
        }
        if (op_a && ta.contains('$')) || (op_b && tb.contains('$')) || (self.d == Dialect::ClickHouse && op_a && ta.ends_with('#')) {
            return false;
        }
        if self.d == Dialect::Oracle && (matches!(ta, "%" | "@") || matches!(tb, "%" | "@")) {
            return false;
        }
        if op_a && op_b {
            return matches!(tb, "-" | "+") && self.operator_is_binary(a);
        }
        true
    }

    /// A `*` is always spaced like a binary operator; other operators are binary after a value.
    fn operator_is_binary(&self, i: usize) -> bool {
        if self.text(i) == "*" {
            return true;
        }
        let Some(s) = self.items[i].sig else {
            return true;
        };
        s > 0 && ends_value(self.d, self.src, &self.sig[s - 1])
    }

    // ----- layout -----

    /// Width of items `from..to` printed on one line, or None when they cannot be (line comments, multi-line comments,
    /// script lines).
    fn flat_width(&self, from: usize, to: usize) -> Option<usize> {
        let mut w = 0;
        let mut prev: Option<usize> = None;
        for i in from..to {
            match self.kind(i) {
                // A trailing line comment may end a one-line range; anywhere else it forces a break.
                TokenKind::LineComment if i + 1 == to => {}
                TokenKind::LineComment | TokenKind::SlashLine | TokenKind::BatchSeparator | TokenKind::ClientCommand => return None,
                TokenKind::BlockComment if self.text(i).contains('\n') => return None,
                _ => {}
            }
            if let Some(p) = prev {
                if self.space_between(p, i) {
                    w += 1;
                }
            }
            w += self.render(i).chars().count();
            prev = Some(i);
        }
        Some(w)
    }

    fn fits(&self, width: Option<usize>, leading_space: bool) -> bool {
        match width {
            None => false,
            Some(w) => self.opts.max_line == 0 || self.line_len + usize::from(leading_space) + w <= self.opts.max_line,
        }
    }

    fn emit_flat(&mut self, from: usize, to: usize) {
        for i in from..to {
            if !self.emit_special(i) {
                self.emit(i);
            }
        }
    }

    /// Emits a comment or a script line (`GO`, `/`, `DELIMITER`) with the line breaks it needs; false for any other
    /// item.
    fn emit_special(&mut self, i: usize) -> bool {
        match self.kind(i) {
            TokenKind::LineComment | TokenKind::BlockComment => self.comment(i),
            TokenKind::SlashLine | TokenKind::BatchSeparator | TokenKind::ClientCommand => {
                let level = self.pending.unwrap_or(self.line_indent);
                self.newline(0);
                self.emit(i);
                self.newline(level);
            }
            _ => return false,
        }
        true
    }

    /// Lays out items `from..to` as statements (clauses, procedural blocks) at indent `base`.
    fn stmt(&mut self, from: usize, to: usize, base0: usize, depth: usize) {
        if depth > MAX_DEPTH {
            self.emit_flat(from, to);
            return;
        }
        let mut base = base0;
        let mut frames: Vec<Frame> = Vec::new();
        let mut at_start = true;
        let mut stmt_first: Option<String> = None;
        let mut plsql_routine = false;
        let mut clause: Option<Clause> = None;
        let mut between = false;
        let mut i = from;
        while i < to {
            let kind = self.kind(i);
            if self.items[i].tok.is_comment() {
                self.comment(i);
                i += 1;
                continue;
            }
            if kind == TokenKind::ClientCommand {
                let delim = self.text(i)[9..].trim();
                self.delimiter = (!delim.is_empty() && delim != ";").then(|| delim.to_owned());
            }
            if let Some(end) = self.custom_delimiter_at(i, to) {
                // MySQL `END$$` / `$$` under DELIMITER: the statement (and any open block) ends here.
                let t = self.items[i].tok;
                let text = self.text(i);
                if kind == TokenKind::Word
                    && text.len() > 3
                    && text[..3].eq_ignore_ascii_case("END")
                    && !self.src[t.start..].starts_with(self.delimiter.as_deref().unwrap_or_default())
                {
                    self.newline(base0);
                }
                for j in i..end {
                    self.emit(j);
                }
                frames.clear();
                base = base0;
                self.newline(base0);
                self.boundary = true;
                at_start = true;
                stmt_first = None;
                plsql_routine = false;
                clause = None;
                i = end;
                continue;
            }
            if matches!(kind, TokenKind::SlashLine | TokenKind::BatchSeparator | TokenKind::ClientCommand) {
                self.boundary = true;
                self.newline(0);
                self.emit(i);
                self.newline(0);
                self.boundary = true;
                frames.clear();
                base = base0;
                at_start = true;
                stmt_first = None;
                plsql_routine = false;
                clause = None;
                i += 1;
                continue;
            }
            if self.is_punct(i, ";") {
                self.emit(i);
                self.newline(base);
                if frames.is_empty() {
                    self.boundary = true;
                    plsql_routine = false;
                }
                at_start = true;
                stmt_first = None;
                clause = None;
                i += 1;
                continue;
            }
            if at_start && kind == TokenKind::Word && stmt_first.is_none() {
                stmt_first = Some(self.upper[i].clone());
                if frames.is_empty() && self.d == Dialect::Oracle && self.upper[i] == "CREATE" {
                    plsql_routine = self.creates_plsql_routine(i);
                }
            }
            let up = self.upper[i].clone();
            if kind == TokenKind::Word && self.kw[i] {
                // ----- procedural structure -----
                match up.as_str() {
                    "BEGIN" if self.begin_opens_block(i) => {
                        if frames.last().is_some_and(|f| f.kind == FrameKind::Declare) {
                            base = frames.pop().map_or(base0, |f| f.base);
                        }
                        self.newline(base);
                        self.emit(i);
                        let mut j = i + 1;
                        while j < to
                            && (self.is_word(j, "ATOMIC") || self.is_word(j, "TRY") || self.is_word(j, "CATCH") || self.is_word(j, "NOT"))
                        {
                            self.emit(j);
                            j += 1;
                        }
                        frames.push(Frame {
                            kind: FrameKind::Begin,
                            base,
                        });
                        base += 1;
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i = j;
                        continue;
                    }
                    "END" if !frames.is_empty() => {
                        let next = self.next_sig(i).filter(|&j| j < to);
                        let tail = next.filter(|&j| ["IF", "LOOP", "WHILE", "REPEAT", "CASE"].iter().any(|w| self.is_word(j, w)));
                        if tail.is_some() {
                            if let Some(f) = frames.pop() {
                                base = f.base;
                            }
                        } else {
                            while let Some(f) = frames.pop() {
                                base = f.base;
                                if f.kind == FrameKind::Begin {
                                    break;
                                }
                            }
                        }
                        self.newline(base);
                        self.emit(i);
                        if let Some(j) = tail {
                            if j == i + 1 {
                                self.emit(j);
                                i = j;
                            }
                        }
                        at_start = false;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "THEN"
                        if !frames.is_empty()
                            && stmt_first
                                .as_deref()
                                .is_some_and(|w| matches!(w, "IF" | "ELSIF" | "ELSEIF" | "WHEN")) =>
                    {
                        self.emit(i);
                        frames.push(Frame {
                            kind: FrameKind::Then,
                            base,
                        });
                        base += 1;
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "ELSE" | "ELSIF" | "ELSEIF" if frames.last().is_some_and(|f| f.kind == FrameKind::Then) => {
                        base = frames.pop().map_or(base0, |f| f.base);
                        self.newline(base);
                        self.emit(i);
                        if up == "ELSE" {
                            frames.push(Frame {
                                kind: FrameKind::Then,
                                base,
                            });
                            base += 1;
                            self.newline(base);
                            at_start = true;
                            stmt_first = None;
                        } else {
                            at_start = false;
                            stmt_first = Some(up.clone());
                        }
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "ELSE" if self.d == Dialect::MsSql => {
                        self.newline(base);
                        self.emit(i);
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "WHEN" if at_start && frames.last().is_some_and(|f| f.kind == FrameKind::Then) => {
                        base = frames.pop().map_or(base0, |f| f.base);
                        self.newline(base);
                        self.emit(i);
                        at_start = false;
                        stmt_first = Some(up.clone());
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "LOOP" if !frames.is_empty() && !self.prev_sig(i).is_some_and(|p| self.is_word(p, "END")) => {
                        self.emit(i);
                        frames.push(Frame {
                            kind: FrameKind::Loop,
                            base,
                        });
                        base += 1;
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "EXCEPTION" if frames.last().is_some_and(|f| f.kind == FrameKind::Begin) => {
                        let outer = frames.last().map_or(base0, |f| f.base);
                        self.newline(outer);
                        self.emit(i);
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "DECLARE" if self.d == Dialect::Oracle && at_start => {
                        self.newline(base);
                        self.emit(i);
                        frames.push(Frame {
                            kind: FrameKind::Declare,
                            base,
                        });
                        base += 1;
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    "IS" | "AS" if plsql_routine && frames.is_empty() => {
                        self.emit(i);
                        frames.push(Frame {
                            kind: FrameKind::Declare,
                            base,
                        });
                        base += 1;
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                        clause = None;
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
                if self.d.is_mysql()
                    && !frames.is_empty()
                    && ((up == "DO" && stmt_first.as_deref() == Some("WHILE")) || (up == "REPEAT" && at_start))
                {
                    self.emit(i);
                    frames.push(Frame {
                        kind: FrameKind::Loop,
                        base,
                    });
                    base += 1;
                    self.newline(base);
                    at_start = true;
                    stmt_first = None;
                    clause = None;
                    i += 1;
                    continue;
                }
                // ----- clauses -----
                let in_with = clause.is_some_and(|c| c.style == Style::With);
                if let Some((style, n)) = self.clause_at(i, at_start, in_with, stmt_first.as_deref()) {
                    if !at_start {
                        self.newline(base);
                    }
                    let mut last_word = i;
                    for _ in 1..n {
                        last_word = self.next_sig(last_word).filter(|&k| k < to).unwrap_or(last_word);
                    }
                    for k in i..=last_word {
                        if self.items[k].tok.is_comment() {
                            self.comment(k);
                        } else {
                            self.emit(k);
                        }
                    }
                    let j = last_word + 1;
                    let body_end = self.clause_end(j, to, stmt_first.as_deref());
                    let width = self.flat_width(j, body_end);
                    let broken = body_end > j && !self.fits(width, true);
                    clause = Some(Clause { style, broken });
                    between = false;
                    if broken && style == Style::List {
                        self.newline(base + 1);
                    }
                    if style == Style::Simple && matches!(up.as_str(), "UNION" | "INTERSECT" | "EXCEPT" | "MINUS") {
                        self.newline(base);
                        at_start = true;
                        stmt_first = None;
                    } else {
                        at_start = false;
                    }
                    i = j;
                    continue;
                }
                if let Some(c) = clause {
                    if c.broken && matches!(c.style, Style::Predicate | Style::Join) {
                        if up == "BETWEEN" {
                            between = true;
                        } else if matches!(up.as_str(), "AND" | "OR") {
                            if up == "AND" && between {
                                between = false;
                            } else {
                                self.newline(base + 1);
                            }
                        } else if up == "ON" && c.style == Style::Join {
                            self.newline(base + 1);
                        }
                    }
                }
            }
            // ----- groups -----
            if let Some(m) = self.matching[i].filter(|&m| m < to) {
                if kind == TokenKind::Punct {
                    self.group(i, m, depth);
                } else {
                    self.case_group(i, m, depth);
                }
                at_start = false;
                i = m + 1;
                continue;
            }
            self.emit(i);
            at_start = false;
            if self.is_punct(i, ",") {
                if let Some(c) = clause {
                    if c.broken && c.style == Style::List {
                        self.newline(base + 1);
                    } else if c.style == Style::With {
                        self.newline(base);
                    }
                }
            }
            i += 1;
        }
    }

    /// Under a MySQL `DELIMITER`, the item range `i..end` that carries the delimiter, when item `i` starts or ends it.
    fn custom_delimiter_at(&self, i: usize, to: usize) -> Option<usize> {
        let delim = self.delimiter.as_deref()?;
        if !matches!(self.kind(i), TokenKind::Word | TokenKind::Operator | TokenKind::Punct) {
            return None;
        }
        let t = self.items[i].tok;
        if self.src[t.start..].starts_with(delim) {
            let stop = t.start + delim.len();
            let mut end = i + 1;
            while end < to && self.items[end].tok.end <= stop && self.items[end].sig.is_some() {
                end += 1;
            }
            return Some(end);
        }
        t.text(self.src).ends_with(delim).then_some(i + 1)
    }

    /// Oracle `CREATE [OR REPLACE] [EDITIONABLE] PROCEDURE|FUNCTION|PACKAGE|TRIGGER|TYPE`.
    fn creates_plsql_routine(&self, i: usize) -> bool {
        let mut j = self.next_sig(i);
        for _ in 0..5 {
            let Some(k) = j else { return false };
            match self.upper[k].as_str() {
                "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE" => j = self.next_sig(k),
                "PROCEDURE" | "FUNCTION" | "PACKAGE" | "TRIGGER" | "TYPE" => return true,
                _ => return false,
            }
        }
        false
    }

    /// The clause starting at item `i`, with its style and number of words.
    fn clause_at(&self, i: usize, at_start: bool, in_with: bool, stmt_first: Option<&str>) -> Option<(Style, usize)> {
        if self.kind(i) != TokenKind::Word || !self.kw[i] {
            return None;
        }
        let w = self.upper[i].as_str();
        let n1 = self.next_sig(i);
        let next_is = |w: &str| n1.is_some_and(|j| self.is_word(j, w));
        let prev = self.prev_sig(i);
        let prev_is = |w: &str| prev.is_some_and(|p| self.is_word(p, w));
        let dml_ok = at_start || in_with;
        Some(match w {
            "SELECT" => {
                let mut n = 1;
                let mut j = n1;
                while let Some(k) = j {
                    if self.is_kw(k, "DISTINCT") || self.is_kw(k, "ALL") {
                        n += 1;
                        j = self.next_sig(k);
                    } else {
                        break;
                    }
                }
                (Style::List, n)
            }
            "FROM" if !prev_is("DISTINCT") => (Style::List, 1),
            "WHERE" | "HAVING" | "QUALIFY" | "PREWHERE" => (Style::Predicate, 1),
            "GROUP" | "ORDER" | "PARTITION" if next_is("BY") => (Style::List, 2),
            "CONNECT" if next_is("BY") => (Style::Predicate, 2),
            "START" if next_is("WITH") && !at_start => (Style::Predicate, 2),
            "UNION" | "INTERSECT" | "EXCEPT" => {
                let n = if next_is("ALL") || next_is("DISTINCT") { 2 } else { 1 };
                (Style::Simple, n)
            }
            "MINUS" => (Style::Simple, 1),
            "LIMIT" | "OFFSET" | "FETCH" | "FORMAT" | "SETTINGS" if !at_start => (Style::Simple, 1),
            "WINDOW" => (Style::List, 1),
            "RETURNING" | "OUTPUT" if !at_start => (Style::List, 1),
            "VALUES" if !prev.is_some_and(|p| self.kind(p) == TokenKind::Operator) => (Style::List, 1),
            "SET"
                if at_start
                    || (stmt_first.is_some_and(|f| matches!(f, "UPDATE" | "INSERT" | "MERGE" | "REPLACE")) && !prev_is("CHARACTER")) =>
            {
                (Style::List, 1)
            }
            "WITH" if at_start => {
                let n = if next_is("RECURSIVE") { 2 } else { 1 };
                (Style::With, n)
            }
            "INSERT" | "REPLACE" | "MERGE" if dml_ok => {
                let mut n = 1;
                if next_is("IGNORE") {
                    n += 1;
                }
                let after = (0..n).try_fold(i, |k, _| self.next_sig(k));
                if after.is_some_and(|k| self.is_word(k, "INTO")) {
                    n += 1;
                }
                (Style::Simple, n)
            }
            "UPDATE" if dml_ok => (Style::Simple, 1),
            "DELETE" if dml_ok => (Style::Simple, if next_is("FROM") { 2 } else { 1 }),
            "INTO" if stmt_first == Some("SELECT") => (Style::Simple, 1),
            "ON" if next_is("CONFLICT") => (Style::Simple, 2),
            "ON" if next_is("DUPLICATE") => {
                // ON DUPLICATE KEY UPDATE
                (Style::List, 4)
            }
            "WHEN" if stmt_first == Some("MERGE") => (Style::Simple, 1),
            _ => return self.join_at(i).map(|n| (Style::Join, n)),
        })
    }

    /// A join keyword sequence at `i` (`LEFT OUTER JOIN`, `CROSS APPLY`, `GLOBAL ANY LEFT JOIN`, `ARRAY JOIN`...).
    fn join_at(&self, i: usize) -> Option<usize> {
        const MODIFIERS: [&str; 13] = [
            "NATURAL", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "SEMI", "ANTI", "ANY", "ASOF", "GLOBAL", "ARRAY",
        ];
        let mut j = Some(i);
        let mut n = 0;
        while let Some(k) = j {
            if self.kind(k) != TokenKind::Word || n > 5 {
                return None;
            }
            let w = self.upper[k].as_str();
            if w == "JOIN" || w == "STRAIGHT_JOIN" || (w == "APPLY" && n > 0) {
                return Some(n + 1);
            }
            if !MODIFIERS.contains(&w) && !(w == "ALL" && n > 0) {
                return None;
            }
            n += 1;
            j = self.next_sig(k);
        }
        None
    }

    /// Where the body of a clause starting at `from` ends: the next clause at this level, `;`, a script line, a
    /// procedural keyword, or `to`.
    fn clause_end(&self, from: usize, to: usize, stmt_first: Option<&str>) -> usize {
        let mut i = from;
        while i < to {
            if self.is_punct(i, ";")
                || matches!(
                    self.kind(i),
                    TokenKind::SlashLine | TokenKind::BatchSeparator | TokenKind::ClientCommand
                )
            {
                return i;
            }
            if self.kw[i]
                && (self.clause_at(i, false, false, stmt_first).is_some()
                    || ["BEGIN", "END", "THEN", "ELSE", "LOOP"].contains(&self.upper[i].as_str()))
            {
                return i;
            }
            if let Some(m) = self.matching[i].filter(|&m| m < to) {
                i = m + 1;
                continue;
            }
            i += 1;
        }
        to
    }

    /// A bracketed group `open..=close`.
    fn group(&mut self, open: usize, close: usize, depth: usize) {
        let leading = self.last.is_some_and(|p| self.pending.is_none() && self.space_between(p, open));
        if self.fits(self.flat_width(open, close + 1), leading) || depth > MAX_DEPTH {
            self.emit_flat(open, close + 1);
            return;
        }
        self.emit(open);
        let outer = self.line_indent;
        let inner = outer + 1;
        let first = self.next_sig(open).filter(|&j| j < close);
        let query = first.is_some_and(|j| {
            ["SELECT", "WITH", "VALUES", "INSERT", "UPDATE", "DELETE"]
                .iter()
                .any(|w| self.is_kw(j, w))
        });
        if first.is_some() || (open + 1..close).any(|j| self.items[j].tok.is_comment()) {
            self.newline(inner);
            if query {
                self.stmt(open + 1, close, inner, depth + 1);
            } else {
                self.list(open + 1, close, inner, depth + 1);
            }
            self.newline(outer);
        }
        self.emit(close);
    }

    /// The items of a broken non-query group: one per line, or packed when every item is a single token.
    fn list(&mut self, from: usize, to: usize, level: usize, depth: usize) {
        // Item boundaries at top-level commas.
        let mut commas = Vec::new();
        let mut i = from;
        while i < to {
            if let Some(m) = self.matching[i].filter(|&m| m < to) {
                i = m + 1;
                continue;
            }
            if self.is_punct(i, ",") {
                commas.push(i);
            }
            i += 1;
        }
        let packed = {
            let mut start = from;
            let mut ok = true;
            for &c in commas.iter().chain(std::iter::once(&to)) {
                let toks = (start..c).filter(|&j| self.items[j].sig.is_some()).count();
                let comments = (start..c).any(|j| self.items[j].tok.is_comment());
                if toks > 1 || comments {
                    ok = false;
                    break;
                }
                start = c + 1;
            }
            ok
        };
        let mut i = from;
        while i < to {
            if self.emit_special(i) {
                i += 1;
                continue;
            }
            if let Some(m) = self.matching[i].filter(|&m| m < to) {
                if self.kind(i) == TokenKind::Punct {
                    self.group(i, m, depth);
                } else {
                    self.case_group(i, m, depth);
                }
                i = m + 1;
                continue;
            }
            self.emit(i);
            if self.is_punct(i, ",") {
                if packed {
                    let next_end = commas.iter().copied().find(|&c| c > i).unwrap_or(to);
                    if !self.fits(self.flat_width(i + 1, next_end), true) {
                        self.newline(level);
                    }
                } else {
                    self.newline(level);
                }
            }
            i += 1;
        }
    }

    /// `CASE ... END` at `open..=close`: one line when it fits, else one line per WHEN/ELSE.
    fn case_group(&mut self, open: usize, close: usize, depth: usize) {
        if self.fits(self.flat_width(open, close + 1), self.last.is_some()) || depth > MAX_DEPTH {
            self.emit_flat(open, close + 1);
            return;
        }
        self.emit(open);
        let outer = self.line_indent;
        let mut i = open + 1;
        while i < close {
            if self.emit_special(i) {
                i += 1;
                continue;
            }
            if self.is_kw(i, "WHEN") || self.is_kw(i, "ELSE") {
                self.newline(outer + 1);
            }
            if let Some(m) = self.matching[i].filter(|&m| m < close) {
                if self.kind(i) == TokenKind::Punct {
                    self.group(i, m, depth + 1);
                } else {
                    self.case_group(i, m, depth + 1);
                }
                i = m + 1;
                continue;
            }
            self.emit(i);
            if self.is_punct(i, ";") {
                self.newline(outer + 2);
            }
            i += 1;
        }
        self.newline(outer);
        self.emit(close);
    }
}
