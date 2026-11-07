//! Startup tasks (background processes such as `kubectl port-forward`).

use crate::{
    config::AppConfig,
    error::AppResult,
    tasks::{TaskDef, TaskStatus},
    AppState,
};

pub fn task_statuses(state: &AppState) -> Vec<TaskStatus> {
    let config = state.config.snapshot();
    let tasks: Vec<TaskDef> = config.all_tasks().cloned().collect();
    state.tasks.statuses(&tasks)
}

pub fn task_logs(state: &AppState, id: String) -> Vec<String> {
    state.tasks.logs(&id)
}

pub fn start_task(state: &AppState, id: String) -> AppResult<()> {
    state.tasks.start(state.config.task(&id)?)
}

pub fn stop_task(state: &AppState, id: String) {
    state.tasks.stop(&id);
}

pub async fn restart_task(state: &AppState, id: String) -> AppResult<()> {
    let task = state.config.task(&id)?;
    state.tasks.restart(task).await
}

pub fn save_task(state: &AppState, task: TaskDef, workspace_id: Option<String>) -> AppResult<AppConfig> {
    let workspace_id = workspace_id.unwrap_or_else(|| state.config.snapshot().active_workspace);
    state.config.update(|config| config.put_task(&workspace_id, task))
}

pub fn delete_task(state: &AppState, id: String) -> AppResult<AppConfig> {
    state.tasks.stop(&id);
    state.config.update(|config| {
        config.remove_task(&id);
        Ok(())
    })
}
