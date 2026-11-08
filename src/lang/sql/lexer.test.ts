import { describe, expect, it } from "vitest";
import { ALL_DIALECTS, type LangDialect } from "../types";
import { significant, tokenize } from "./lexer";
import { splitStatements, statementAt } from "./split";

const texts = (d: LangDialect, s: string) => significant(d, s).map((t) => t.text);
const stmts = (d: LangDialect, s: string) => splitStatements(d, s).map((r) => s.slice(r.from, r.to));

describe("lexer", () => {
  it("is lossless on odd input in every dialect", () => {
    const samples = [
      "select 'unterminated",
      "/* open comment",
      'select "a""b", `c``d`, [e]]f] from t -- tail',
      "select $$a$$, $x$b$x$, $1, E'\\'', U&'d\\0061t' from t",
      "select q'[it's]', :bind from dual\n/\n",
      "SELECT 1\r\nGO\r\nselect N'é€', @@ROWCOUNT, #tmp.x",
      "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$\nDELIMITER ;",
      "select {id:UInt32}, x'1F', 1e-5, .5 # comment\n",
      "a--1",
      "",
    ];
    for (const d of ALL_DIALECTS) {
      for (const s of samples) {
        const toks = tokenize(d, s);
        expect(toks.map((t) => t.text).join("")).toBe(s);
        toks.forEach((t, i) => i > 0 && expect(t.start).toBe(toks[i - 1].end));
      }
    }
  });

  it("knows each dialect's comments, strings and operators", () => {
    expect(texts("mySql", 'select a--1, "x\\"y" # c\nfrom t')).toEqual(["select", "a", "-", "-", "1", ",", '"x\\"y"', "from", "t"]);
    expect(texts("postgres", "select a::int, b #> '{x}', x=-1")).toEqual(["select", "a", "::", "int", ",", "b", "#>", "'{x}'", ",", "x", "=", "-", "1"]);
    expect(significant("msSql", "select [a]]b], @x from #t\nGO 2").map((t) => t.kind)).toEqual([
      "word",
      "quotedIdent",
      "punct",
      "variable",
      "word",
      "word",
      "batchSeparator",
    ]);
    expect(significant("oracle", "select q'[a';b]' from dual\n/").map((t) => t.kind)).toEqual(["word", "string", "word", "word", "slashLine"]);
    expect(significant("clickHouse", "select {id:UInt32} # c\n")[1].kind).toBe("param");
    expect(texts("generic", "1e-5 + 2.5E+3 - .5")).toEqual(["1e-5", "+", "2.5E+3", "-", ".5"]);
  });
});

describe("splitStatements", () => {
  it("splits on semicolons outside strings and comments", () => {
    expect(stmts("postgres", "select 'a;b', \"c;d\" from t; -- x;y\n/* p;q */ select 2;;")).toEqual(["select 'a;b', \"c;d\" from t", "select 2"]);
  });

  it("keeps dialect bodies whole", () => {
    expect(stmts("postgres", "CREATE FUNCTION g() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END; SELECT 3")).toHaveLength(2);
    expect(stmts("sqlite", "CREATE TRIGGER t AFTER INSERT ON a BEGIN DELETE FROM c; END; select 1")).toHaveLength(2);
    expect(stmts("mySql", "CREATE PROCEDURE p() BEGIN IF 1 THEN SELECT 1; END IF; END; SELECT 3;")).toHaveLength(2);
    expect(stmts("mariaDb", "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND$$\nSELECT 2$$\nDELIMITER ;\nSELECT 3;")).toEqual([
      "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND",
      "SELECT 2",
      "SELECT 3",
    ]);
    const mssql = stmts(
      "msSql",
      "CREATE PROCEDURE p AS\nSELECT 1;\nSELECT 2;\nGO\nIF @x = 1 BEGIN SELECT 1; END ELSE BEGIN SELECT 2; END;\nBEGIN TRAN; select 4",
    );
    expect(mssql).toHaveLength(4);
    expect(mssql[2]).toBe("BEGIN TRAN");
    const oracle = stmts("oracle", "select 1 from dual;\nBEGIN\n  null;\nEND;\n/\nselect 4 / 2 from dual");
    expect(oracle).toEqual(["select 1 from dual", "BEGIN\n  null;\nEND;", "select 4 / 2 from dual"]);
  });
});

describe("statementAt", () => {
  const s = "select 1;\nselect 2; select 3;\n\n\nselect 4";
  const at = (o: number) => {
    const r = statementAt("generic", s, o);
    return s.slice(r.from, r.to);
  };
  it("picks the statement under or nearest to the cursor", () => {
    expect(at(0)).toBe("select 1");
    expect(at(9)).toBe("select 1");
    expect(at(10)).toBe("select 2");
    expect(at(19)).toBe("select 2");
    expect(at(21)).toBe("select 3");
    expect(at(30)).toBe("select 3");
    expect(at(31)).toBe("select 4");
    expect(at(10_000)).toBe("select 4");
  });
  it("returns an empty range when there is nothing to run", () => {
    expect(statementAt("generic", "  -- only a comment\n", 3)).toEqual({ from: 3, to: 3 });
  });
});
