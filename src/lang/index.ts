// Public API of the language library (see docs/engines/language.md).
export * from "./types";
export { SchemaCache, fetchCompletionSchema, type SchemaLoader } from "./schemaCache";

export { tokenize, significant, type Token, type TokenKind } from "./sql/lexer";
export { splitStatements, statementAt as sqlStatementAt, statementRangeAt } from "./sql/split";
export { analyzeSql, resolveMember, findTable, type SqlContext, type ScopeTable, type ContextKind } from "./sql/analyze";
export { sqlCompletions, sqlCompletionSource, aliasFor, type SqlCompletionOptions } from "./sql/completion";
export { dialectFor, needsQuoting, quoteName, quoteIfNeeded, unquote } from "./sql/dialect";
export { dialectKeywords, dialectFunctions, isReservedWord } from "./sql/keywords";
export {
  createSqlLanguage,
  sqlEditorLanguage,
  codemirrorDialect,
  formatSqlRemote,
  currentStatement,
  currentSqlStatement,
  formatInEditor,
  type SqlLanguageOptions,
  type SqlLanguageSupport,
  type CurrentStatement,
} from "./sql/support";

export {
  analyzeMongo,
  mongoCompletions,
  mongoCompletionSource,
  mongoStatements,
  mongoStatementAt,
  type MongoContext,
  type MongoRole,
} from "./mongo/completion";
export { createMongoLanguage, mongoEditorExtension } from "./mongo/support";

export { REDIS_COMMANDS, findCommand, arityError, optionTokens, isKeyArgument, type RedisCommand } from "./redis/commands";
export { redisCompletions, redisCompletionSource, redisStatementAt, redisWords, quoteArg } from "./redis/completion";
export { createRedisLanguage } from "./redis/support";
