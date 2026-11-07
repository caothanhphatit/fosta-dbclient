//! Tauri commands: the whole IPC surface the UI calls through `src/core/api.ts`.
//! Command names and argument names are part of that contract; keep them stable.
//!
//! Commands are grouped by area; `main.rs` registers each one by its module path.

pub mod ai;
pub mod connections;
pub mod data;
pub mod files;
pub mod tasks;
pub mod vector;
pub mod workspaces;

use tauri::State;

use crate::{config::AppConfig, AppState};

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> AppConfig {
    state.config.snapshot()
}
