//! Saved queries: named query-bar states of an engine workspace (the MongoDB collection workspace first), kept per
//! workspace in `fosta-studio.json` (`Workspace::saved_queries`). A saved query belongs to the workspace of its
//! connection; it stays when that connection is deleted (it is matched by engine and database, not by connection).

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// The most saved queries one workspace keeps.
pub const MAX_SAVED_QUERIES: usize = 1000;
/// The longest name.
pub const MAX_NAME_CHARS: usize = 200;
/// The longest text of one field (filter, sort, one pipeline stage, ...).
pub const MAX_FIELD_CHARS: usize = 64 * 1024;
/// The most stages a saved pipeline keeps.
pub const MAX_PIPELINE_STAGES: usize = 200;

const DEFAULT_LIMIT: u32 = 20;

/// The query bar's fields as typed (mongosh shell text), so a saved query opens exactly as it was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQueryState {
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub collation: String,
    #[serde(default)]
    pub skip: u64,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

impl Default for SavedQueryState {
    fn default() -> Self {
        Self {
            filter: String::new(),
            project: String::new(),
            sort: String::new(),
            collation: String::new(),
            skip: 0,
            limit: DEFAULT_LIMIT,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    /// The engine id (`mongodb`).
    pub engine: String,
    /// The connection it was saved from; informational (the query is offered on every connection of its workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub collection: String,
    #[serde(default)]
    pub query: SavedQueryState,
    /// Aggregation stages as typed, one text per stage (the pipeline builder, Phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<Vec<String>>,
    /// Milliseconds since the epoch of the last save or rename; 0 when unknown.
    #[serde(default)]
    pub updated_at: i64,
}

impl SavedQuery {
    /// Trims the name and ids and refuses what does not fit in the config file.
    pub fn validated(mut self) -> AppResult<Self> {
        self.id = self.id.trim().to_owned();
        self.engine = self.engine.trim().to_owned();
        self.name = valid_name(&self.name)?;
        if self.id.is_empty() {
            return Err(AppError::new("A saved query needs an id"));
        }
        if self.engine.is_empty() {
            return Err(AppError::new("A saved query needs an engine"));
        }
        self.connection_id = self
            .connection_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let query = &self.query;
        let mut fields = vec![
            &self.database,
            &self.collection,
            &query.filter,
            &query.project,
            &query.sort,
            &query.collation,
        ];
        if let Some(stages) = &self.pipeline {
            if stages.len() > MAX_PIPELINE_STAGES {
                return Err(AppError::new(format!(
                    "A saved pipeline keeps at most {MAX_PIPELINE_STAGES} stages"
                )));
            }
            fields.extend(stages);
        }
        if fields.iter().any(|text| text.chars().count() > MAX_FIELD_CHARS) {
            return Err(AppError::new(format!(
                "A field of a saved query is longer than {MAX_FIELD_CHARS} characters"
            )));
        }
        if self.query.limit == 0 {
            self.query.limit = DEFAULT_LIMIT;
        }
        Ok(self)
    }
}

/// A trimmed, non-empty name of at most [`MAX_NAME_CHARS`] characters.
pub fn valid_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::new("A saved query needs a name"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::new(format!("A saved query name is at most {MAX_NAME_CHARS} characters")));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> SavedQuery {
        SavedQuery {
            id: " q1 ".into(),
            name: "  Paid orders ".into(),
            engine: "mongodb".into(),
            connection_id: Some(" ".into()),
            database: "shop".into(),
            collection: "orders".into(),
            query: SavedQueryState {
                filter: "{ status: 'paid' }".into(),
                limit: 0,
                ..Default::default()
            },
            pipeline: None,
            updated_at: 0,
        }
    }

    #[test]
    fn validation_trims_and_defaults() {
        let saved = query().validated().unwrap();
        assert_eq!((saved.id.as_str(), saved.name.as_str()), ("q1", "Paid orders"));
        assert_eq!(saved.connection_id, None, "a blank connection id is no connection");
        assert_eq!(saved.query.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn validation_refuses_what_does_not_fit() {
        let blank = SavedQuery {
            name: " ".into(),
            ..query()
        };
        assert!(blank.validated().is_err());
        let long_name = SavedQuery {
            name: "x".repeat(MAX_NAME_CHARS + 1),
            ..query()
        };
        assert!(long_name.validated().is_err());
        let mut long_filter = query();
        long_filter.query.filter = "x".repeat(MAX_FIELD_CHARS + 1);
        assert!(long_filter.validated().is_err());
        let stages = SavedQuery {
            pipeline: Some(vec!["{ $match: {} }".into(); MAX_PIPELINE_STAGES + 1]),
            ..query()
        };
        assert!(stages.validated().is_err());
        let no_id = SavedQuery { id: "".into(), ..query() };
        assert!(no_id.validated().is_err());
    }

    #[test]
    fn a_minimal_entry_reads_with_defaults() {
        let saved: SavedQuery = serde_json::from_str(r#"{ "id": "a", "name": "A", "engine": "mongodb" }"#).unwrap();
        assert_eq!(saved.query, SavedQueryState::default());
        assert_eq!(saved.pipeline, None);
        let written = serde_json::to_value(&saved).unwrap();
        assert!(written.get("pipeline").is_none() && written.get("connectionId").is_none());
        assert_eq!(written["query"]["limit"], 20);
    }
}
