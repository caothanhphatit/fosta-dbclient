//! Startup tasks (background processes such as `kubectl port-forward`).

use tauri::State;

use crate::{
    config::AppConfig,
    error::AppResult,
    tasks::{TaskDef, TaskStatus},
    AppState,
};

#[tauri::command]
pub fn task_statuses(state: State<'_, AppState>) -> Vec<TaskStatus> {
    let config = state.config.snapshot();
    let tasks: Vec<TaskDef> = config.all_tasks().cloned().collect();
    state.tasks.statuses(&tasks)
}

#[tauri::command]
pub fn task_logs(state: State<'_, AppState>, id: String) -> Vec<String> {
    state.tasks.logs(&id)
}

#[tauri::command]
pub fn start_task(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.tasks.start(state.config.task(&id)?)
}

#[tauri::command]
pub fn stop_task(state: State<'_, AppState>, id: String) {
    state.tasks.stop(&id);
}

#[tauri::command]
pub async fn restart_task(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let task = state.config.task(&id)?;
    state.tasks.restart(task).await
}

#[tauri::command]
pub fn save_task(state: State<'_, AppState>, task: TaskDef, workspace_id: Option<String>) -> AppResult<AppConfig> {
    let workspace_id = workspace_id.unwrap_or_else(|| state.config.snapshot().active_workspace);
    state.config.update(|config| config.put_task(&workspace_id, task))
}

#[tauri::command]
pub fn delete_task(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
    state.tasks.stop(&id);
    state.config.update(|config| {
        config.remove_task(&id);
        Ok(())
    })
}
