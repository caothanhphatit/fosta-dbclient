//! T-SQL batches: top-level statements, row counts and result labels.
//!
//! tiberius streams the result sets of a batch but drops the DONE tokens that carry "N rows affected", and its `execute`
//! keeps the counts but drops the rows. So a query tab runs the batch as a stream and asks for the count itself: after
//! every top-level INSERT, UPDATE, DELETE or MERGE it adds `SELECT @@ROWCOUNT AS [__fosta_rowcount]` on the same line
//! (server error line numbers stay those of the user's text). The batch stays one batch, so variables declared in it keep
//! their scope; those one-cell result sets become "N row(s) affected" results again.

/// Column name of the injected row-count result sets.
pub const ROWCOUNT_COLUMN: &str = "__fosta_rowcount";

const DML: [&str; 4] = ["insert", "update", "delete", "merge"];
/// `BEGIN` followed by one of these opens no block (`BEGIN TRANSACTION`, `BEGIN DIALOG CONVERSATION`…).
const BEGIN_WITHOUT_END: [&str; 5] = ["tran", "transaction", "distributed", "dialog", "conversation"];
/// Statements that return no result set.
const SILENT: [&str; 16] = [
    "declare",
    "set",
    "use",
    "print",
    "create",
    "alter",
    "drop",
    "truncate",
    "grant",
    "revoke",
    "deny",
    "commit",
    "rollback",
    "save",
    "raiserror",
    "waitfor",
];

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Semicolon,
    Open,
    Close,
    Other,
}

#[derive(Debug)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

/// T-SQL tokens, skipping whitespace and comments (block comments nest in T-SQL).
fn tokenize(sql: &str) -> Vec<Token> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            while i < bytes.len() {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let kind = match c {
            b'\'' | b'"' | b'[' => {
                let close = if c == b'[' { b']' } else { c };
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == close {
                        // A doubled closing character is an escaped one.
                        if bytes.get(i + 1) == Some(&close) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                Kind::Other
            }
            b';' => {
                i += 1;
                Kind::Semicolon
            }
            b'(' => {
                i += 1;
                Kind::Open
            }
            b')' => {
                i += 1;
                Kind::Close
            }
            _ if c.is_ascii_alphabetic() || c == b'_' || c == b'@' || c == b'#' => {
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'@' | b'#' | b'$')) {
                    i += 1;
                }
                // N'…' is a string literal, not the word N.
                if &sql[start..i] == "N" && bytes.get(i) == Some(&b'\'') {
                    continue_string(bytes, &mut i);
                    Kind::Other
                } else {
                    Kind::Word
                }
            }
            _ => {
                // Any other character, one UTF-8 sequence at a time.
                i += sql[i..].chars().next().map_or(1, char::len_utf8);
                Kind::Other
            }
        };
        tokens.push(Token { kind, start, end: i });
    }
    tokens
}

fn continue_string(bytes: &[u8], i: &mut usize) {
    *i += 1;
    while *i < bytes.len() {
        if bytes[*i] == b'\'' {
            if bytes.get(*i + 1) == Some(&b'\'') {
                *i += 2;
                continue;
            }
            break;
        }
        *i += 1;
    }
    *i = (*i + 1).min(bytes.len());
}

/// One top-level statement: the byte range of its first to last token and what the labels need to know.
#[derive(Debug, PartialEq, Eq)]
struct Statement {
    start: usize,
    end: usize,
    first_word: String,
    counts_rows: bool,
    /// `SELECT @var = …` (an assignment, no result set).
    assigns: bool,
    /// INTO or OUTPUT outside parentheses.
    into: bool,
    output: bool,
    /// The word after the first (`BEGIN TRAN`).
    second_word: Option<String>,
}

/// Splits at `;` outside BEGIN…END and CASE…END blocks and parentheses.
fn top_level_statements(sql: &str) -> Vec<Statement> {
    let tokens = tokenize(sql);
    let mut statements = Vec::new();
    let mut block_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut current: Option<Statement> = None;
    let mut top_dml = false;
    let mut position = 0usize;
    let mut finish = |current: &mut Option<Statement>, top_dml: &mut bool| {
        if let Some(mut statement) = current.take() {
            statement.counts_rows = DML.contains(&statement.first_word.as_str()) || (statement.first_word == "with" && *top_dml);
            statements.push(statement);
        }
        *top_dml = false;
    };
    for (index, token) in tokens.iter().enumerate() {
        let text = &sql[token.start..token.end];
        if token.kind == Kind::Semicolon && block_depth == 0 && paren_depth == 0 {
            finish(&mut current, &mut top_dml);
            position = 0;
            continue;
        }
        let word = (token.kind == Kind::Word).then(|| text.to_ascii_lowercase());
        let statement = current.get_or_insert_with(|| Statement {
            start: token.start,
            end: token.end,
            first_word: word.clone().unwrap_or_default(),
            counts_rows: false,
            assigns: false,
            into: false,
            output: false,
            second_word: None,
        });
        statement.end = token.end;
        if position == 1 {
            statement.second_word = word.clone();
            statement.assigns = statement.first_word == "select"
                && text.starts_with('@')
                && !text.starts_with("@@")
                && next_is_assignment(sql, &tokens, index);
        }
        position += 1;
        match token.kind {
            Kind::Open => paren_depth += 1,
            Kind::Close => paren_depth = paren_depth.saturating_sub(1),
            Kind::Word => {
                let word = word.unwrap_or_default();
                match word.as_str() {
                    "begin" => {
                        let next = tokens
                            .get(index + 1)
                            .filter(|next| next.kind == Kind::Word)
                            .map(|next| sql[next.start..next.end].to_ascii_lowercase());
                        if !next.is_some_and(|next| BEGIN_WITHOUT_END.contains(&next.as_str())) {
                            block_depth += 1;
                        }
                    }
                    "case" => block_depth += 1,
                    "end" => block_depth = block_depth.saturating_sub(1),
                    _ => {}
                }
                if paren_depth == 0 {
                    if DML.contains(&word.as_str()) {
                        top_dml = true;
                    }
                    if word == "into" && position > 1 {
                        statement.into = true;
                    }
                    if word == "output" {
                        statement.output = true;
                    }
                }
            }
            _ => {}
        }
    }
    finish(&mut current, &mut top_dml);
    statements
}

/// `=` or a compound assignment (`+=` …) after a variable.
fn next_is_assignment(sql: &str, tokens: &[Token], index: usize) -> bool {
    let text = |i: usize| tokens.get(i).map(|token| &sql[token.start..token.end]).unwrap_or_default();
    text(index + 1) == "=" || (matches!(text(index + 1), "+" | "-" | "*" | "/" | "|" | "&" | "^") && text(index + 2) == "=")
}

/// Whether a batch may get row-count queries: the added SELECT resets @@ROWCOUNT and @@ERROR, so a batch that reads them
/// is run as written.
fn may_count_rows(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    !["@@rowcount", "@@error", "rowcount_big"].iter().any(|name| lower.contains(name))
}

/// The batch with a row-count query after every top-level data change; unchanged when there is none.
pub fn with_row_counts(sql: &str) -> String {
    if !may_count_rows(sql) {
        return sql.to_owned();
    }
    let statements = top_level_statements(sql);
    if !statements.iter().any(|statement| statement.counts_rows) {
        return sql.to_owned();
    }
    let mut out = String::with_capacity(sql.len() + 64);
    let mut copied = 0;
    for statement in statements.iter().filter(|statement| statement.counts_rows) {
        out.push_str(&sql[copied..statement.end]);
        out.push_str(&format!("; SELECT @@ROWCOUNT AS [{ROWCOUNT_COLUMN}]"));
        copied = statement.end;
    }
    out.push_str(&sql[copied..]);
    out
}

/// How many result sets a statement returns, when that can be told from its text.
#[derive(Debug, PartialEq, Eq)]
enum Results {
    None,
    One,
    Unknown,
}

fn results_of(statement: &Statement, counted: bool) -> Results {
    let dml = if statement.output {
        Results::Unknown
    } else if counted {
        Results::One
    } else {
        Results::None
    };
    match statement.first_word.as_str() {
        "select" if statement.assigns || statement.into => Results::None,
        "select" => Results::One,
        "with" if statement.counts_rows => dml,
        "with" if statement.into => Results::None,
        "with" => Results::One,
        word if DML.contains(&word) => dml,
        "begin"
            if statement
                .second_word
                .as_deref()
                .is_some_and(|next| BEGIN_WITHOUT_END.contains(&next)) =>
        {
            Results::None
        }
        word if SILENT.contains(&word) => Results::None,
        _ => Results::Unknown,
    }
}

/// The statement text of each result set of a batch, in order, as far as it can be told from the text: labels stop at
/// the first statement whose result count is unknown (IF, EXEC, OUTPUT …).
pub fn result_labels(sql: &str) -> Vec<String> {
    let statements = top_level_statements(sql);
    if let [only] = statements.as_slice() {
        return vec![sql[only.start..only.end].to_owned()];
    }
    let counted = may_count_rows(sql);
    let mut labels = Vec::new();
    for statement in &statements {
        match results_of(statement, counted) {
            Results::None => {}
            Results::One => labels.push(sql[statement.start..statement.end].to_owned()),
            Results::Unknown => break,
        }
    }
    labels
}

fn line_of(sql: &str, offset: usize) -> u32 {
    sql[..offset].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

/// The top-level statement a server error on `line` (1-based) belongs to: its index and text. When several statements
/// start on that line, the first one that spans it.
pub fn statement_at_line(sql: &str, line: u32) -> Option<(usize, String)> {
    let statements = top_level_statements(sql);
    let ranges: Vec<(u32, u32)> = statements
        .iter()
        .map(|statement| (line_of(sql, statement.start), line_of(sql, statement.end)))
        .collect();
    let starting: Vec<usize> = (0..ranges.len()).filter(|&i| ranges[i].0 == line).collect();
    let index = match starting.as_slice() {
        [only] => Some(*only),
        _ => (0..ranges.len())
            .find(|&i| ranges[i].0 <= line && line <= ranges[i].1)
            .or_else(|| (0..ranges.len()).rev().find(|&i| ranges[i].0 <= line)),
    }?;
    let statement = &statements[index];
    Some((index, sql[statement.start..statement.end].to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNT: &str = "; SELECT @@ROWCOUNT AS [__fosta_rowcount]";

    #[test]
    fn counts_follow_each_data_change() {
        assert_eq!(
            with_row_counts("UPDATE t SET a = 1 WHERE id = 1; SELECT TOP 2 id FROM t"),
            format!("UPDATE t SET a = 1 WHERE id = 1{COUNT}; SELECT TOP 2 id FROM t")
        );
        assert_eq!(
            with_row_counts("SELECT 1; DELETE FROM t WHERE id = 2"),
            format!("SELECT 1; DELETE FROM t WHERE id = 2{COUNT}")
        );
        assert_eq!(
            with_row_counts("insert into t values (1);\ninsert into t values (2);"),
            format!("insert into t values (1){COUNT};\ninsert into t values (2){COUNT};")
        );
    }

    #[test]
    fn reads_ddl_and_procedures_are_left_alone() {
        for sql in [
            "SELECT * FROM t",
            "CREATE TABLE x (id INT)",
            "EXEC sp_who",
            "DECLARE @n INT = 1; SELECT @n",
            // The batch reads @@ROWCOUNT itself; an added SELECT would change what it sees.
            "UPDATE t SET a = 1; IF @@ROWCOUNT = 0 INSERT INTO t VALUES (1)",
        ] {
            assert_eq!(with_row_counts(sql), sql);
        }
    }

    #[test]
    fn strings_comments_and_brackets_do_not_split() {
        let sql = "UPDATE [we;ird] SET note = N'a; b' /* ; */ -- ;\nWHERE id = 1";
        assert_eq!(with_row_counts(sql), format!("{sql}{COUNT}"));
        // A trailing comment must not swallow the added query.
        assert_eq!(with_row_counts("DELETE FROM t -- done"), format!("DELETE FROM t{COUNT} -- done"));
    }

    #[test]
    fn blocks_are_one_statement_and_get_no_count_inside() {
        let sql = "IF 1 = 1 BEGIN UPDATE t SET a = 1; DELETE FROM t; END; SELECT 1";
        assert_eq!(with_row_counts(sql), sql);
        let case = "UPDATE t SET a = CASE WHEN b = 1 THEN 'x' ELSE 'y' END; SELECT 1";
        assert_eq!(
            with_row_counts(case),
            format!("UPDATE t SET a = CASE WHEN b = 1 THEN 'x' ELSE 'y' END{COUNT}; SELECT 1")
        );
        // BEGIN TRANSACTION opens no block.
        assert_eq!(
            with_row_counts("BEGIN TRAN; UPDATE t SET a = 1; COMMIT"),
            format!("BEGIN TRAN; UPDATE t SET a = 1{COUNT}; COMMIT")
        );
    }

    #[test]
    fn common_table_expressions_that_change_rows_are_counted() {
        let sql = "WITH old AS (SELECT id FROM t WHERE a < 0) DELETE FROM t WHERE id IN (SELECT id FROM old)";
        assert_eq!(with_row_counts(sql), format!("{sql}{COUNT}"));
        let read = "WITH x AS (SELECT 1 AS n) SELECT n FROM x";
        assert_eq!(with_row_counts(read), read);
    }

    #[test]
    fn result_sets_are_labelled_with_their_statement() {
        assert_eq!(result_labels("SELECT 1"), ["SELECT 1"]);
        assert_eq!(
            result_labels("DECLARE @n INT = 1; SELECT @n = 2; UPDATE t SET a = @n; SELECT a FROM t; IF 1 = 1 SELECT 2; SELECT 3"),
            ["UPDATE t SET a = @n", "SELECT a FROM t"]
        );
        assert_eq!(
            result_labels("BEGIN TRAN; SELECT * INTO x FROM t; DELETE FROM t OUTPUT deleted.id; SELECT 1"),
            Vec::<String>::new()
        );
        // A batch that reads @@ROWCOUNT gets no row-count results.
        assert_eq!(result_labels("UPDATE t SET a = 1; SELECT @@ROWCOUNT"), ["SELECT @@ROWCOUNT"]);
    }

    #[test]
    fn error_lines_map_to_statements() {
        let sql = "SELECT 1;\nSELECT *\nFROM nope;\nSELECT 3";
        assert_eq!(statement_at_line(sql, 3), Some((1, "SELECT *\nFROM nope".to_owned())));
        assert_eq!(statement_at_line(sql, 4), Some((2, "SELECT 3".to_owned())));
        assert_eq!(statement_at_line("SELECT 1; SELEC 2", 1).map(|(index, _)| index), Some(0));
        assert_eq!(statement_at_line("", 1), None);
        // The added row-count queries keep the user's line numbers.
        assert_eq!(with_row_counts("DELETE FROM t\nWHERE id = 1").lines().count(), 2);
    }
}
