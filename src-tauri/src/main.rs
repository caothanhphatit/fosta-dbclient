#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod commands;
mod config;
mod db;
mod drivers;
mod embedding;
mod error;
mod files;
mod job;
#[cfg(test)]
mod live_tests;
mod lock;
mod mongo;
mod mssql;
mod relations;
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
            });
            show_window_if_frontend_stalls(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_connection,
            commands::delete_connection,
            commands::test_connection,
            commands::connect,
            commands::disconnect,
            commands::list_databases,
            commands::create_database,
            commands::schema_graph,
            commands::save_download,
            commands::reveal_file,
            commands::list_tables,
            commands::describe_table,
            commands::browse_table,
            commands::apply_changes,
            commands::open_session,
            commands::close_session,
            commands::run_sql,
            commands::cancel_sql,
            commands::task_statuses,
            commands::task_logs,
            commands::start_task,
            commands::stop_task,
            commands::restart_task,
            commands::save_task,
            commands::delete_task,
            commands::save_workspace,
            commands::set_active_workspace,
            commands::delete_workspace,
            commands::table_relations,
            commands::vector_info,
            commands::fetch_vector,
            commands::vector_search,
            commands::vector_health,
            commands::index_recall,
            commands::save_embedding_settings,
            commands::test_embedding,
            commands::save_ai_connection,
            commands::delete_ai_connection,
            commands::set_ai_active,
            commands::list_ai_models,
            commands::test_ai,
            commands::ai_chat,
            commands::ai_cancel,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start DB Studio")
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
