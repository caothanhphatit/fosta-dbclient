//! What Redis adds to the assistant's learned profile (`ProfileExtras`): key patterns and types, never values. One
//! bounded `redis.scan` of key names (with their types and TTLs, no value read) is grouped into patterns such as
//! `user:{id}:profile`: segments that look like numbers, UUIDs, hashes, emails, dates or mixed identifiers become
//! placeholders, and a segment whose spellings are mostly seen once (`user:alice:profile`, `user:bob:profile`…)
//! becomes `{name}`. Each pattern is a profile object of kind `keyPattern` with its key count and types in the sample.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value as Json};

use crate::contract::{ExtrasRead, ObjectExtras, ObjectRef, ProfileEnrichment, KEY_PATTERN};

/// Keys read in the sample (one SCAN page, bounded in time by the scan itself), and the patterns kept.
pub const SAMPLE_KEYS: u64 = 5_000;
pub const MAX_PATTERNS: usize = 200;
const DELIMITER: char = ':';
/// Spellings of one segment, in one group of otherwise equal keys, before it becomes `{name}`.
const MIN_VARIANTS: usize = 3;

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 8
        && (bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit)
            || bytes.len() >= 10
                && bytes[..10].iter().enumerate().all(|(index, b)| match index {
                    4 | 7 => *b == b'-',
                    _ => b.is_ascii_digit(),
                }))
}

/// The placeholder a segment stands for, or none for a word kept as it is.
pub fn placeholder(segment: &str) -> Option<&'static str> {
    if segment.is_empty() {
        return None;
    }
    if is_uuid(segment) {
        return Some("{uuid}");
    }
    if is_date(segment) && segment.len() >= 10 {
        return Some("{date}");
    }
    if segment.chars().all(|c| c.is_ascii_digit()) {
        return Some("{id}");
    }
    if segment.contains('@') {
        return Some("{email}");
    }
    let digits = segment.chars().filter(char::is_ascii_digit).count();
    if segment.len() >= 12 && digits > 0 && segment.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some("{hash}");
    }
    let letters = segment.chars().filter(char::is_ascii_alphabetic).count();
    if segment.len() >= 6
        && digits >= 2
        && letters > 0
        && segment.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Some("{id}");
    }
    None
}

/// One pattern: how many sampled keys follow it, by type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPattern {
    pub pattern: String,
    pub count: usize,
    pub types: BTreeMap<String, usize>,
    /// Keys with a time to live.
    pub expiring: usize,
}

/// A sampled key: its name (none when it is not text), type and whether it expires.
pub struct SampledKey {
    pub name: Option<String>,
    pub key_type: String,
    pub expiring: bool,
}

/// Groups `keys` into patterns, most keys first, at most [`MAX_PATTERNS`] (the rest are counted under `…`).
pub fn key_patterns(keys: &[SampledKey]) -> Vec<KeyPattern> {
    let mut tokens: Vec<Vec<String>> = keys
        .iter()
        .map(|key| match &key.name {
            Some(name) => name
                .split(DELIMITER)
                .map(|segment| placeholder(segment).map_or_else(|| segment.to_owned(), str::to_owned))
                .collect(),
            None => vec!["{binary}".to_owned()],
        })
        .collect();
    let longest = tokens.iter().map(Vec::len).max().unwrap_or(0);
    for position in 0..longest {
        // Keys equal but for this segment; a segment whose spellings are mostly seen once is a name.
        let mut groups: HashMap<(usize, Vec<&str>), HashMap<&str, usize>> = HashMap::new();
        for key in &tokens {
            if key.len() < 2 || position >= key.len() || key[position].starts_with('{') {
                continue;
            }
            let rest: Vec<&str> = key
                .iter()
                .enumerate()
                .map(|(index, token)| if index == position { "" } else { token.as_str() })
                .collect();
            *groups
                .entry((key.len(), rest))
                .or_default()
                .entry(key[position].as_str())
                .or_default() += 1;
        }
        let collapsed: Vec<(usize, Vec<String>)> = groups
            .into_iter()
            .filter(|(_, spellings)| {
                let once = spellings.values().filter(|count| **count == 1).count();
                spellings.len() >= MIN_VARIANTS && once * 2 >= spellings.len()
            })
            .map(|((length, rest), _)| (length, rest.into_iter().map(str::to_owned).collect()))
            .collect();
        if collapsed.is_empty() {
            continue;
        }
        for key in &mut tokens {
            if key.len() < 2 || position >= key.len() || key[position].starts_with('{') {
                continue;
            }
            let matches = collapsed.iter().any(|(length, rest)| {
                *length == key.len()
                    && rest
                        .iter()
                        .zip(key.iter())
                        .enumerate()
                        .all(|(index, (expected, token))| index == position || expected == token)
            });
            if matches {
                key[position] = "{name}".to_owned();
            }
        }
    }
    let mut patterns: HashMap<String, KeyPattern> = HashMap::new();
    for (key, token) in keys.iter().zip(tokens) {
        let pattern = token.join(&DELIMITER.to_string());
        let entry = patterns.entry(pattern.clone()).or_insert_with(|| KeyPattern {
            pattern,
            count: 0,
            types: BTreeMap::new(),
            expiring: 0,
        });
        entry.count += 1;
        *entry.types.entry(key.key_type.clone()).or_default() += 1;
        entry.expiring += usize::from(key.expiring);
    }
    let mut patterns: Vec<KeyPattern> = patterns.into_values().collect();
    patterns.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.pattern.cmp(&b.pattern)));
    if patterns.len() > MAX_PATTERNS {
        let rest = patterns.split_off(MAX_PATTERNS - 1);
        let mut other = KeyPattern {
            pattern: "…".to_owned(),
            count: 0,
            types: BTreeMap::new(),
            expiring: 0,
        };
        for pattern in rest {
            other.count += pattern.count;
            other.expiring += pattern.expiring;
            for (key_type, count) in pattern.types {
                *other.types.entry(key_type).or_default() += count;
            }
        }
        patterns.push(other);
    }
    patterns
}

/// The database number of a profile's database (`db0`, `0`).
fn number_of(database: &str) -> Option<i64> {
    database.trim_start_matches("db").parse().ok().filter(|number| *number >= 0)
}

/// One SCAN page of key names with their types; no value is read.
pub fn reads(database: &str) -> Vec<ExtrasRead> {
    let Some(db) = number_of(database) else {
        return Vec::new();
    };
    vec![ExtrasRead {
        object: None,
        name: "redis.scan".into(),
        args: json!({ "db": db, "cursor": "0", "count": 1000, "pageSize": SAMPLE_KEYS, "withMeta": true }),
    }]
}

fn sampled_key(row: &Json) -> SampledKey {
    let key_type = match &row["keyType"] {
        Json::String(name) => name.clone(),
        Json::Object(other) => other.values().next().and_then(Json::as_str).unwrap_or("other").to_owned(),
        _ => "unknown".to_owned(),
    };
    SampledKey {
        name: row["key"]["text"].as_str().map(str::to_owned),
        key_type,
        expiring: row["ttlMs"].as_i64().is_some_and(|ttl| ttl >= 0),
    }
}

fn types_text(types: &BTreeMap<String, usize>) -> String {
    let mut listed: Vec<(&String, &usize)> = types.iter().collect();
    listed.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    listed
        .into_iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn interpret(database: &str, answers: &[(ExtrasRead, Json)]) -> ProfileEnrichment {
    let mut enrichment = ProfileEnrichment::default();
    let Some((_, answer)) = answers.iter().find(|(read, _)| read.name == "redis.scan") else {
        return enrichment;
    };
    let keys: Vec<SampledKey> = answer["keys"].as_array().into_iter().flatten().map(sampled_key).collect();
    let patterns = key_patterns(&keys);
    let database_ref = (!database.is_empty()).then_some(database);
    for pattern in &patterns {
        let mut extras = ObjectExtras::new(ObjectRef::new(database_ref, None, &pattern.pattern, KEY_PATTERN));
        let types: Vec<&str> = pattern.types.keys().map(String::as_str).collect();
        extras.fingerprint_basis = Some(format!("{}\n{}", pattern.pattern, types.join(",")));
        extras.facts = vec![
            ("Types".into(), types_text(&pattern.types)),
            ("Keys in the sample".into(), pattern.count.to_string()),
        ];
        if pattern.expiring > 0 {
            extras
                .facts
                .push(("Expiring keys".into(), format!("{} of {}", pattern.expiring, pattern.count)));
        }
        enrichment.objects.push(extras);
    }
    enrichment.extras.insert(
        "keyPatterns".into(),
        json!({
            "sampledKeys": keys.len(),
            "databaseKeys": answer["dbSize"],
            "complete": answer["done"].as_bool().unwrap_or(false),
            "patterns": patterns.len(),
        }),
    );
    if answer["done"].as_bool() == Some(false) {
        enrichment.note = Some(format!("Key patterns come from the first {} keys of a SCAN", keys.len()));
    }
    enrichment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, key_type: &str) -> SampledKey {
        SampledKey {
            name: Some(name.into()),
            key_type: key_type.into(),
            expiring: false,
        }
    }

    fn shown(patterns: &[KeyPattern]) -> Vec<(&str, usize)> {
        patterns.iter().map(|pattern| (pattern.pattern.as_str(), pattern.count)).collect()
    }

    #[test]
    fn segments_become_placeholders() {
        assert_eq!(placeholder("42"), Some("{id}"));
        assert_eq!(placeholder("a2b3c4d5-e6f7-4890-abcd-ef0123456789"), Some("{uuid}"));
        assert_eq!(placeholder("5f2b9c0e1d2a3b4c"), Some("{hash}"));
        assert_eq!(placeholder("ann@example.com"), Some("{email}"));
        assert_eq!(placeholder("2026-11-12"), Some("{date}"));
        assert_eq!(placeholder("ord_8x2k99"), Some("{id}"));
        assert_eq!(placeholder("profile"), None);
        assert_eq!(placeholder("v2"), None);
        assert_eq!(placeholder(""), None);
    }

    #[test]
    fn keys_group_into_patterns_with_counts_and_types() {
        let mut keys: Vec<SampledKey> = (0..5).map(|id| key(&format!("user:{id}:profile"), "hash")).collect();
        keys.extend((0..3).map(|id| key(&format!("user:{id}:cart"), "list")));
        keys.extend(["alice", "bob", "carol", "dave"].map(|name| key(&format!("session:{name}:token"), "string")));
        keys.push(key("config", "string"));
        keys.push(key("counter", "string"));
        keys.push(key("cache:users", "string"));
        keys.push(key("cache:orders", "zset"));
        keys.push(SampledKey {
            name: None,
            key_type: "string".into(),
            expiring: true,
        });
        let patterns = key_patterns(&keys);
        assert_eq!(
            shown(&patterns),
            [
                ("user:{id}:profile", 5),
                ("session:{name}:token", 4),
                ("user:{id}:cart", 3),
                ("cache:orders", 1),
                ("cache:users", 1),
                ("config", 1),
                ("counter", 1),
                ("{binary}", 1),
            ]
        );
        assert_eq!(patterns[0].types, BTreeMap::from([("hash".to_owned(), 5)]));
        assert_eq!(patterns[7].expiring, 1);
        // Namespaces seen with many keys each are not names.
        let spread: Vec<SampledKey> = ["user", "order", "item"]
            .iter()
            .flat_map(|space| (0..4).map(move |id| key(&format!("{space}:{id}"), "string")))
            .collect();
        assert_eq!(
            shown(&key_patterns(&spread)),
            [("item:{id}", 4), ("order:{id}", 4), ("user:{id}", 4)]
        );
    }

    #[test]
    fn patterns_are_capped_and_the_scan_answer_is_read() {
        let keys: Vec<SampledKey> = (0..(MAX_PATTERNS + 50))
            .map(|index| {
                let letters: String = index.to_string().bytes().map(|digit| char::from(b'a' + digit - b'0')).collect();
                key(&format!("k{letters}"), "string")
            })
            .collect();
        let many = key_patterns(&keys);
        assert_eq!(many.len(), MAX_PATTERNS);
        let rest = many.last().unwrap();
        assert_eq!((rest.pattern.as_str(), rest.count), ("…", 51));
        let answer = json!({
            "cursor": "17", "done": false, "calls": 2, "dbSize": 900,
            "keys": [
                { "key": { "text": "user:1:profile" }, "keyType": "hash", "ttlMs": -1 },
                { "key": { "text": "user:2:profile" }, "keyType": "hash", "ttlMs": 5000 },
                { "key": { "base64": "AAE=" }, "keyType": { "other": "MBbloom--" }, "ttlMs": -1 },
            ],
        });
        let read = reads("db0").remove(0);
        assert_eq!((read.name.as_str(), read.args["db"].as_i64()), ("redis.scan", Some(0)));
        let enrichment = interpret("db0", &[(read, answer)]);
        let names: Vec<&str> = enrichment.objects.iter().map(|object| object.object.name.as_str()).collect();
        assert_eq!(names, ["user:{id}:profile", "{binary}"]);
        assert!(enrichment.objects.iter().all(|object| object.object.kind == KEY_PATTERN));
        assert!(enrichment.objects[0]
            .facts
            .contains(&("Expiring keys".to_owned(), "1 of 2".to_owned())));
        assert_eq!(enrichment.objects[1].facts[0].1, "MBbloom-- 1");
        assert_eq!(enrichment.extras["keyPatterns"]["databaseKeys"], 900);
        assert!(enrichment.note.is_some());
        assert!(reads("not a db").is_empty());
    }
}
