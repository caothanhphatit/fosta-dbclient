// The Redis command table used for completion and arity checks. Arity follows `COMMAND INFO`: a positive number is
// the exact argument count including the command name, a negative one the minimum. Container commands (CONFIG,
// CLIENT, XGROUP...) list their subcommands. `args` uses the redis.io notation: lower-case words are placeholders,
// upper-case words are literal tokens, `[...]` is optional and `...` repeats.

export interface RedisCommand {
  name: string;
  arity: number;
  group: string;
  args: string;
  summary: string;
  /** For container commands: the subcommands. */
  subcommands?: RedisCommand[];
}

type Row = [name: string, arity: number, args: string, summary: string];

const group = (g: string, rows: Row[]): RedisCommand[] => rows.map(([name, arity, args, summary]) => ({ name, arity, group: g, args, summary }));

const container = (name: string, g: string, summary: string, subs: Row[]): RedisCommand => ({
  name,
  arity: -2,
  group: g,
  args: "subcommand [argument ...]",
  summary,
  subcommands: subs.map(([sub, arity, args, s]) => ({ name: `${name} ${sub}`, arity, group: g, args, summary: s })),
});

export const REDIS_COMMANDS: RedisCommand[] = [
  ...group("string", [
    ["GET", 2, "key", "Value of a key"],
    [
      "SET",
      -3,
      "key value [NX | XX] [GET] [EX seconds | PX milliseconds | EXAT unix-time-seconds | PXAT unix-time-milliseconds | KEEPTTL]",
      "Set a string value",
    ],
    ["SETNX", 3, "key value", "Set when the key does not exist"],
    ["SETEX", 4, "key seconds value", "Set with an expiry in seconds"],
    ["PSETEX", 4, "key milliseconds value", "Set with an expiry in milliseconds"],
    ["GETSET", 3, "key value", "Set and return the old value"],
    ["GETDEL", 2, "key", "Get and delete"],
    ["GETEX", -2, "key [EX seconds | PX milliseconds | EXAT unix-time-seconds | PXAT unix-time-milliseconds | PERSIST]", "Get and change the expiry"],
    ["MGET", -2, "key [key ...]", "Values of several keys"],
    ["MSET", -3, "key value [key value ...]", "Set several keys"],
    ["MSETNX", -3, "key value [key value ...]", "Set several keys when none exists"],
    ["APPEND", 3, "key value", "Append to a string"],
    ["STRLEN", 2, "key", "Length of a string"],
    ["GETRANGE", 4, "key start end", "Substring of a string"],
    ["SETRANGE", 4, "key offset value", "Overwrite part of a string"],
    ["INCR", 2, "key", "Increment by one"],
    ["INCRBY", 3, "key increment", "Increment by an integer"],
    ["INCRBYFLOAT", 3, "key increment", "Increment by a float"],
    ["DECR", 2, "key", "Decrement by one"],
    ["DECRBY", 3, "key decrement", "Decrement by an integer"],
    ["LCS", -3, "key1 key2 [LEN] [IDX] [MINMATCHLEN min-match-len] [WITHMATCHLEN]", "Longest common substring"],
  ]),
  ...group("generic", [
    ["DEL", -2, "key [key ...]", "Delete keys"],
    ["UNLINK", -2, "key [key ...]", "Delete keys asynchronously"],
    ["EXISTS", -2, "key [key ...]", "Count existing keys"],
    ["TYPE", 2, "key", "Type of a key"],
    ["KEYS", 2, "pattern", "Keys matching a pattern (blocks the server)"],
    ["SCAN", -2, "cursor [MATCH pattern] [COUNT count] [TYPE type]", "Iterate keys"],
    ["RANDOMKEY", 1, "", "A random key"],
    ["RENAME", 3, "key newkey", "Rename a key"],
    ["RENAMENX", 3, "key newkey", "Rename when the new name is free"],
    ["COPY", -3, "source destination [DB destination-db] [REPLACE]", "Copy a key"],
    ["MOVE", 3, "key db", "Move a key to another database"],
    ["EXPIRE", -3, "key seconds [NX | XX | GT | LT]", "Set a timeout in seconds"],
    ["PEXPIRE", -3, "key milliseconds [NX | XX | GT | LT]", "Set a timeout in milliseconds"],
    ["EXPIREAT", -3, "key unix-time-seconds [NX | XX | GT | LT]", "Expire at a Unix time"],
    ["PEXPIREAT", -3, "key unix-time-milliseconds [NX | XX | GT | LT]", "Expire at a Unix time in ms"],
    ["EXPIRETIME", 2, "key", "Expiry as a Unix time"],
    ["TTL", 2, "key", "Time to live in seconds"],
    ["PTTL", 2, "key", "Time to live in milliseconds"],
    ["PERSIST", 2, "key", "Remove the timeout"],
    ["TOUCH", -2, "key [key ...]", "Update last access time"],
    ["DUMP", 2, "key", "Serialized value"],
    ["RESTORE", -4, "key ttl serialized-value [REPLACE] [ABSTTL] [IDLETIME seconds] [FREQ frequency]", "Create a key from DUMP"],
    [
      "SORT",
      -2,
      "key [BY pattern] [LIMIT offset count] [GET pattern [GET pattern ...]] [ASC | DESC] [ALPHA] [STORE destination]",
      "Sort a list, set or sorted set",
    ],
    ["WAIT", 3, "numreplicas timeout", "Wait for replication"],
  ]),
  container("OBJECT", "generic", "Inspect key internals", [
    ["ENCODING", 3, "key", "Internal encoding"],
    ["FREQ", 3, "key", "LFU access frequency"],
    ["IDLETIME", 3, "key", "Seconds since last access"],
    ["REFCOUNT", 3, "key", "Reference count"],
    ["HELP", 2, "", "Help"],
  ]),
  ...group("hash", [
    ["HGET", 3, "key field", "Value of a hash field"],
    ["HSET", -4, "key field value [field value ...]", "Set hash fields"],
    ["HSETNX", 4, "key field value", "Set a field when absent"],
    ["HMGET", -3, "key field [field ...]", "Values of several fields"],
    ["HMSET", -4, "key field value [field value ...]", "Set several fields (deprecated)"],
    ["HGETALL", 2, "key", "All fields and values"],
    ["HDEL", -3, "key field [field ...]", "Delete fields"],
    ["HEXISTS", 3, "key field", "Whether a field exists"],
    ["HKEYS", 2, "key", "All field names"],
    ["HVALS", 2, "key", "All values"],
    ["HLEN", 2, "key", "Number of fields"],
    ["HSTRLEN", 3, "key field", "Length of a field value"],
    ["HINCRBY", 4, "key field increment", "Increment a field"],
    ["HINCRBYFLOAT", 4, "key field increment", "Increment a field by a float"],
    ["HRANDFIELD", -2, "key [count [WITHVALUES]]", "Random fields"],
    ["HSCAN", -3, "key cursor [MATCH pattern] [COUNT count] [NOVALUES]", "Iterate fields"],
  ]),
  ...group("list", [
    ["LPUSH", -3, "key element [element ...]", "Prepend elements"],
    ["RPUSH", -3, "key element [element ...]", "Append elements"],
    ["LPUSHX", -3, "key element [element ...]", "Prepend when the list exists"],
    ["RPUSHX", -3, "key element [element ...]", "Append when the list exists"],
    ["LPOP", -2, "key [count]", "Remove and return the first elements"],
    ["RPOP", -2, "key [count]", "Remove and return the last elements"],
    ["LRANGE", 4, "key start stop", "A range of elements"],
    ["LLEN", 2, "key", "Length of a list"],
    ["LINDEX", 3, "key index", "Element at an index"],
    ["LSET", 4, "key index element", "Set the element at an index"],
    ["LREM", 4, "key count element", "Remove elements"],
    ["LTRIM", 4, "key start stop", "Trim to a range"],
    ["LINSERT", 5, "key BEFORE | AFTER pivot element", "Insert before or after an element"],
    ["LPOS", -3, "key element [RANK rank] [COUNT num-matches] [MAXLEN len]", "Index of matching elements"],
    ["LMOVE", 5, "source destination LEFT | RIGHT LEFT | RIGHT", "Move an element between lists"],
    ["LMPOP", -4, "numkeys key [key ...] LEFT | RIGHT [COUNT count]", "Pop from the first non-empty list"],
    ["BLPOP", -3, "key [key ...] timeout", "Blocking LPOP"],
    ["BRPOP", -3, "key [key ...] timeout", "Blocking RPOP"],
    ["BLMOVE", 6, "source destination LEFT | RIGHT LEFT | RIGHT timeout", "Blocking LMOVE"],
    ["RPOPLPUSH", 3, "source destination", "Move the last element (deprecated)"],
  ]),
  ...group("set", [
    ["SADD", -3, "key member [member ...]", "Add members"],
    ["SREM", -3, "key member [member ...]", "Remove members"],
    ["SMEMBERS", 2, "key", "All members"],
    ["SISMEMBER", 3, "key member", "Whether a member exists"],
    ["SMISMEMBER", -3, "key member [member ...]", "Whether members exist"],
    ["SCARD", 2, "key", "Number of members"],
    ["SPOP", -2, "key [count]", "Remove random members"],
    ["SRANDMEMBER", -2, "key [count]", "Random members"],
    ["SINTER", -2, "key [key ...]", "Intersection"],
    ["SINTERCARD", -3, "numkeys key [key ...] [LIMIT limit]", "Size of the intersection"],
    ["SUNION", -2, "key [key ...]", "Union"],
    ["SDIFF", -2, "key [key ...]", "Difference"],
    ["SINTERSTORE", -3, "destination key [key ...]", "Store the intersection"],
    ["SUNIONSTORE", -3, "destination key [key ...]", "Store the union"],
    ["SDIFFSTORE", -3, "destination key [key ...]", "Store the difference"],
    ["SMOVE", 4, "source destination member", "Move a member"],
    ["SSCAN", -3, "key cursor [MATCH pattern] [COUNT count]", "Iterate members"],
  ]),
  ...group("sorted-set", [
    ["ZADD", -4, "key [NX | XX] [GT | LT] [CH] [INCR] score member [score member ...]", "Add members with scores"],
    ["ZREM", -3, "key member [member ...]", "Remove members"],
    ["ZRANGE", -4, "key start stop [BYSCORE | BYLEX] [REV] [LIMIT offset count] [WITHSCORES]", "A range of members"],
    ["ZRANGESTORE", -5, "dst src min max [BYSCORE | BYLEX] [REV] [LIMIT offset count]", "Store a range"],
    ["ZREVRANGE", -4, "key start stop [WITHSCORES]", "A range, high to low (deprecated)"],
    ["ZRANGEBYSCORE", -4, "key min max [WITHSCORES] [LIMIT offset count]", "Members in a score range (deprecated)"],
    ["ZREVRANGEBYSCORE", -4, "key max min [WITHSCORES] [LIMIT offset count]", "Score range, high to low (deprecated)"],
    ["ZRANGEBYLEX", -4, "key min max [LIMIT offset count]", "Members in a lexical range"],
    ["ZLEXCOUNT", 4, "key min max", "Count in a lexical range"],
    ["ZSCORE", 3, "key member", "Score of a member"],
    ["ZMSCORE", -3, "key member [member ...]", "Scores of members"],
    ["ZINCRBY", 4, "key increment member", "Increment a score"],
    ["ZCARD", 2, "key", "Number of members"],
    ["ZCOUNT", 4, "key min max", "Count in a score range"],
    ["ZRANK", -3, "key member [WITHSCORE]", "Rank, low to high"],
    ["ZREVRANK", -3, "key member [WITHSCORE]", "Rank, high to low"],
    ["ZPOPMIN", -2, "key [count]", "Remove the lowest members"],
    ["ZPOPMAX", -2, "key [count]", "Remove the highest members"],
    ["BZPOPMIN", -3, "key [key ...] timeout", "Blocking ZPOPMIN"],
    ["BZPOPMAX", -3, "key [key ...] timeout", "Blocking ZPOPMAX"],
    ["ZRANDMEMBER", -2, "key [count [WITHSCORES]]", "Random members"],
    ["ZREMRANGEBYSCORE", 4, "key min max", "Remove a score range"],
    ["ZREMRANGEBYRANK", 4, "key start stop", "Remove a rank range"],
    ["ZREMRANGEBYLEX", 4, "key min max", "Remove a lexical range"],
    ["ZUNION", -3, "numkeys key [key ...] [WEIGHTS weight [weight ...]] [AGGREGATE SUM | MIN | MAX] [WITHSCORES]", "Union"],
    ["ZINTER", -3, "numkeys key [key ...] [WEIGHTS weight [weight ...]] [AGGREGATE SUM | MIN | MAX] [WITHSCORES]", "Intersection"],
    ["ZDIFF", -3, "numkeys key [key ...] [WITHSCORES]", "Difference"],
    ["ZUNIONSTORE", -4, "destination numkeys key [key ...] [WEIGHTS weight [weight ...]] [AGGREGATE SUM | MIN | MAX]", "Store a union"],
    ["ZINTERSTORE", -4, "destination numkeys key [key ...] [WEIGHTS weight [weight ...]] [AGGREGATE SUM | MIN | MAX]", "Store an intersection"],
    ["ZSCAN", -3, "key cursor [MATCH pattern] [COUNT count]", "Iterate members"],
  ]),
  ...group("stream", [
    ["XADD", -5, "key [NOMKSTREAM] [MAXLEN | MINID [= | ~] threshold [LIMIT count]] * | id field value [field value ...]", "Append an entry"],
    ["XRANGE", -4, "key start end [COUNT count]", "Entries in an ID range"],
    ["XREVRANGE", -4, "key end start [COUNT count]", "Entries in reverse order"],
    ["XLEN", 2, "key", "Number of entries"],
    ["XREAD", -4, "[COUNT count] [BLOCK milliseconds] STREAMS key [key ...] id [id ...]", "Read entries"],
    ["XREADGROUP", -7, "GROUP group consumer [COUNT count] [BLOCK milliseconds] [NOACK] STREAMS key [key ...] id [id ...]", "Read as a consumer group"],
    ["XACK", -4, "key group id [id ...]", "Acknowledge entries"],
    ["XDEL", -3, "key id [id ...]", "Delete entries"],
    ["XTRIM", -4, "key MAXLEN | MINID [= | ~] threshold [LIMIT count]", "Trim a stream"],
    ["XPENDING", -3, "key group [[IDLE min-idle-time] start end count [consumer]]", "Pending entries"],
    [
      "XCLAIM",
      -6,
      "key group consumer min-idle-time id [id ...] [IDLE ms] [TIME unix-time-milliseconds] [RETRYCOUNT count] [FORCE] [JUSTID]",
      "Claim pending entries",
    ],
    ["XAUTOCLAIM", -6, "key group consumer min-idle-time start [COUNT count] [JUSTID]", "Claim idle pending entries"],
  ]),
  container("XGROUP", "stream", "Manage consumer groups", [
    ["CREATE", -5, "key group id | $ [MKSTREAM] [ENTRIESREAD entries-read]", "Create a group"],
    ["DESTROY", 4, "key group", "Destroy a group"],
    ["CREATECONSUMER", 5, "key group consumer", "Create a consumer"],
    ["DELCONSUMER", 5, "key group consumer", "Delete a consumer"],
    ["SETID", -5, "key group id | $ [ENTRIESREAD entries-read]", "Set the last delivered ID"],
  ]),
  container("XINFO", "stream", "Stream introspection", [
    ["STREAM", -3, "key [FULL [COUNT count]]", "Stream details"],
    ["GROUPS", 3, "key", "Consumer groups"],
    ["CONSUMERS", 4, "key group", "Consumers of a group"],
  ]),
  ...group("bitmap", [
    ["SETBIT", 4, "key offset value", "Set a bit"],
    ["GETBIT", 3, "key offset", "Value of a bit"],
    ["BITCOUNT", -2, "key [start end [BYTE | BIT]]", "Count set bits"],
    ["BITPOS", -3, "key bit [start [end [BYTE | BIT]]]", "First bit set or clear"],
    ["BITOP", -4, "AND | OR | XOR | NOT destkey key [key ...]", "Bitwise operation"],
  ]),
  ...group("hyperloglog", [
    ["PFADD", -2, "key [element [element ...]]", "Add to a HyperLogLog"],
    ["PFCOUNT", -2, "key [key ...]", "Approximate cardinality"],
    ["PFMERGE", -2, "destkey [sourcekey [sourcekey ...]]", "Merge HyperLogLogs"],
  ]),
  ...group("geo", [
    ["GEOADD", -5, "key [NX | XX] [CH] longitude latitude member [longitude latitude member ...]", "Add locations"],
    ["GEODIST", -4, "key member1 member2 [M | KM | FT | MI]", "Distance between members"],
    ["GEOPOS", -2, "key [member [member ...]]", "Positions of members"],
    ["GEOHASH", -2, "key [member [member ...]]", "Geohashes of members"],
    [
      "GEOSEARCH",
      -7,
      "key FROMMEMBER member | FROMLONLAT longitude latitude BYRADIUS radius M | KM | FT | MI | BYBOX width height M | KM | FT | MI [ASC | DESC] [COUNT count [ANY]] [WITHCOORD] [WITHDIST] [WITHHASH]",
      "Search an area",
    ],
  ]),
  ...group("pubsub", [
    ["PUBLISH", 3, "channel message", "Post a message"],
    ["SUBSCRIBE", -2, "channel [channel ...]", "Listen to channels"],
    ["PSUBSCRIBE", -2, "pattern [pattern ...]", "Listen to channel patterns"],
    ["UNSUBSCRIBE", -1, "[channel [channel ...]]", "Stop listening"],
    ["PUNSUBSCRIBE", -1, "[pattern [pattern ...]]", "Stop listening to patterns"],
  ]),
  container("PUBSUB", "pubsub", "Pub/Sub introspection", [
    ["CHANNELS", -2, "[pattern]", "Active channels"],
    ["NUMSUB", -2, "[channel [channel ...]]", "Subscriber counts"],
    ["NUMPAT", 2, "", "Pattern subscription count"],
  ]),
  ...group("transactions", [
    ["MULTI", 1, "", "Start a transaction"],
    ["EXEC", 1, "", "Run the queued commands"],
    ["DISCARD", 1, "", "Drop the queued commands"],
    ["WATCH", -2, "key [key ...]", "Watch keys for MULTI/EXEC"],
    ["UNWATCH", 1, "", "Forget watched keys"],
  ]),
  ...group("scripting", [
    ["EVAL", -3, "script numkeys [key [key ...]] [arg [arg ...]]", "Run a Lua script"],
    ["EVALSHA", -3, "sha1 numkeys [key [key ...]] [arg [arg ...]]", "Run a cached script"],
    ["EVAL_RO", -3, "script numkeys [key [key ...]] [arg [arg ...]]", "Run a read-only script"],
    ["FCALL", -3, "function numkeys [key [key ...]] [arg [arg ...]]", "Call a function"],
  ]),
  container("SCRIPT", "scripting", "Manage the script cache", [
    ["LOAD", 3, "script", "Cache a script"],
    ["EXISTS", -3, "sha1 [sha1 ...]", "Whether scripts are cached"],
    ["FLUSH", -2, "[ASYNC | SYNC]", "Empty the cache"],
    ["KILL", 2, "", "Kill the running script"],
  ]),
  container("FUNCTION", "scripting", "Manage functions", [
    ["LIST", -2, "[LIBRARYNAME library-name-pattern] [WITHCODE]", "Libraries"],
    ["LOAD", -3, "[REPLACE] function-code", "Load a library"],
    ["DELETE", 3, "library-name", "Delete a library"],
    ["FLUSH", -2, "[ASYNC | SYNC]", "Delete all libraries"],
    ["STATS", 2, "", "Running function"],
  ]),
  ...group("connection", [
    ["PING", -1, "[message]", "Test the connection"],
    ["ECHO", 2, "message", "Echo a message"],
    ["SELECT", 2, "index", "Change the database"],
    ["AUTH", -2, "[username] password", "Authenticate"],
    ["HELLO", -1, "[protover [AUTH username password] [SETNAME clientname]]", "Handshake (RESP2/RESP3)"],
    ["RESET", 1, "", "Reset the connection"],
    ["QUIT", -1, "", "Close the connection"],
  ]),
  container("CLIENT", "connection", "Manage client connections", [
    ["LIST", -2, "[TYPE NORMAL | MASTER | REPLICA | PUBSUB] [ID client-id [client-id ...]]", "Connected clients"],
    ["INFO", 2, "", "This connection"],
    ["ID", 2, "", "This connection's ID"],
    ["KILL", -3, "ip:port | ID client-id | TYPE type | USER username | ADDR ip:port | SKIPME YES | NO", "Close connections"],
    ["SETNAME", 3, "connection-name", "Name this connection"],
    ["GETNAME", 2, "", "This connection's name"],
    ["PAUSE", -3, "timeout [WRITE | ALL]", "Pause clients"],
    ["UNPAUSE", 2, "", "Resume clients"],
    ["NO-EVICT", 3, "ON | OFF", "Exclude from eviction"],
  ]),
  ...group("server", [
    ["INFO", -1, "[section [section ...]]", "Server information"],
    ["DBSIZE", 1, "", "Number of keys"],
    ["FLUSHDB", -1, "[ASYNC | SYNC]", "Delete all keys of the database"],
    ["FLUSHALL", -1, "[ASYNC | SYNC]", "Delete all keys of all databases"],
    ["TIME", 1, "", "Server time"],
    ["LASTSAVE", 1, "", "Time of the last save"],
    ["SAVE", 1, "", "Synchronous save"],
    ["BGSAVE", -1, "[SCHEDULE]", "Background save"],
    ["BGREWRITEAOF", 1, "", "Rewrite the append-only file"],
    ["ROLE", 1, "", "Replication role"],
    ["MONITOR", 1, "", "Stream every command"],
    ["SWAPDB", 3, "index1 index2", "Swap two databases"],
  ]),
  container("CONFIG", "server", "Read and change the configuration", [
    ["GET", -3, "parameter [parameter ...]", "Configuration values"],
    ["SET", -4, "parameter value [parameter value ...]", "Change configuration"],
    ["RESETSTAT", 2, "", "Reset statistics"],
    ["REWRITE", 2, "", "Write the configuration file"],
  ]),
  container("SLOWLOG", "server", "The slow query log", [
    ["GET", -2, "[count]", "Recent slow commands"],
    ["LEN", 2, "", "Length of the log"],
    ["RESET", 2, "", "Clear the log"],
  ]),
  container("MEMORY", "server", "Memory introspection", [
    ["USAGE", -3, "key [SAMPLES count]", "Bytes used by a key"],
    ["STATS", 2, "", "Memory statistics"],
    ["DOCTOR", 2, "", "Memory problems report"],
  ]),
  container("COMMAND", "server", "Command introspection", [
    ["COUNT", 2, "", "Number of commands"],
    ["INFO", -2, "[command-name [command-name ...]]", "Details of commands"],
    ["DOCS", -2, "[command-name [command-name ...]]", "Documentation of commands"],
    ["LIST", -2, "[FILTERBY MODULE module-name | ACLCAT category | PATTERN pattern]", "Command names"],
  ]),
  container("LATENCY", "server", "Latency monitoring", [
    ["LATEST", 2, "", "Latest latency samples"],
    ["HISTORY", 3, "event", "Samples of an event"],
    ["RESET", -2, "[event [event ...]]", "Reset samples"],
    ["DOCTOR", 2, "", "Latency report"],
  ]),
  container("ACL", "server", "Access control lists", [
    ["WHOAMI", 2, "", "Current user"],
    ["LIST", 2, "", "ACL rules"],
    ["USERS", 2, "", "User names"],
    ["GETUSER", 3, "username", "Rules of a user"],
    ["SETUSER", -3, "username [rule [rule ...]]", "Create or change a user"],
    ["DELUSER", -3, "username [username ...]", "Delete users"],
    ["CAT", -2, "[category]", "Command categories"],
  ]),
];

const BY_NAME = new Map<string, RedisCommand>();
for (const c of REDIS_COMMANDS) {
  BY_NAME.set(c.name, c);
  for (const s of c.subcommands ?? []) BY_NAME.set(s.name, s);
}

/** The command (or `CONTAINER SUB` subcommand) named by the first words of a command line. */
export function findCommand(words: string[]): RedisCommand | undefined {
  if (!words.length) return undefined;
  const first = words[0].toUpperCase();
  const top = BY_NAME.get(first);
  if (top?.subcommands && words.length > 1) return BY_NAME.get(`${first} ${words[1].toUpperCase()}`) ?? top;
  return top;
}

/** The literal tokens of an argument spec (`EX`, `NX`, `WITHSCORES`...). */
export function optionTokens(args: string): string[] {
  const out = new Set<string>();
  for (const m of args.matchAll(/(?<![\w-])([A-Z][A-Z0-9_-]+)(?![\w-])/g)) out.add(m[1]);
  return [...out];
}

/** Which arguments (1-based, after the command words) are key names. */
export function isKeyArgument(cmd: RedisCommand, index: number): boolean {
  const spec = cmd.args;
  if (/^key \[key \.\.\.\]/.test(spec) || /^(destination|destkey) key \[key \.\.\.\]/.test(spec)) return true;
  if (/^key value \[key value/.test(spec)) return index % 2 === 1;
  if (/^(source destination|key newkey|key1 key2)\b/.test(spec)) return index <= 2;
  if (/^key\b/.test(spec)) return index === 1;
  return false;
}

/** An arity problem of a command line, or null. `words` includes the command (and subcommand) words. */
export function arityError(words: string[]): string | null {
  const cmd = findCommand(words);
  if (!cmd) return words.length ? `Unknown command '${words[0]}'.` : null;
  // Arity counts every word, the subcommand included (CONFIG GET maxmemory has arity 3).
  const fixed = cmd.name.split(" ").length;
  const usage = `${cmd.name} ${cmd.args}`.trim();
  if (cmd.arity > 0 && words.length !== cmd.arity) return `${cmd.name} takes ${cmd.arity - fixed} argument(s): ${usage}`;
  if (cmd.arity < 0 && words.length < -cmd.arity) return `${cmd.name} takes at least ${-cmd.arity - fixed} argument(s): ${usage}`;
  return null;
}
