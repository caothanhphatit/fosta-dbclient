//! The open tabs of each workspace, restored after a restart (see `open_tabs.rs`). File access runs on the blocking pool.

use std::{collections::HashSet, sync::Arc};

use serde_json::Value;

use crate::{
    error::{AppError, AppResult},
    open_tabs::{LoadedTabs, OpenTabsStore, SaveOutcome},
    AppState,
};

/// Every workspace's saved tabs, by workspace id, and the revision each was saved at.
pub async fn load_open_tabs(state: &AppState) -> AppResult<LoadedTabs> {
    with_store(state, |store| store.load()).await
}

/// Saves the tabs of one workspace; `None` forgets them (a deleted workspace). `revision`: the revision the tabs are
/// based on (a conflict writes nothing and returns the stored tabs); none saves unconditionally. Entries of workspaces
/// no longer in the configuration are dropped.
pub async fn save_open_tabs(state: &AppState, workspace_id: String, tabs: Option<Value>, revision: Option<u64>) -> AppResult<SaveOutcome> {
    let workspaces: HashSet<String> = state
        .config
        .read(|config| config.workspaces.iter().map(|workspace| workspace.id.clone()).collect());
    with_store(state, move |store| {
        store.save(&workspace_id, tabs, revision, &|id| workspaces.contains(id))
    })
    .await
}

async fn with_store<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&OpenTabsStore) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let store = Arc::clone(&state.open_tabs);
    tokio::task::spawn_blocking(move || work(&store))
        .await
        .map_err(|error| AppError::new(format!("Saving the open tabs failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::AppConfig;

    #[tokio::test]
    async fn the_commands_save_and_load_each_workspace() {
        let state = AppState::for_test("open-tabs-commands", crate::Capabilities::desktop(), AppConfig::default());
        let ws = state.config.read(|config| config.workspaces[0].id.clone());
        let tabs = json!({ "version": 1, "tabs": [{ "kind": "query", "connectionId": "c", "title": "Query 1", "text": "SELECT 1" }], "active": 0 });
        let saved = save_open_tabs(&state, ws.clone(), Some(tabs.clone()), Some(0)).await.unwrap();
        assert_eq!((saved.saved, saved.revision), (true, 1));
        let loaded = load_open_tabs(&state).await.unwrap();
        assert_eq!(loaded.workspaces.get(&ws), Some(&tabs));
        assert_eq!(loaded.revisions.get(&ws), Some(&1));
        // A stale writer gets the stored tabs back.
        let stale = save_open_tabs(&state, ws.clone(), Some(json!({ "tabs": [] })), Some(0))
            .await
            .unwrap();
        assert_eq!((stale.saved, stale.current), (false, Some(tabs.clone())));
        // A workspace that is not in the configuration is not stored.
        save_open_tabs(&state, "elsewhere".into(), Some(tabs.clone()), None).await.unwrap();
        assert!(!load_open_tabs(&state).await.unwrap().workspaces.contains_key("elsewhere"));
        save_open_tabs(&state, ws.clone(), None, None).await.unwrap();
        assert!(load_open_tabs(&state).await.unwrap().workspaces.is_empty());
        assert!(save_open_tabs(&state, String::new(), Some(tabs), None).await.is_err());
    }
}
