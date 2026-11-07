#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod audit;
mod commands;
mod config;
mod drivers;
mod embedding;
mod error;
mod files;
mod gateway;
mod history;
mod job;
#[cfg(test)]
mod live_tests;
mod lock;
mod schema;
mod secrets;
mod tasks;
mod vector;

use std::{sync::Arc, time::Duration};

use tauri::{App, Manager, RunEvent};

const SHOW_WINDOW_FALLBACK: Duration = Duration::from_secs(3);

pub struct AppState {
    pub config: config::ConfigStore,
    pub db: drivers::DbManager,
    pub tasks: Arc<tasks::TaskManager>,
    pub ai: ai::AiRuns,
    pub history: Arc<history::HistoryStore>,
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let config = config::ConfigStore::load()?;
            let tasks = Arc::new(tasks::TaskManager::new(app.handle().clone()));
            tasks.start_auto(&config.snapshot().active().tasks);
            app.manage(AppState {
                config,
                db: drivers::DbManager::default(),
                tasks,
                ai: ai::AiRuns::default(),
                history: Arc::new(history::HistoryStore::open_default()?),
            });
            show_window_if_frontend_stalls(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::connections::save_connection,
            commands::connections::delete_connection,
            commands::connections::test_connection,
            commands::connections::connect,
            commands::connections::disconnect,
            commands::data::list_databases,
            commands::data::create_database,
            commands::data::schema_graph,
            commands::files::save_download,
            commands::files::reveal_file,
            commands::data::list_tables,
            commands::data::describe_table,
            commands::data::browse_table,
            commands::data::apply_changes,
            commands::data::open_session,
            commands::data::close_session,
            commands::data::run_sql,
            commands::data::cancel_sql,
            commands::data::sql_transaction,
            commands::tasks::task_statuses,
            commands::tasks::task_logs,
            commands::tasks::start_task,
            commands::tasks::stop_task,
            commands::tasks::restart_task,
            commands::tasks::save_task,
            commands::tasks::delete_task,
            commands::workspaces::save_workspace,
            commands::workspaces::set_active_workspace,
            commands::workspaces::delete_workspace,
            commands::data::table_relations,
            commands::vector::vector_info,
            commands::vector::fetch_vector,
            commands::vector::vector_search,
            commands::vector::vector_health,
            commands::vector::index_recall,
            commands::vector::save_embedding_settings,
            commands::vector::test_embedding,
            commands::ai::save_ai_connection,
            commands::ai::delete_ai_connection,
            commands::ai::set_ai_active,
            commands::ai::list_ai_models,
            commands::ai::test_ai,
            commands::ai::ai_chat,
            commands::ai::ai_cancel,
            commands::history::history_append,
            commands::history::history_list,
            commands::history::history_delete,
            commands::history::history_clear,
            commands::history::set_query_history,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start Fosta Studio")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<AppState>().tasks.stop_all();
            }
        });
}

fn show_window_if_frontend_stalls(app: &App) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SHOW_WINDOW_FALLBACK).await;
        let _ = window.show();
    });
}
