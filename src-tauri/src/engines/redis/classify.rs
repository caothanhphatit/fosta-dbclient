//! Command classification for the statement gateway.
//!
//! | Class         | Meaning                                                    | Examples                                                   |
//! |---------------|------------------------------------------------------------|------------------------------------------------------------|
//! | `Read`        | Reads data or metadata                                     | GET, HGETALL, SCAN, ZRANGE, XRANGE, INFO, JSON.GET, FT.SEARCH |
//! | `Write`       | Changes data of the keys it names                          | SET, HSET, LPUSH, ZADD, XADD, EXPIRE, DEL/UNLINK of keys    |
//! | `Destructive` | Can drop data beyond the keys it names, or cannot be checked | FLUSHALL, FLUSHDB, SWAPDB, EVAL/EVALSHA/FCALL (scripts can delete by pattern), FUNCTION FLUSH, FT.DROPINDEX DD |
//! | `Admin`       | Changes server state, not data, or blocks the server; reads that expose secrets or other clients (CONFIG GET, ACL LIST/USERS/GETUSER/LOG, CLIENT LIST/INFO, SLOWLOG GET, LATENCY) | CONFIG SET, SHUTDOWN, DEBUG, KEYS *, SCRIPT FLUSH, ACL SETUSER, CLIENT KILL, REPLICAOF, CLUSTER ... |
//! | `Session`     | Changes only this connection                               | SELECT, MULTI/EXEC/DISCARD, WATCH, CLIENT SETNAME, HELLO, AUTH |
//! | `Unsupported` | Needs a streaming channel (deferred)                       | SUBSCRIBE, PSUBSCRIBE, SSUBSCRIBE, MONITOR, SYNC, PSYNC     |
//! | `Unknown`     | Not in the table (module commands etc.); treat like `Write` and ask | |
//!
//! `EXEC` takes the highest class of the commands queued since `MULTI` (tracked by the console).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandClass {
    Session,
    Read,
    Write,
    Unknown,
    Admin,
    Destructive,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub class: CommandClass,
    /// Upper-case command, with the subcommand for container commands (`CONFIG SET`, `XINFO STREAM`).
    pub command: String,
    /// Why the class is above `Write`, for the confirmation dialog.
    pub reason: Option<String>,
}

const READ: &[&str] = &[
    "GET",
    "MGET",
    "STRLEN",
    "GETRANGE",
    "SUBSTR",
    "LCS",
    "EXISTS",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "KEYS",
    "SCAN",
    "RANDOMKEY",
    "DUMP",
    "TOUCH",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSCAN",
    "HSTRLEN",
    "HRANDFIELD",
    "HTTL",
    "HPTTL",
    "HEXPIRETIME",
    "HPEXPIRETIME",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "LPOS",
    "SMEMBERS",
    "SISMEMBER",
    "SMISMEMBER",
    "SCARD",
    "SSCAN",
    "SRANDMEMBER",
    "SINTER",
    "SUNION",
    "SDIFF",
    "SINTERCARD",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZREVRANGEBYLEX",
    "ZSCORE",
    "ZMSCORE",
    "ZCARD",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZRANK",
    "ZREVRANK",
    "ZSCAN",
    "ZRANDMEMBER",
    "ZDIFF",
    "ZINTER",
    "ZUNION",
    "ZINTERCARD",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XREAD",
    "XPENDING",
    "PFCOUNT",
    "GETBIT",
    "BITCOUNT",
    "BITPOS",
    "BITFIELD_RO",
    "GEOPOS",
    "GEODIST",
    "GEOHASH",
    "GEOSEARCH",
    "GEORADIUS_RO",
    "GEORADIUSBYMEMBER_RO",
    "SORT_RO",
    "EVAL_RO",
    "EVALSHA_RO",
    "FCALL_RO",
    "DBSIZE",
    "INFO",
    "TIME",
    "LASTSAVE",
    "ROLE",
    "LOLWUT",
    "WAIT",
    "WAITAOF",
    "JSON.GET",
    "JSON.MGET",
    "JSON.TYPE",
    "JSON.STRLEN",
    "JSON.ARRLEN",
    "JSON.ARRINDEX",
    "JSON.OBJKEYS",
    "JSON.OBJLEN",
    "JSON.RESP",
    "JSON.DEBUG",
    "FT.SEARCH",
    "FT.AGGREGATE",
    "FT.INFO",
    "FT._LIST",
    "FT.EXPLAIN",
    "FT.EXPLAINCLI",
    "FT.PROFILE",
    "FT.SPELLCHECK",
    "FT.SYNDUMP",
    "FT.TAGVALS",
    "FT.DICTDUMP",
    "FT.HYBRID",
    "TS.GET",
    "TS.MGET",
    "TS.RANGE",
    "TS.REVRANGE",
    "TS.MRANGE",
    "TS.MREVRANGE",
    "TS.INFO",
    "TS.QUERYINDEX",
    "BF.EXISTS",
    "BF.MEXISTS",
    "BF.INFO",
    "BF.CARD",
    "CF.EXISTS",
    "CF.MEXISTS",
    "CF.COUNT",
    "CF.INFO",
    "CMS.QUERY",
    "CMS.INFO",
    "TOPK.QUERY",
    "TOPK.LIST",
    "TOPK.INFO",
    "TDIGEST.QUANTILE",
    "TDIGEST.CDF",
    "TDIGEST.INFO",
    "TDIGEST.MIN",
    "TDIGEST.MAX",
    "TDIGEST.RANK",
    "TDIGEST.TRIMMED_MEAN",
    "VSIM",
    "VCARD",
    "VDIM",
    "VEMB",
    "VGETATTR",
    "VINFO",
    "VLINKS",
    "VRANDMEMBER",
    "VISMEMBER",
];

const WRITE: &[&str] = &[
    "SET",
    "SETNX",
    "SETEX",
    "PSETEX",
    "MSET",
    "MSETNX",
    "APPEND",
    "INCR",
    "INCRBY",
    "INCRBYFLOAT",
    "DECR",
    "DECRBY",
    "GETSET",
    "GETDEL",
    "GETEX",
    "SETRANGE",
    "SETBIT",
    "BITOP",
    "BITFIELD",
    "DEL",
    "UNLINK",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "PERSIST",
    "RENAME",
    "RENAMENX",
    "COPY",
    "MOVE",
    "RESTORE",
    "HSET",
    "HSETNX",
    "HMSET",
    "HDEL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "HEXPIRE",
    "HPEXPIRE",
    "HEXPIREAT",
    "HPEXPIREAT",
    "HPERSIST",
    "HGETDEL",
    "HGETEX",
    "HSETEX",
    "LPUSH",
    "RPUSH",
    "LPUSHX",
    "RPUSHX",
    "LPOP",
    "RPOP",
    "LSET",
    "LREM",
    "LINSERT",
    "LTRIM",
    "LMOVE",
    "BLMOVE",
    "RPOPLPUSH",
    "BRPOPLPUSH",
    "BLPOP",
    "BRPOP",
    "LMPOP",
    "BLMPOP",
    "SADD",
    "SREM",
    "SPOP",
    "SMOVE",
    "SINTERSTORE",
    "SUNIONSTORE",
    "SDIFFSTORE",
    "ZADD",
    "ZREM",
    "ZINCRBY",
    "ZPOPMIN",
    "ZPOPMAX",
    "BZPOPMIN",
    "BZPOPMAX",
    "ZMPOP",
    "BZMPOP",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREMRANGEBYLEX",
    "ZRANGESTORE",
    "ZINTERSTORE",
    "ZUNIONSTORE",
    "ZDIFFSTORE",
    "XADD",
    "XDEL",
    "XTRIM",
    "XACK",
    "XCLAIM",
    "XAUTOCLAIM",
    "XREADGROUP",
    "XSETID",
    "XDELEX",
    "XACKDEL",
    "PFADD",
    "PFMERGE",
    "GEOADD",
    "GEOSEARCHSTORE",
    "GEORADIUS",
    "GEORADIUSBYMEMBER",
    "SORT",
    "PUBLISH",
    "SPUBLISH",
    "JSON.SET",
    "JSON.MSET",
    "JSON.MERGE",
    "JSON.DEL",
    "JSON.FORGET",
    "JSON.CLEAR",
    "JSON.TOGGLE",
    "JSON.NUMINCRBY",
    "JSON.NUMMULTBY",
    "JSON.STRAPPEND",
    "JSON.ARRAPPEND",
    "JSON.ARRINSERT",
    "JSON.ARRPOP",
    "JSON.ARRTRIM",
    "FT.CREATE",
    "FT.ALTER",
    "FT.ALIASADD",
    "FT.ALIASUPDATE",
    "FT.ALIASDEL",
    "FT.SYNUPDATE",
    "FT.DICTADD",
    "FT.DICTDEL",
    "FT.SUGADD",
    "FT.SUGDEL",
    "TS.CREATE",
    "TS.ALTER",
    "TS.ADD",
    "TS.MADD",
    "TS.INCRBY",
    "TS.DECRBY",
    "TS.DEL",
    "TS.CREATERULE",
    "TS.DELETERULE",
    "BF.ADD",
    "BF.MADD",
    "BF.INSERT",
    "BF.RESERVE",
    "CF.ADD",
    "CF.ADDNX",
    "CF.INSERT",
    "CF.DEL",
    "CF.RESERVE",
    "CMS.INCRBY",
    "CMS.INITBYDIM",
    "CMS.INITBYPROB",
    "CMS.MERGE",
    "TOPK.ADD",
    "TOPK.INCRBY",
    "TOPK.RESERVE",
    "TDIGEST.ADD",
    "TDIGEST.CREATE",
    "TDIGEST.RESET",
    "TDIGEST.MERGE",
    "VADD",
    "VREM",
    "VSETATTR",
];

const DESTRUCTIVE: &[(&str, &str)] = &[
    ("FLUSHALL", "Deletes every key in every database."),
    ("FLUSHDB", "Deletes every key in the selected database."),
    ("SWAPDB", "Swaps two whole databases for every client."),
    (
        "EVAL",
        "A Lua script can change or delete any key (e.g. delete by pattern); it cannot be checked beforehand.",
    ),
    (
        "EVALSHA",
        "A Lua script can change or delete any key; it cannot be checked beforehand.",
    ),
    ("FCALL", "A function can change or delete any key; it cannot be checked beforehand."),
    ("FT.DROPINDEX", "Drops a search index (with DD also its documents)."),
    ("FT.DROP", "Drops a search index and its documents."),
    ("MIGRATE", "Moves keys to another server and removes them here."),
];

const ADMIN: &[&str] = &[
    "SHUTDOWN",
    "DEBUG",
    "BGSAVE",
    "SAVE",
    "BGREWRITEAOF",
    "REPLICAOF",
    "SLAVEOF",
    "FAILOVER",
    "RESTORE-ASKING",
    "REPLCONF",
];

const SESSION: &[&str] = &[
    "SELECT",
    "AUTH",
    "HELLO",
    "MULTI",
    "EXEC",
    "DISCARD",
    "WATCH",
    "UNWATCH",
    "RESET",
    "QUIT",
    "ECHO",
    "PING",
    "READONLY",
    "READWRITE",
    "ASKING",
];

const UNSUPPORTED: &[&str] = &[
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "UNSUBSCRIBE",
    "PUNSUBSCRIBE",
    "SUNSUBSCRIBE",
    "MONITOR",
    "SYNC",
    "PSYNC",
];

/// Container commands whose class depends on the subcommand: (command, read-only subcommands, admin subcommands,
/// destructive subcommands). Anything else under them is `Admin`.
const CONTAINERS: &[(&str, &[&str], &[&str])] = &[
    ("CONFIG", &[], &[]),
    (
        "CLIENT",
        &["GETNAME", "ID", "GETREDIR", "TRACKINGINFO", "NO-EVICT", "NO-TOUCH"],
        &[],
    ),
    ("SCRIPT", &["EXISTS"], &["FLUSH"]),
    ("FUNCTION", &["LIST", "DUMP", "STATS"], &["FLUSH", "DELETE", "RESTORE"]),
    ("OBJECT", &["ENCODING", "FREQ", "IDLETIME", "REFCOUNT", "HELP"], &[]),
    ("MEMORY", &["USAGE", "STATS", "DOCTOR", "MALLOC-STATS", "HELP"], &[]),
    ("ACL", &["WHOAMI", "CAT", "GENPASS", "DRYRUN", "HELP"], &[]),
    (
        "CLUSTER",
        &[
            "INFO",
            "NODES",
            "SHARDS",
            "SLOTS",
            "MYID",
            "MYSHARDID",
            "KEYSLOT",
            "COUNTKEYSINSLOT",
            "GETKEYSINSLOT",
            "LINKS",
        ],
        &[],
    ),
    ("XINFO", &["STREAM", "GROUPS", "CONSUMERS", "HELP"], &[]),
    ("XGROUP", &[], &[]),
    ("SLOWLOG", &["LEN", "HELP"], &[]),
    (
        "COMMAND",
        &["COUNT", "DOCS", "GETKEYS", "GETKEYSANDFLAGS", "INFO", "LIST", "HELP"],
        &[],
    ),
    ("LATENCY", &["HELP"], &[]),
    ("MODULE", &["LIST", "HELP"], &[]),
    (
        "PUBSUB",
        &["CHANNELS", "NUMSUB", "NUMPAT", "SHARDCHANNELS", "SHARDNUMSUB", "HELP"],
        &[],
    ),
    (
        "SENTINEL",
        &[
            "MASTERS",
            "MASTER",
            "REPLICAS",
            "SLAVES",
            "SENTINELS",
            "GET-MASTER-ADDR-BY-NAME",
            "CKQUORUM",
            "INFO-CACHE",
        ],
        &[],
    ),
];

/// Container reads that expose secrets or other clients' traffic (password hashes, client addresses and last commands,
/// logged command arguments). They are `Admin` so neither the assistant nor a Read-only workspace runs them.
const SENSITIVE_READS: &[&str] = &[
    "CLIENT LIST",
    "CLIENT INFO",
    "ACL LIST",
    "ACL USERS",
    "ACL GETUSER",
    "ACL LOG",
    "SLOWLOG GET",
];
const CONFIG_GET_REASON: &str = "Shows the server configuration, which includes secrets such as requirepass and masterauth.";
const SENSITIVE_READ_REASON: &str =
    "Shows users, password hashes, other clients or logged commands with their arguments; treated as administration.";

/// Client-side commands that only touch this connection, under CLIENT.
const CLIENT_SESSION: &[&str] = &["SETNAME", "SETINFO", "REPLY", "TRACKING", "CACHING", "NO-EVICT", "NO-TOUCH"];

fn upper(arg: &[u8]) -> String {
    String::from_utf8_lossy(arg).to_ascii_uppercase()
}

/// Classifies one tokenized command (`args[0]` is the command name).
pub fn classify(args: &[Vec<u8>]) -> Classification {
    let Some(first) = args.first() else {
        return Classification {
            class: CommandClass::Session,
            command: String::new(),
            reason: None,
        };
    };
    let name = upper(first);
    let sub = args.get(1).map(|arg| upper(arg));
    let result = |class, command: String, reason: Option<&str>| Classification {
        class,
        command,
        reason: reason.map(str::to_string),
    };

    if let Some((_, reason)) = DESTRUCTIVE.iter().find(|(command, _)| *command == name) {
        return result(CommandClass::Destructive, name, Some(reason));
    }
    // KEYS walks the whole keyspace in one blocking call. A pattern that matches everything is the dangerous form (the key
    // browser uses SCAN); a narrower pattern still scans every key but returns less, so it stays a read.
    if name == "KEYS" && args.get(1).is_none_or(|pattern| pattern.iter().all(|byte| *byte == b'*')) {
        return result(
            CommandClass::Admin,
            name,
            Some("KEYS * blocks the server while it walks every key; use SCAN."),
        );
    }
    if let Some((_, read, destructive)) = CONTAINERS.iter().find(|(command, _, _)| *command == name) {
        let Some(sub) = sub else {
            return result(CommandClass::Read, name, None);
        };
        let full = format!("{name} {sub}");
        if read.contains(&sub.as_str()) {
            return result(CommandClass::Read, full, None);
        }
        if destructive.contains(&sub.as_str()) {
            let class = if name == "SCRIPT" {
                CommandClass::Admin
            } else {
                CommandClass::Destructive
            };
            return result(class, full, Some("Removes server-side scripts or functions."));
        }
        return match name.as_str() {
            "CLIENT" if CLIENT_SESSION.contains(&sub.as_str()) => result(CommandClass::Session, full, None),
            "XGROUP" => result(CommandClass::Write, full, None),
            "OBJECT" | "MEMORY" | "XINFO" | "COMMAND" => result(CommandClass::Read, full, None),
            "CONFIG" if sub == "GET" => result(CommandClass::Admin, full, Some(CONFIG_GET_REASON)),
            "CONFIG" => result(CommandClass::Admin, full, Some("Changes the server configuration.")),
            "LATENCY" if sub != "RESET" => result(
                CommandClass::Admin,
                full,
                Some("Shows server internals (latency events); treated as administration."),
            ),
            _ if SENSITIVE_READS.contains(&full.as_str()) => result(CommandClass::Admin, full, Some(SENSITIVE_READ_REASON)),
            _ => result(CommandClass::Admin, full, Some("Changes server state.")),
        };
    }
    if UNSUPPORTED.contains(&name.as_str()) {
        return result(
            CommandClass::Unsupported,
            name,
            Some("Pub/Sub and MONITOR need a live stream; the console does not support them yet."),
        );
    }
    if SESSION.contains(&name.as_str()) {
        return result(CommandClass::Session, name, None);
    }
    if ADMIN.contains(&name.as_str()) {
        return result(CommandClass::Admin, name, Some("Changes server state."));
    }
    if READ.contains(&name.as_str()) {
        return result(CommandClass::Read, name, None);
    }
    if WRITE.contains(&name.as_str()) {
        // FT.DROPINDEX handled above; DEL of explicit keys is a write, deleting by pattern needs a script (destructive).
        return result(CommandClass::Write, name, None);
    }
    result(
        CommandClass::Unknown,
        name,
        Some("This command is not in the classification table."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_of(line: &str) -> CommandClass {
        let args: Vec<Vec<u8>> = line.split_whitespace().map(|part| part.as_bytes().to_vec()).collect();
        classify(&args).class
    }

    #[test]
    fn classifies_commands() {
        use CommandClass::*;
        let cases = [
            ("get a", Read),
            ("HGETALL h", Read),
            ("scan 0 match users:*", Read),
            ("set a 1", Write),
            ("del a b", Write),
            ("unlink a", Write),
            ("xadd s * f v", Write),
            ("json.set k $ 1", Write),
            ("flushall", Destructive),
            ("FLUSHDB ASYNC", Destructive),
            ("eval \"return 1\" 0", Destructive),
            ("ft.dropindex idx DD", Destructive),
            ("function delete lib", Destructive),
            ("keys user:*", Read),
            ("keys *", Admin),
            ("KEYS **", Admin),
            ("keys", Admin),
            ("config get maxmemory", Admin),
            ("config get requirepass", Admin),
            ("acl list", Admin),
            ("acl users", Admin),
            ("acl getuser default", Admin),
            ("acl log", Admin),
            ("acl cat", Read),
            ("client info", Admin),
            ("client getname", Read),
            ("slowlog get 10", Admin),
            ("slowlog len", Read),
            ("latency latest", Admin),
            ("latency doctor", Admin),
            ("latency history command", Admin),
            ("debug object k", Admin),
            ("config set maxmemory 1", Admin),
            ("script flush", Admin),
            ("script exists abc", Read),
            ("shutdown nosave", Admin),
            ("debug sleep 1", Admin),
            ("acl setuser bob on", Admin),
            ("acl whoami", Read),
            ("client kill id 5", Admin),
            ("client list", Admin),
            ("client setname x", Session),
            ("select 2", Session),
            ("multi", Session),
            ("subscribe ch", Unsupported),
            // MONITOR streams every command of every client: refused outright, which is stricter than Admin.
            ("monitor", Unsupported),
            ("xinfo stream s", Read),
            ("xgroup create s g $", Write),
            ("memory usage k", Read),
            ("frobnicate", Unknown),
        ];
        for (line, expected) in cases {
            assert_eq!(class_of(line), expected, "{line}");
        }
        let config = classify(&[b"config".to_vec(), b"set".to_vec()]);
        assert_eq!(config.command, "CONFIG SET");
        assert!(config.reason.is_some());
    }
}
