/**
 * Shared SQL bits for the SQL engines' UI modules. The quoting helpers stay in `core/sql.ts` because the shared grid,
 * filters and exporters (which must not import `src/engines`) use them too; engine modules import them from here.
 * Not an engine: there is no `index.ts`, so `engines/index.ts` does not load this folder as one.
 */
export { containsPattern, describeKey, dialectOf, equalsFilter, quoteIdent, quoteTable, sqlLiteral, type SqlDialect } from "../../core/sql";
export { sqlLanguageSupport, splitStatements, statementAt } from "../../core/language";
