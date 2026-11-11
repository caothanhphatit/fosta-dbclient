//! Saved queries of the engine workspaces (`crate::saved_queries`), kept per workspace in the config file. Each
//! change goes through `ConfigStore::update` (written atomically) and answers with the saved queries of the workspace
//! it touched.

use crate::{
    audit::now_millis,
    error::{AppError, AppResult},
    saved_queries::SavedQuery,
    AppState,
};

/// The saved queries of the workspace holding `connection_id` (else `workspace_id`, else the active workspace).
pub fn list_saved_queries(state: &AppState, connection_id: Option<String>, workspace_id: Option<String>) -> AppResult<Vec<SavedQuery>> {
    state.config.read(|config| {
        config
            .saved_query_scope(connection_id.as_deref(), workspace_id.as_deref())
            .map(|workspace| workspace.saved_queries.clone())
    })
}

/// Adds or replaces a saved query (same id) in the workspace of its connection (else `workspace_id`, else the active
/// workspace).
pub fn save_saved_query(state: &AppState, query: SavedQuery, workspace_id: Option<String>) -> AppResult<Vec<SavedQuery>> {
    let mut query = query;
    query.updated_at = now_millis();
    let mut target = String::new();
    let config = state.config.update(|config| {
        target = config
            .saved_query_scope(query.connection_id.as_deref(), workspace_id.as_deref())?
            .id
            .clone();
        config.put_saved_query(&target, query)
    })?;
    Ok(config.workspace(&target)?.saved_queries.clone())
}

pub fn rename_saved_query(state: &AppState, id: String, name: String) -> AppResult<Vec<SavedQuery>> {
    let mut target = String::new();
    let config = state.config.update(|config| {
        target = config.rename_saved_query(&id, &name, now_millis())?;
        Ok(())
    })?;
    Ok(config.workspace(&target)?.saved_queries.clone())
}

pub fn delete_saved_query(state: &AppState, id: String) -> AppResult<Vec<SavedQuery>> {
    let mut target = None;
    let config = state.config.update(|config| {
        target = config.remove_saved_query(&id);
        target
            .as_ref()
            .map(|_| ())
            .ok_or_else(|| AppError::new("The saved query no longer exists"))
    })?;
    let workspace = target.unwrap_or_else(|| config.active().id.clone());
    Ok(config.workspace(&workspace)?.saved_queries.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{AppConfig, ConnectionProfile, WorkspaceMeta},
        saved_queries::SavedQueryState,
    };

    fn query(id: &str, connection: Option<&str>) -> SavedQuery {
        SavedQuery {
            id: id.into(),
            name: format!("Query {id}"),
            engine: "mongodb".into(),
            connection_id: connection.map(Into::into),
            database: "shop".into(),
            collection: "orders".into(),
            query: SavedQueryState {
                filter: "{ status: 'paid' }".into(),
                sort: "{ total: -1 }".into(),
                ..Default::default()
            },
            pipeline: Some(vec!["{ $match: {} }".into()]),
            updated_at: 0,
        }
    }

    /// Workspaces `default` (active) and `other` holding connection `m`.
    fn state(name: &str) -> AppState {
        let state = AppState::for_test(name, crate::Capabilities::desktop(), AppConfig::default());
        state
            .config
            .update(|config| {
                config.put_workspace(WorkspaceMeta {
                    id: "other".into(),
                    name: "Other".into(),
                    color: String::new(),
                    icon: String::new(),
                    safety: None,
                    assistant_queries: None,
                    assistant_learn: None,
                    mcp: None,
                });
                config.put_connection(
                    "other",
                    ConnectionProfile {
                        id: "m".into(),
                        name: "Mongo".into(),
                        engine: "mongodb".into(),
                        host: "127.0.0.1".into(),
                        port: 27017,
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        state
    }

    #[tokio::test]
    async fn saved_queries_live_in_the_workspace_of_their_connection() {
        let state = state("saved-queries-scope");
        let saved = save_saved_query(&state, query("q1", Some("m")), None).unwrap();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].updated_at > 0);
        assert!(
            list_saved_queries(&state, None, None).unwrap().is_empty(),
            "the active workspace has none"
        );
        assert_eq!(list_saved_queries(&state, Some("m".into()), None).unwrap(), saved);
        assert_eq!(list_saved_queries(&state, None, Some("other".into())).unwrap(), saved);

        // Without a saved connection: the workspace named, else the active one.
        save_saved_query(&state, query("q2", Some("unsaved")), None).unwrap();
        assert_eq!(list_saved_queries(&state, None, None).unwrap().len(), 1);

        // Saved again from the other workspace's connection: moved, not copied.
        save_saved_query(&state, query("q2", Some("m")), None).unwrap();
        assert!(list_saved_queries(&state, None, None).unwrap().is_empty());
        assert_eq!(list_saved_queries(&state, Some("m".into()), None).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rename_and_delete_persist_through_the_config_file() {
        let state = state("saved-queries-edit");
        save_saved_query(&state, query("q1", Some("m")), None).unwrap();
        let renamed = rename_saved_query(&state, "q1".into(), "  Big orders ".into()).unwrap();
        assert_eq!(renamed[0].name, "Big orders");
        assert!(rename_saved_query(&state, "q1".into(), " ".into()).is_err());
        assert!(rename_saved_query(&state, "gone".into(), "x".into()).is_err());

        // Written to disk: a reload reads the same queries back.
        let text = std::fs::read_to_string(state.config.path_for_tests()).unwrap();
        let reloaded: AppConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(reloaded.workspace("other").unwrap().saved_queries, renamed);

        assert!(delete_saved_query(&state, "q1".into()).unwrap().is_empty());
        assert!(delete_saved_query(&state, "q1".into()).is_err());
        assert!(state
            .config
            .snapshot()
            .workspaces
            .iter()
            .all(|workspace| workspace.saved_queries.is_empty()));
    }

    #[tokio::test]
    async fn invalid_queries_are_refused_and_nothing_is_written() {
        let state = state("saved-queries-invalid");
        let nameless = SavedQuery {
            name: "  ".into(),
            ..query("q1", Some("m"))
        };
        assert!(save_saved_query(&state, nameless, None).is_err());
        assert!(list_saved_queries(&state, Some("m".into()), None).unwrap().is_empty());
        assert!(list_saved_queries(&state, None, Some("missing".into())).is_err());
    }
}
