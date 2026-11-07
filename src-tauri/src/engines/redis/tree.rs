//! Prefix tree for the key browser: keys are split on a delimiter (`:` by default) into folders. Built incrementally as
//! SCAN pages arrive; the UI asks for the children of one folder at a time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{scan::KeySummary, types::Blob, types::RedisType};

pub const DEFAULT_DELIMITER: &str = ":";

#[derive(Clone, Debug, Default)]
pub struct PrefixTree {
    delimiter: String,
    root: Node,
    keys: usize,
}

#[derive(Clone, Debug, Default)]
struct Node {
    children: BTreeMap<String, Node>,
    /// Set when a key ends exactly here (a key can also be a folder: `users` and `users:1`).
    leaf: Option<Leaf>,
    /// Keys at or below this node.
    count: usize,
}

#[derive(Clone, Debug)]
struct Leaf {
    key: Blob,
    key_type: Option<RedisType>,
    ttl_ms: Option<i64>,
}

/// One row under a folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    /// The segment shown in the tree.
    pub name: String,
    /// The folder path including this segment and a trailing delimiter (`users:`), usable as a SCAN pattern prefix.
    pub prefix: String,
    /// Keys at or below this entry.
    pub key_count: usize,
    /// Present when a key ends at this entry.
    pub key: Option<Blob>,
    pub key_type: Option<RedisType>,
    pub ttl_ms: Option<i64>,
    pub has_children: bool,
}

impl PrefixTree {
    pub fn new(delimiter: &str) -> Self {
        let delimiter = if delimiter.is_empty() { DEFAULT_DELIMITER } else { delimiter };
        Self {
            delimiter: delimiter.to_string(),
            ..Default::default()
        }
    }

    pub fn delimiter(&self) -> &str {
        &self.delimiter
    }

    /// Number of distinct keys inserted.
    pub fn len(&self) -> usize {
        self.keys
    }

    pub fn is_empty(&self) -> bool {
        self.keys == 0
    }

    /// Adds keys from a SCAN page. Inserting the same key again only refreshes its type and TTL.
    pub fn extend(&mut self, keys: impl IntoIterator<Item = KeySummary>) {
        for summary in keys {
            self.insert(summary.key, summary.key_type, summary.ttl_ms);
        }
    }

    pub fn insert(&mut self, key: Blob, key_type: Option<RedisType>, ttl_ms: Option<i64>) {
        let text = key.display();
        let segments: Vec<&str> = text.split(self.delimiter.as_str()).collect();
        // Walk once to see whether the key is new, so counts are not inflated by repeated pages.
        let exists = {
            let mut node = &self.root;
            let mut found = true;
            for segment in &segments {
                match node.children.get(*segment) {
                    Some(child) => node = child,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            found && node.leaf.as_ref().is_some_and(|leaf| leaf.key == key)
        };
        let mut node = &mut self.root;
        if !exists {
            node.count += 1;
        }
        for segment in &segments {
            node = node.children.entry((*segment).to_string()).or_default();
            if !exists {
                node.count += 1;
            }
        }
        node.leaf = Some(Leaf { key, key_type, ttl_ms });
        if !exists {
            self.keys += 1;
        }
    }

    /// Removes a key (after a delete or rename); empty folders disappear.
    pub fn remove(&mut self, key: &Blob) -> bool {
        let text = key.display();
        let segments: Vec<String> = text.split(self.delimiter.as_str()).map(str::to_string).collect();
        if remove_path(&mut self.root, &segments, key) {
            self.root.count -= 1;
            self.keys -= 1;
            true
        } else {
            false
        }
    }

    /// Children of a folder. `prefix` is `""` for the root or a folder prefix as returned in [`TreeEntry::prefix`]
    /// (`users:` or `users:eu:`). Folders come first, then keys, each sorted by name.
    pub fn children(&self, prefix: &str) -> Vec<TreeEntry> {
        let path = prefix.strip_suffix(self.delimiter.as_str()).unwrap_or(prefix);
        let mut node = &self.root;
        if !prefix.is_empty() {
            for segment in path.split(self.delimiter.as_str()) {
                match node.children.get(segment) {
                    Some(child) => node = child,
                    None => return Vec::new(),
                }
            }
        }
        let mut entries: Vec<TreeEntry> = node
            .children
            .iter()
            .map(|(name, child)| TreeEntry {
                name: name.clone(),
                prefix: format!("{prefix}{name}{}", self.delimiter),
                key_count: child.count,
                key: child.leaf.as_ref().map(|leaf| leaf.key.clone()),
                key_type: child.leaf.as_ref().and_then(|leaf| leaf.key_type.clone()),
                ttl_ms: child.leaf.as_ref().and_then(|leaf| leaf.ttl_ms),
                has_children: !child.children.is_empty(),
            })
            .collect();
        entries.sort_by(|a, b| b.has_children.cmp(&a.has_children).then_with(|| a.name.cmp(&b.name)));
        entries
    }
}

fn remove_path(node: &mut Node, segments: &[String], key: &Blob) -> bool {
    let Some((first, rest)) = segments.split_first() else {
        if node.leaf.as_ref().is_some_and(|leaf| &leaf.key == key) {
            node.leaf = None;
            return true;
        }
        return false;
    };
    let Some(child) = node.children.get_mut(first) else {
        return false;
    };
    if !remove_path(child, rest, key) {
        return false;
    }
    child.count -= 1;
    if child.count == 0 {
        node.children.remove(first);
    }
    true
}

/// Escapes glob characters so a folder prefix can be used in `SCAN MATCH <prefix>*`.
pub fn glob_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '*' | '?' | '[' | ']' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(key: &str) -> KeySummary {
        KeySummary {
            key: Blob::text(key),
            key_type: Some(RedisType::String),
            ttl_ms: Some(-1),
        }
    }

    #[test]
    fn builds_folders_incrementally() {
        let mut tree = PrefixTree::new(":");
        tree.extend(["users:1", "users:2", "users:eu:3"].map(summary));
        tree.extend(["sessions:a", "leaderboard", "users"].map(summary));
        // A repeated page does not change the counts.
        tree.extend(["users:1"].map(summary));
        assert_eq!(tree.len(), 6);

        let root = tree.children("");
        let names: Vec<_> = root
            .iter()
            .map(|entry| (entry.name.as_str(), entry.key_count, entry.key.is_some()))
            .collect();
        assert_eq!(names, vec![("sessions", 1, false), ("users", 4, true), ("leaderboard", 1, true)]);

        let users = tree.children("users:");
        let names: Vec<_> = users
            .iter()
            .map(|entry| (entry.name.as_str(), entry.prefix.as_str(), entry.key_count))
            .collect();
        assert_eq!(names, vec![("eu", "users:eu:", 1), ("1", "users:1:", 1), ("2", "users:2:", 1)]);
        assert_eq!(tree.children("users:eu:")[0].key, Some(Blob::text("users:eu:3")));
        assert!(tree.children("nope:").is_empty());
    }

    #[test]
    fn removes_keys_and_empty_folders() {
        let mut tree = PrefixTree::new("::");
        tree.extend(["a::b::c", "a::d"].map(summary));
        assert_eq!(tree.children("")[0].key_count, 2);
        assert!(tree.remove(&Blob::text("a::b::c")));
        assert!(!tree.remove(&Blob::text("a::b::c")));
        let a = tree.children("a::");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "d");
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn handles_empty_segments_and_escaping() {
        let mut tree = PrefixTree::new("");
        assert_eq!(tree.delimiter(), ":");
        tree.extend(["a::b", ":x"].map(summary));
        let root = tree.children("");
        assert_eq!(root.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), vec!["", "a"]);
        assert_eq!(tree.children("a:")[0].name, "");
        assert_eq!(glob_escape("a*b?[c]\\"), "a\\*b\\?\\[c\\]\\\\");
    }
}
