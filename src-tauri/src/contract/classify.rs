//! Statement classification shared by every engine's classifier and the statement gateway: what an operation can do,
//! from harmless to dangerous. Each engine classifies its own languages, row changes and capability calls
//! (`Engine::classify`); the gateway applies the workspace's rules per [`Category`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{session::TransactionOp, tabular::RowChangeSet};

const EXCERPT_CHARS: usize = 240;

/// What a statement can do, from harmless to dangerous. `Ord` follows that severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// SELECT, SHOW, EXPLAIN, USE, session SET, MongoDB find/aggregate/count…
    Read,
    /// BEGIN / COMMIT / ROLLBACK / SAVEPOINT / autocommit: always allowed.
    Transaction,
    /// INSERT, UPDATE, DELETE (with WHERE), MERGE, REPLACE, procedure calls.
    Write,
    /// SQL that could not be parsed; treated like a write.
    Unknown,
    /// CREATE / ALTER / RENAME (without dropping anything).
    Ddl,
    /// DROP, TRUNCATE, UPDATE/DELETE without WHERE, ALTER … DROP.
    Destructive,
    /// Users, grants, KILL, server-wide settings.
    Admin,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Read,
        Category::Transaction,
        Category::Write,
        Category::Unknown,
        Category::Ddl,
        Category::Destructive,
        Category::Admin,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Read => "Read",
            Category::Transaction => "Transaction",
            Category::Write => "Write",
            Category::Unknown => "Unparsed",
            Category::Ddl => "DDL",
            Category::Destructive => "Destructive",
            Category::Admin => "Administrative",
        }
    }
}

/// One classified statement, as shown in the confirmation dialog and written to the audit log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementInfo {
    pub category: Category,
    /// Short statement kind, e.g. `DELETE`, `DROP TABLE`, `find`.
    pub kind: String,
    /// Tables or collections it touches, as written.
    pub tables: Vec<String>,
    pub excerpt: String,
    /// Why it got this category when that is not obvious ("DELETE without WHERE").
    pub reason: Option<String>,
    /// A hard block whatever the workspace's preset (Redis `SUBSCRIBE` in a console, an unknown capability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl StatementInfo {
    pub fn new(category: Category, kind: &str, excerpt: &str) -> Self {
        Self {
            category,
            kind: kind.to_owned(),
            tables: Vec::new(),
            excerpt: excerpt_of(excerpt),
            reason: None,
            refused: None,
        }
    }

    pub fn with_tables(mut self, tables: Vec<String>) -> Self {
        self.tables = tables;
        self
    }

    pub fn with_reason(mut self, reason: &str) -> Self {
        self.reason = Some(reason.to_owned());
        self
    }

    /// A statement the gateway refuses whatever the rules say.
    pub fn refused(kind: &str, excerpt: &str, why: &str) -> Self {
        Self {
            refused: Some(why.to_owned()),
            ..Self::new(Category::Admin, kind, excerpt)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub statements: Vec<StatementInfo>,
}

impl Classification {
    pub fn single(info: StatementInfo) -> Self {
        Self { statements: vec![info] }
    }

    /// Distinct categories, most severe last.
    pub fn categories(&self) -> Vec<Category> {
        let set: BTreeSet<Category> = self.statements.iter().map(|statement| statement.category).collect();
        set.into_iter().collect()
    }

    pub fn tables(&self) -> Vec<String> {
        let mut tables: Vec<String> = Vec::new();
        for table in self.statements.iter().flat_map(|statement| &statement.tables) {
            if !tables.contains(table) {
                tables.push(table.clone());
            }
        }
        tables
    }

    pub fn worst(&self) -> Option<Category> {
        self.statements.iter().map(|statement| statement.category).max()
    }

    /// The first hard refusal, if any.
    pub fn refusal(&self) -> Option<&str> {
        self.statements.iter().find_map(|statement| statement.refused.as_deref())
    }
}

/// What the gateway asks an engine to classify.
#[derive(Clone, Copy, Debug)]
pub enum ClassifyInput<'a> {
    /// Text a session runs, in one of the engine's languages.
    Statement {
        language: &'a str,
        text: &'a str,
    },
    /// Grid saves and imports.
    Changes(&'a RowChangeSet),
    /// An engine-specific operation (`Connection::invoke`).
    Capability {
        name: &'a str,
        args: &'a serde_json::Value,
    },
    Transaction(TransactionOp),
    CreateDatabase {
        name: &'a str,
    },
}

/// Grid saves and imports: inserts and updates are writes, and so are deletes that name their rows by key. A change
/// without a key would touch every row, so it is destructive.
pub fn classify_changes(set: &RowChangeSet) -> Classification {
    use super::tabular::RowChange;
    let table = set.object.display_name();
    let mut statements = Vec::new();
    for change in &set.changes {
        let (kind, keyed) = match change {
            RowChange::Insert { .. } => ("INSERT", true),
            RowChange::Update { key, .. } => ("UPDATE", !key.is_empty()),
            RowChange::Delete { key, .. } => ("DELETE", !key.is_empty()),
        };
        if statements
            .iter()
            .any(|s: &StatementInfo| s.kind == kind && (s.category == Category::Write) == keyed)
        {
            continue;
        }
        let mut info = StatementInfo::new(
            if keyed { Category::Write } else { Category::Destructive },
            kind,
            &format!("{kind} {table} (grid)"),
        )
        .with_tables(vec![set.object.name.clone()]);
        if !keyed {
            info.reason = Some(format!("{kind} without a row key"));
        }
        statements.push(info);
    }
    Classification { statements }
}

/// The fixed classification of "New database…".
pub fn classify_create_database(name: &str) -> Classification {
    Classification::single(StatementInfo::new(
        Category::Ddl,
        "CREATE DATABASE",
        &format!("CREATE DATABASE {name}"),
    ))
}

/// Begin / Commit / … from the toolbar.
pub fn classify_transaction(op: TransactionOp) -> Classification {
    let kind = format!("{op:?}").to_uppercase();
    Classification::single(StatementInfo::new(Category::Transaction, &kind, &format!("{op:?}")))
}

/// Statement text shortened for dialogs and the audit log.
pub fn excerpt_of(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= EXCERPT_CHARS {
        collapsed
    } else {
        let mut cut: String = collapsed.chars().take(EXCERPT_CHARS).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        object::ObjectRef,
        tabular::{Field, RowChange},
        value::Value,
    };

    #[test]
    fn keyed_changes_are_writes_and_keyless_ones_destructive() {
        let key = vec![Field::new("id", Value::Int(1))];
        let set = RowChangeSet {
            object: ObjectRef::table("shop", None, "orders"),
            changes: vec![
                RowChange::Insert { values: key.clone() },
                RowChange::Delete {
                    key: key.clone(),
                    original: vec![],
                    version: None,
                    force: false,
                },
                RowChange::Delete {
                    key: vec![],
                    original: vec![],
                    version: None,
                    force: false,
                },
            ],
        };
        let classification = classify_changes(&set);
        assert_eq!(classification.worst(), Some(Category::Destructive));
        assert_eq!(classification.tables(), vec!["orders".to_owned()]);
        assert_eq!(classification.statements.len(), 3);
    }

    #[test]
    fn refusals_are_found_and_serialised_only_when_set() {
        let plain = StatementInfo::new(Category::Read, "GET", "GET k");
        assert!(serde_json::to_value(&plain).unwrap().get("refused").is_none());
        let classification = Classification {
            statements: vec![
                plain,
                StatementInfo::refused("SUBSCRIBE", "SUBSCRIBE ch", "Pub/sub is not supported"),
            ],
        };
        assert_eq!(classification.refusal(), Some("Pub/sub is not supported"));
        assert_eq!(excerpt_of("a\n   b"), "a b");
    }
}
