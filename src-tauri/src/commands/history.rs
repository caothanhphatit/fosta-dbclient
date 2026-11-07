//! Query history: runs of the query tabs, stored on this machine only (see `history.rs`).
//!
//! File access runs on the blocking pool so a slow disk never stalls the UI; the store
//! serialises concurrent calls from several tabs.

use std::sync::Arc;

use tauri::State;

use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
    history::{HistoryEntry, HistoryQuery, HistoryStore},
    AppState,
};

/// Records one run; returns the stored entry, or nothing when history is turned off.
#[tauri::command]
pub async fn history_append(state: State<'_, AppState>, entry: HistoryEntry) -> AppResult<Option<HistoryEntry>> {
    if !state.config.query_history_enabled() {
        return Ok(None);
    }
    if entry.connection_id.trim().is_empty() {
        return Err(AppError::new("A history entry needs the connection it ran on"));
    }
    with_store(&state, move |store| store.append(entry)).await.map(Some)
}

/// Entries matching `query`, newest first.
#[tauri::command]
pub async fn history_list(state: State<'_, AppState>, query: HistoryQuery) -> AppResult<Vec<HistoryEntry>> {
    with_store(&state, move |store| store.list(&query)).await
}

/// Removes entries by id; returns how many were removed.
#[tauri::command]
pub async fn history_delete(state: State<'_, AppState>, ids: Vec<String>) -> AppResult<usize> {
    with_store(&state, move |store| store.delete(&ids)).await
}

#[tauri::command]
pub async fn history_clear(state: State<'_, AppState>) -> AppResult<()> {
    with_store(&state, |store| store.clear()).await
}

/// Turns recording on or off (`queryHistory` in `fosta-studio.json`); existing entries are kept.
#[tauri::command]
pub fn set_query_history(state: State<'_, AppState>, enabled: bool) -> AppResult<AppConfig> {
    state.config.update(|config| {
        config.query_history = enabled;
        Ok(())
    })
}

async fn with_store<T: Send + 'static>(
    state: &State<'_, AppState>,
    work: impl FnOnce(&HistoryStore) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let store: Arc<HistoryStore> = Arc::clone(&state.history);
    tauri::async_runtime::spawn_blocking(move || work(&store))
        .await
        .map_err(|error| AppError::new(format!("Query history failed: {error}")))?
}
