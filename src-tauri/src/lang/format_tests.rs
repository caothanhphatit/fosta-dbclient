//! Formatter tests: tricky inputs per dialect must format without tripping the verifier (tokens identical up to
//! whitespace and keyword case), keep every comment, be idempotent, and lay out the way the docs say.

use super::{
    format::{signature, try_format, verify, FormatOptions, KeywordCase},
    Dialect,
};

fn opts() -> FormatOptions {
    FormatOptions::default()
}

fn fmt(d: Dialect, s: &str) -> String {
    match try_format(d, s, &opts()) {
        Ok(out) => out,
        Err(e) => panic!("{d}: {e}\n--- input ---\n{s}"),
    }
}

/// Formats, checks the round trip and idempotence, and returns the result.
fn check(d: Dialect, s: &str) -> String {
    let once = fmt(d, s);
    verify(d, s, &once).unwrap();
    let twice = fmt(d, &once);
    assert_eq!(once, twice, "{d}: not idempotent\n--- once ---\n{once}\n--- twice ---\n{twice}");
    // Every comment survives verbatim.
    let comments = |t: &str| {
        signature(d, t)
            .into_iter()
            .filter(|(k, _)| matches!(k, super::TokenKind::LineComment | super::TokenKind::BlockComment))
            .count()
    };
    assert_eq!(comments(s), comments(&once));
    once
}

const MYSQL: &[&str] = &[
    "select a,b from t where x=1 and y='it''s' -- trailing\n order by a desc limit 10",
    "SELECT `select`, \"str\\\"ing\", 'a\\'b' FROM `my table` WHERE a--1 > 0 # hash comment\n",
    "insert into t (a, b) values (1, 'x'), (2, 'y') on duplicate key update b = values(b)",
    "select count(*), left(name, 3), if(a > 1, 'x', 'y') from users u left join orders o on o.user_id = u.id group by 1",
    "select _utf8mb4'abc' collate utf8mb4_bin, x'1F', b'101', @v := @v + 1, @@session.sql_mode from dual",
    "DELIMITER $$\nCREATE PROCEDURE p(IN a INT)\nBEGIN\n  DECLARE x INT DEFAULT 0;\n  IF a > 0 THEN\n    SET x = 1;\n  ELSEIF a < 0 THEN\n    SET x = -1;\n  ELSE\n    SET x = 0;\n  END IF;\n  WHILE x < 10 DO\n    SET x = x + 1;\n  END WHILE;\n  SELECT x;\nEND$$\nDELIMITER ;\nselect 1;",
    "select /*+ MAX_EXECUTION_TIME(1000) */ * from t /*!40001 SQL_NO_CACHE */ where id in (select id from u where u.flag = 1)",
    "update t set a = a - -1, b = case when c then 1 else 2 end where id = ?",
];

const MSSQL: &[&str] = &[
    "select top 10 [order id], N'unicode', @x, @@ROWCOUNT from dbo.[Order Details] od with (nolock) where od.qty>5",
    "SET NOCOUNT ON;\nDECLARE @n INT = 1;\nIF @n = 1 BEGIN SELECT 1; END ELSE BEGIN SELECT 2; END\nGO\nselect * from #tmp\nGO 3\n",
    "CREATE PROCEDURE dbo.p @a INT AS\nBEGIN\n  /* nested /* comment */ still */\n  SELECT a FROM t WHERE a = @a;\nEND\nGO",
    "BEGIN TRY SELECT 1/0; END TRY BEGIN CATCH SELECT ERROR_MESSAGE(); END CATCH",
    "select a, b from t1 cross apply (select top 1 * from t2 where t2.id = t1.id order by t2.d desc) x",
    "merge into target t using source s on t.id = s.id when matched then update set t.v = s.v when not matched then insert (id, v) values (s.id, s.v);",
    "select $12.50 as price, geography::Point(1, 2, 4326) as p",
];

const POSTGRES: &[&str] = &[
    "select a::int, b::text[], c #> '{a,b}', d @> '{}'::jsonb, e ->> 'k' from t where f ~* '^x' and g is distinct from h",
    "create function f(x int) returns int as $body$\nbegin\n  return x + 1; -- inside\nend;\n$body$ language plpgsql;",
    "CREATE FUNCTION g() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END;",
    "with recursive r(n) as (select 1 union all select n + 1 from r where n < 10) select * from r",
    "insert into t(a) values (1) on conflict (a) do update set a = excluded.a returning *",
    "select E'it\\'s', U&'d\\0061t', $1, $$dollar ; quoted$$, arr[1:2], x=-1 from t /* a /* nested */ b */",
    "select a from t order by a desc nulls last offset 5 rows fetch first 10 rows only",
    "select count(*) filter (where x > 0) over (partition by g order by d rows between unbounded preceding and current row) from t",
];

const SQLITE: &[&str] = &[
    "select [a b], `c`, \"d\", ?1, :name, @p, $q from t limit 5 offset 2",
    "CREATE TRIGGER trg AFTER INSERT ON a BEGIN UPDATE b SET x = CASE WHEN new.y THEN 1 ELSE 0 END; DELETE FROM c WHERE id = new.id; END;",
    "pragma table_info('t'); begin; insert into t values (1); commit;",
    "select * from t where name glob 'a*' and id not in (1, 2, 3)",
];

const CLICKHOUSE: &[&str] = &[
    "select toDate(ts) as d, count() from events prewhere d > {from:Date} where user_id = {uid:UInt64} group by d order by d limit 10 format JSONEachRow",
    "SELECT 'a\\'b', `weird``name`, arrayMap(x -> x * 2, [1, 2, 3]) FROM t FINAL SAMPLE 0.1 # comment\nSETTINGS max_threads = 4",
    "select first, offset, left(s, 1), `limit` from t array join arr as a left array join b global any left join u using (id)",
    "select {'k': 1} as m, $heredoc$ raw ; text $heredoc$ as h",
];

const ORACLE: &[&str] = &[
    "select q'[it's]', nq'{x}', :bind, emp.ename from emp, dept@remote d where emp.deptno = d.deptno(+)",
    "DECLARE\n  v_count NUMBER := 0;\n  v_name emp.ename%TYPE;\nBEGIN\n  FOR r IN (SELECT * FROM emp) LOOP\n    v_count := v_count + 1;\n  END LOOP;\n  IF v_count > 0 THEN\n    dbms_output.put_line('x;y');\n  ELSIF v_count = 0 THEN\n    NULL;\n  ELSE\n    RAISE no_data_found;\n  END IF;\nEXCEPTION\n  WHEN no_data_found THEN\n    NULL;\n  WHEN others THEN\n    RAISE;\nEND;\n/\nselect 4 / 2 from dual\n/\n",
    "create or replace procedure p(a in number) is\n  x number;\nbegin\n  select count(*) into x from t where id = a;\nend p;\n/",
    "select level, lpad(' ', level) || ename from emp start with mgr is null connect by prior empno = mgr",
    "select * from t minus select * from u",
    "for i in 1..10 loop null; end loop",
];

const GENERIC: &[&str] = &[
    "select a from t where b between 1 and 2 and c = 3 or d is null",
    "select 1e-5, .5, 2.5E+3, -x, a-b, a - -b, a*-1, (a)-(b) from t",
    "",
    "   \n\n  ",
    "-- only a comment",
    "select 1 /* unterminated",
    "select 'unterminated",
];

fn corpus() -> Vec<(Dialect, &'static str)> {
    let mut v = Vec::new();
    for (d, samples) in [
        (Dialect::MySql, MYSQL),
        (Dialect::MariaDb, MYSQL),
        (Dialect::MsSql, MSSQL),
        (Dialect::Postgres, POSTGRES),
        (Dialect::Sqlite, SQLITE),
        (Dialect::ClickHouse, CLICKHOUSE),
        (Dialect::Oracle, ORACLE),
        (Dialect::Generic, GENERIC),
    ] {
        for s in samples {
            v.push((d, *s));
        }
    }
    v
}

#[test]
fn corpus_round_trips_and_is_idempotent() {
    for (d, s) in corpus() {
        check(d, s);
    }
}

#[test]
fn every_sample_round_trips_in_every_dialect() {
    // Lexing a sample with the wrong dialect must still never change it.
    for (_, s) in corpus() {
        for d in Dialect::ALL {
            let out = super::format::format_sql(d, s, &opts());
            verify(d, s, &out).unwrap();
        }
    }
}

#[test]
fn round_trips_with_every_option() {
    let variants = [
        FormatOptions {
            keyword_case: KeywordCase::Lower,
            ..opts()
        },
        FormatOptions {
            keyword_case: KeywordCase::Preserve,
            indent: 4,
            ..opts()
        },
        FormatOptions {
            use_tabs: true,
            max_line: 20,
            ..opts()
        },
        FormatOptions { max_line: 0, ..opts() },
        FormatOptions { max_line: 1, ..opts() },
    ];
    for o in variants {
        for (d, s) in corpus() {
            let out = try_format(d, s, &o).unwrap_or_else(|e| panic!("{d} {o:?}: {e}\n{s}"));
            let again = try_format(d, &out, &o).unwrap();
            assert_eq!(out, again, "{d} {o:?} not idempotent");
        }
    }
}

#[test]
fn print_corpus() {
    // `cargo test print_corpus -- --nocapture` shows the layout of every sample.
    for (d, s) in corpus() {
        println!("=== {d} ===\n{}\n", fmt(d, s));
    }
}

#[test]
fn layout_of_a_simple_query() {
    assert_eq!(
        check(
            Dialect::Postgres,
            "select a, b from t join u on u.id = t.uid where a = 1 and b > 2 order by a"
        ),
        "SELECT a, b\nFROM t\nJOIN u ON u.id = t.uid\nWHERE a = 1 AND b > 2\nORDER BY a"
    );
}

#[test]
fn long_clauses_break_at_commas_and_conditions() {
    let s = "select customer_id, first_name, last_name, email_address, phone_number, created_at, updated_at, country from customers where status = 'active' and created_at > now() - interval '30 days' and email_address like '%@example.com'";
    assert_eq!(
        check(Dialect::Postgres, s),
        "SELECT\n  customer_id,\n  first_name,\n  last_name,\n  email_address,\n  phone_number,\n  created_at,\n  updated_at,\n  country\nFROM customers\nWHERE status = 'active'\n  AND created_at > now() - interval '30 days'\n  AND email_address LIKE '%@example.com'"
    );
}

#[test]
fn subqueries_indent_when_too_long() {
    let s = "select * from orders where customer_id in (select id from customers where country = 'NL' and created_at > '2020-01-01' and status = 'active')";
    assert_eq!(
        check(Dialect::Generic, s),
        "SELECT *\nFROM orders\nWHERE customer_id IN (\n  SELECT id\n  FROM customers\n  WHERE country = 'NL' AND created_at > '2020-01-01' AND status = 'active'\n)"
    );
}

#[test]
fn keeps_comments_in_place() {
    let s = "-- header\nselect a, -- first\n  b /* inline */ from t; -- after\n\n\n-- next\nselect 2";
    assert_eq!(
        check(Dialect::Generic, s),
        "-- header\nSELECT\n  a, -- first\n  b /* inline */\nFROM t; -- after\n\n-- next\nSELECT 2"
    );
}

#[test]
fn keyword_case_never_touches_identifiers_or_functions() {
    let out = check(
        Dialect::ClickHouse,
        "select first, offset, toDate(x), left(s, 1), t.from from t limit 1",
    );
    assert_eq!(out, "SELECT first, offset, toDate(x), left(s, 1), t.from\nFROM t\nLIMIT 1");
    let lower = try_format(
        Dialect::MsSql,
        "SELECT TOP 5 Name FROM T",
        &FormatOptions {
            keyword_case: KeywordCase::Lower,
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(lower, "select top 5 Name\nfrom T");
}

#[test]
fn spacing_never_glues_or_splits_tokens() {
    // MySQL: `a--1` is a minus minus one, and must not become a comment.
    assert_eq!(check(Dialect::MySql, "select a--1"), "SELECT a - -1");
    assert_eq!(check(Dialect::Generic, "select a=-1,b<>2,c||d"), "SELECT a = -1, b <> 2, c || d");
    assert_eq!(
        check(Dialect::Postgres, "select a :: int, t . c, count (*)"),
        "SELECT a::int, t.c, count (*)"
    );
    assert_eq!(check(Dialect::MySql, "select count(*),COUNT( x )"), "SELECT count(*), COUNT(x)");
    assert_eq!(
        check(Dialect::Oracle, "select x%rowcount, t@link.c from dual"),
        "SELECT x%rowcount, t@link.c\nFROM dual"
    );
}

#[test]
fn mssql_batches_stay_on_their_own_lines() {
    let out = check(Dialect::MsSql, "select 1\ngo\nselect 2\nGO 5");
    assert_eq!(out, "SELECT 1\ngo\nSELECT 2\nGO 5");
}

#[test]
fn oracle_blocks_indent_and_keep_slash_lines() {
    let out = check(
        Dialect::Oracle,
        "begin if x > 0 then null; else raise e; end if; end;\n/\nselect 1 from dual",
    );
    assert_eq!(
        out,
        "BEGIN\n  IF x > 0 THEN\n    NULL;\n  ELSE\n    RAISE e;\n  END IF;\nEND;\n/\nSELECT 1\nFROM dual"
    );
}

#[test]
fn mysql_routine_body_indents() {
    let out = check(
        Dialect::MySql,
        "create procedure p() begin declare x int; while x < 3 do set x = x + 1; end while; select x; end",
    );
    assert_eq!(
        out,
        "CREATE PROCEDURE p()\nBEGIN\n  DECLARE x int;\n  WHILE x < 3 DO\n    SET x = x + 1;\n  END WHILE;\n  SELECT x;\nEND"
    );
}

#[test]
fn mysql_delimiter_scripts() {
    let out = check(
        Dialect::MySql,
        "DELIMITER $$\ncreate procedure p() begin select 1; end$$\nselect 2 $$\nDELIMITER ;\nselect 3;",
    );
    assert_eq!(
        out,
        "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nend$$\nSELECT 2 $$\nDELIMITER ;\nSELECT 3;"
    );
}

#[test]
fn window_and_vendor_phrases_are_recased() {
    assert_eq!(
        check(
            Dialect::Postgres,
            "select count(*) filter (where x > 0) over (partition by g) from t offset 5 rows fetch next 1 row only"
        ),
        "SELECT count(*) FILTER (WHERE x > 0) OVER (PARTITION BY g)\nFROM t\nOFFSET 5 ROWS\nFETCH NEXT 1 ROW ONLY"
    );
    assert_eq!(
        check(Dialect::MsSql, "select * from a cross apply f(a.id) x outer apply g(x.v) y"),
        "SELECT *\nFROM a\nCROSS APPLY f(a.id) x\nOUTER APPLY g(x.v) y"
    );
    assert_eq!(check(Dialect::Postgres, "select arr[1:2] from t"), "SELECT arr[1:2]\nFROM t");
}

#[test]
fn trailing_comment_does_not_break_a_clause() {
    assert_eq!(
        check(Dialect::Generic, "select a from t -- why\nwhere b = 1"),
        "SELECT a\nFROM t -- why\nWHERE b = 1"
    );
}

#[test]
fn case_breaks_per_branch_when_long() {
    let s = "select case when status = 'active' then 'Active customer' when status = 'blocked' then 'Blocked customer' else 'Unknown status value' end as label from customers";
    assert_eq!(
        check(Dialect::Generic, s),
        "SELECT\n  CASE\n    WHEN status = 'active' THEN 'Active customer'\n    WHEN status = 'blocked' THEN 'Blocked customer'\n    ELSE 'Unknown status value'\n  END AS label\nFROM customers"
    );
}

/// Random scripts glued from fragments that stress the lexer and the layout: the formatter must never fall back and
/// never panic, in any dialect.
#[test]
fn random_fragment_soup_formats_safely() {
    const FRAGMENTS: &[&str] = &[
        "select",
        "from",
        "where",
        "and",
        "or",
        "join",
        "on",
        "group by",
        "order by",
        "limit",
        "(",
        ")",
        ",",
        ";",
        ".",
        "*",
        "-",
        "--",
        "- -1",
        "=",
        "<>",
        "::",
        ":",
        "a",
        "b1",
        "\"q\"",
        "`bt`",
        "[br]",
        "'s'",
        "N'n'",
        "E'e'",
        "$$d$$",
        "$1",
        "?",
        ":p",
        "@v",
        "@@g",
        "#t",
        "1e-5",
        ".5",
        "/*c*/",
        "-- line\n",
        "# hash\n",
        "\n",
        "\n\n",
        " ",
        "case",
        "when",
        "then",
        "else",
        "end",
        "begin",
        "if",
        "loop",
        "declare",
        "is",
        "as",
        "create procedure",
        "GO\n",
        "\n/\n",
        "DELIMITER //\n",
        "//",
        "DELIMITER ;\n",
        "values",
        "set",
        "update",
        "insert into",
        "with",
        "union all",
        "between",
        "x'1F'",
        "q'[z]'",
        "{p:UInt8}",
        "%",
        "@>",
        "||",
        "!",
        "~",
        "/",
        "+",
        "e",
        "n",
        "count",
        "over",
        "filter",
        "partition",
        "rows",
        "only",
        "fetch",
    ];
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..1500 {
        let len = 3 + (next() % 40) as usize;
        let mut s = String::new();
        for _ in 0..len {
            s.push_str(FRAGMENTS[(next() % FRAGMENTS.len() as u64) as usize]);
            if next() % 3 > 0 {
                s.push(' ');
            }
        }
        for d in Dialect::ALL {
            for max_line in [0, 30, 100] {
                let o = FormatOptions { max_line, ..opts() };
                if let Err(e) = try_format(d, &s, &o) {
                    let out = super::format::format_unchecked(d, &s, &o);
                    panic!("round {round} {d} max_line {max_line}: {e}\n--- input ---\n{s}\n--- output ---\n{out}");
                }
            }
        }
    }
}
