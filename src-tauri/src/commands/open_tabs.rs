//! The open tabs of each workspace, restored after a restart (see `open_tabs.rs`). File access runs on the blocking pool.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::{
    error::{AppError, AppResult},
    open_tabs::OpenTabsStore,
    AppState,
};

/// Every workspace's saved tabs, by workspace id.
pub async fn load_open_tabs(state: &AppState) -> AppResult<Map<String, Value>> {
    with_store(state, |store| store.load()).await
}

/// Saves the tabs of one workspace; `None` forgets them (a deleted workspace).
pub async fn save_open_tabs(state: &AppState, workspace_id: String, tabs: Option<Value>) -> AppResult<()> {
    with_store(state, move |store| store.save(&workspace_id, tabs)).await
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
        let tabs = json!({ "version": 1, "tabs": [{ "kind": "query", "connectionId": "c", "title": "Query 1", "text": "SELECT 1" }], "active": 0 });
        save_open_tabs(&state, "ws".into(), Some(tabs.clone())).await.unwrap();
        assert_eq!(load_open_tabs(&state).await.unwrap().get("ws"), Some(&tabs));
        save_open_tabs(&state, "ws".into(), None).await.unwrap();
        assert!(load_open_tabs(&state).await.unwrap().is_empty());
        assert!(save_open_tabs(&state, String::new(), Some(tabs)).await.is_err());
    }
}
