//! What an engine (statically, in its descriptor) and a connection (for one server) can do. The UI shows only what is
//! advertised; the core refuses the rest with `UNSUPPORTED_CAPABILITY`.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum Capability {
    /// `children` returns an object tree.
    Tree,
    Describe,
    /// The server holds several databases the tree lists.
    MultipleDatabases,
    /// Objects live in schemas inside a database.
    Schemas,
    /// `tabular().browse`.
    BrowseRows,
    /// `tabular().apply`, with optimistic concurrency checks.
    EditRows,
    /// `Filter::Raw` in the engine's filter language.
    RawFilter,
    KeysetPaging,
    EstimatedCount,
    ExactCount,
    /// Sessions run statements.
    Query,
    Cancel,
    Transactions,
    ReadOnlySession,
    StatementTimeout,
    StreamingResults,
    SchemaGraph,
    CreateDatabase,
    Explain,
    Documents,
    KeyValue,
    Search,
    Console,
    ServerOverview,
    Vector,
}

impl Capability {
    pub const ALL: [Capability; 25] = [
        Capability::Tree,
        Capability::Describe,
        Capability::MultipleDatabases,
        Capability::Schemas,
        Capability::BrowseRows,
        Capability::EditRows,
        Capability::RawFilter,
        Capability::KeysetPaging,
        Capability::EstimatedCount,
        Capability::ExactCount,
        Capability::Query,
        Capability::Cancel,
        Capability::Transactions,
        Capability::ReadOnlySession,
        Capability::StatementTimeout,
        Capability::StreamingResults,
        Capability::SchemaGraph,
        Capability::CreateDatabase,
        Capability::Explain,
        Capability::Documents,
        Capability::KeyValue,
        Capability::Search,
        Capability::Console,
        Capability::ServerOverview,
        Capability::Vector,
    ];

    const fn bit(self) -> u64 {
        1 << (self as u8)
    }

    pub fn name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// A set of capabilities; serialises as `["tree", "describe", …]` in declaration order.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CapabilitySet(u64);

impl CapabilitySet {
    pub const fn of(caps: &[Capability]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < caps.len() {
            bits |= caps[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub fn contains(self, c: Capability) -> bool {
        self.0 & c.bit() != 0
    }

    pub fn with(self, c: Capability) -> Self {
        Self(self.0 | c.bit())
    }

    pub fn without(self, c: Capability) -> Self {
        Self(self.0 & !c.bit())
    }

    /// The capabilities in both sets.
    pub fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL.into_iter().filter(move |c| self.contains(*c))
    }
}

impl std::fmt::Debug for CapabilitySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl Serialize for CapabilitySet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

/// Unknown names (from a newer build) are ignored rather than rejected.
impl<'de> Deserialize<'de> for CapabilitySet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let names = Vec::<serde_json::Value>::deserialize(deserializer)?;
        let mut set = CapabilitySet::default();
        for name in names {
            if !name.is_string() {
                return Err(de::Error::custom("capabilities are names"));
            }
            if let Ok(c) = serde_json::from_value::<Capability>(name) {
                set = set.with(c);
            }
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_serialise_as_names_and_ignore_unknown_ones() {
        let set = CapabilitySet::of(&[Capability::Tree, Capability::EditRows]);
        assert_eq!(serde_json::to_string(&set).unwrap(), r#"["tree","editRows"]"#);
        let back: CapabilitySet = serde_json::from_str(r#"["editRows","tree","teleport"]"#).unwrap();
        assert_eq!(back, set);
        assert!(set.with(Capability::Vector).contains(Capability::Vector));
        assert!(!set.without(Capability::Tree).contains(Capability::Tree));
        assert!(set.is_subset_of(set.with(Capability::Query)));
        assert_eq!(Capability::ALL.len(), Capability::Vector as usize + 1);
    }
}
