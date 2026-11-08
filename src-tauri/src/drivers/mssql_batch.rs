//! Row counts for SQL Server batches. tiberius streams the result sets of a batch but drops the
//! DONE tokens that carry "N rows affected", and its `execute` keeps the counts but drops the rows.
//! So a query tab runs the batch as a stream and asks for the count itself: after every top-level
//! INSERT, UPDATE, DELETE or MERGE it adds `SELECT @@ROWCOUNT AS [__fosta_rowcount]`. The batch stays
//! one batch, so variables declared in it keep their scope; the driver turns those one-cell result
//! sets back into "N row(s) affected" entries.

/// Column name of the injected row-count result sets.
pub(crate) const ROWCOUNT_COLUMN: &str = "__fosta_rowcount";

const DML: [&str; 4] = ["insert", "update", "delete", "merge"];
/// `BEGIN` followed by one of these opens no block (`BEGIN TRANSACTION`, `BEGIN DIALOG CONVERSATION`…).
const BEGIN_WITHOUT_END: [&str; 5] = ["tran", "transaction", "distributed", "dialog", "conversation"];

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

/// One top-level statement: the byte range of its first to last token, and whether it changes rows.
#[derive(Debug, PartialEq, Eq)]
struct Statement {
    end: usize,
    counts_rows: bool,
}

/// Splits at `;` outside BEGIN…END and CASE…END blocks and parentheses.
fn top_level_statements(sql: &str) -> Vec<Statement> {
    let tokens = tokenize(sql);
    let mut statements = Vec::new();
    let mut block_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut first_word: Option<String> = None;
    let mut top_dml = false;
    let mut last_end: Option<usize> = None;
    let mut finish = |first: &mut Option<String>, top_dml: &mut bool, last_end: &mut Option<usize>| {
        if let (Some(word), Some(end)) = (first.take(), last_end.take()) {
            let counts_rows = DML.contains(&word.as_str()) || (word == "with" && *top_dml);
            statements.push(Statement { end, counts_rows });
        }
        *top_dml = false;
    };
    for (index, token) in tokens.iter().enumerate() {
        let text = &sql[token.start..token.end];
        match token.kind {
            Kind::Semicolon if block_depth == 0 && paren_depth == 0 => {
                finish(&mut first_word, &mut top_dml, &mut last_end);
                continue;
            }
            Kind::Open => paren_depth += 1,
            Kind::Close => paren_depth = paren_depth.saturating_sub(1),
            Kind::Word => {
                let word = text.to_ascii_lowercase();
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
                if paren_depth == 0 && DML.contains(&word.as_str()) {
                    top_dml = true;
                }
                if first_word.is_none() {
                    first_word = Some(word);
                }
            }
            _ => {}
        }
        last_end = Some(token.end);
    }
    finish(&mut first_word, &mut top_dml, &mut last_end);
    statements
}

/// The batch with a row-count query after every top-level data change; unchanged when there is none.
pub(crate) fn with_row_counts(sql: &str) -> String {
    // The added SELECT resets @@ROWCOUNT and @@ERROR, so a batch that reads them is run as written.
    let lower = sql.to_ascii_lowercase();
    if ["@@rowcount", "@@error", "rowcount_big"].iter().any(|name| lower.contains(name)) {
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
        out.push_str(&format!(";\nSELECT @@ROWCOUNT AS [{ROWCOUNT_COLUMN}]"));
        copied = statement.end;
    }
    out.push_str(&sql[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNT: &str = ";\nSELECT @@ROWCOUNT AS [__fosta_rowcount]";

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
}
