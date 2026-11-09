#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The desktop front-end: a Tauri window around the shared backend in `lib.rs`. Every command is a thin
//! `#[tauri::command]` wrapper generated from `for_each_command!`, so names, arguments and JSON stay those
//! of `src/core/api.ts`.

use std::{sync::Arc, time::Duration};

use fosta_studio::{
    ai::{ChatEvent, ChatRequest},
    commands,
    error::AppResult,
    events::EventEmitter,
    AppState, Capabilities,
};
use serde_json::Value;
use tauri::{ipc::Channel, App, AppHandle, Emitter, Manager, RunEvent, State};

const SHOW_WINDOW_FALLBACK: Duration = Duration::from_secs(3);

/// Task events go out as Tauri events (`listen("task-status", …)` in the UI).
struct TauriEvents(AppHandle);

impl EventEmitter for TauriEvents {
    fn emit(&self, event: &str, payload: Value) {
        let _ = self.0.emit(event, payload);
    }
}

macro_rules! tauri_command {
    ([sync] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty = $function:path) => {
        #[allow(clippy::too_many_arguments)]
        #[tauri::command]
        fn $name(state: State<'_, Arc<AppState>>, $($arg: $type),*) -> $output {
            fosta_studio::logging::command_sync(stringify!($name), || $function(&state, $($arg),*))
        }
    };
    ([async] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty = $function:path) => {
        // A wrapper has the command's arguments plus the state (apply_changes has seven).
        #[allow(clippy::too_many_arguments)]
        #[tauri::command]
        async fn $name(state: State<'_, Arc<AppState>>, $($arg: $type),*) -> $output {
            fosta_studio::logging::command(stringify!($name), $function(&state, $($arg),*)).await
        }
    };
    ([stream] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty => $event:ty = $function:path) => {
        // Events go out on the `onEvent` channel; a failed send means the webview is gone, so the command stops.
        #[allow(clippy::too_many_arguments)]
        #[tauri::command]
        async fn $name(state: State<'_, Arc<AppState>>, $($arg: $type,)* on_event: Channel<$event>) -> $output {
            let events: fosta_studio::events::EventSink<$event> = Arc::new(move |event| on_event.send(event).is_ok());
            fosta_studio::logging::command(stringify!($name), $function(&state, $($arg,)* events)).await
        }
    };
}

/// Defines one wrapper per shared command, then the invoke handler listing them plus the desktop-only ones.
macro_rules! desktop_commands {
    ($([$kind:tt] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty $(=> $event:ty)? = $function:path;)*) => {
        $(tauri_command!([$kind] $name ($($arg : $type),*) -> $output $(=> $event)? = $function);)*

        fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
            tauri::generate_handler![$($name,)* ai_chat, export_rows, save_download, reveal_file, open_log_folder, open_release_page]
        }
    };
}

fosta_studio::for_each_command!(desktop_commands);

/// Streams one assistant reply over `on_event`; resolves with the full text.
#[tauri::command]
async fn ai_chat(state: State<'_, Arc<AppState>>, request: ChatRequest, on_event: Channel<ChatEvent>) -> AppResult<String> {
    let sink = move |event: ChatEvent| {
        let _ = on_event.send(event);
    };
    commands::ai::ai_chat(&state, request, &sink).await
}

/// Exports every row matching a grid into Downloads, page by page, with progress on `on_event`.
#[tauri::command]
async fn export_rows(
    state: State<'_, AppState>,
    connection_id: String,
    request: fosta_studio::export::ExportRequest,
    on_event: Channel<fosta_studio::export::ExportProgress>,
) -> AppResult<commands::files::ExportDone> {
    let progress = move |event| {
        let _ = on_event.send(event);
    };
    fosta_studio::logging::command(
        "export_rows",
        commands::files::export_rows(&state, connection_id, request, progress),
    )
    .await
}

/// Saves an export into Downloads; `base64` is for binary content such as PNG.
#[tauri::command]
fn save_download(name: String, text: Option<String>, base64: Option<String>) -> AppResult<String> {
    commands::files::save_download(name, text, base64)
}

#[tauri::command]
fn reveal_file(path: String) -> AppResult<()> {
    commands::files::reveal_file(path)
}

#[tauri::command]
fn open_log_folder() -> AppResult<()> {
    commands::diagnostics::open_log_folder()
}

/// The update item's release link: the webview opens no browser windows itself.
#[tauri::command]
fn open_release_page(url: String) -> AppResult<()> {
    commands::update::open_release_page(url)
}

fn main() {
    // `fosta-studio mcp`: the stdio bridge for MCP clients; it starts no window and writes only JSON-RPC to stdout.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        std::process::exit(fosta_studio::mcp::bridge::run_desktop());
    }
    if let Ok(data_dir) = fosta_studio::config::data_dir() {
        fosta_studio::logging::init(&data_dir, fosta_studio::logging::LogLocation::Desktop);
    }
    fosta_studio::runtime::install_panic_hook();
    tauri::Builder::default()
        // In-place updates of the installed app (src/app/update.ts); the endpoint and key are in tauri.conf.json.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let executor = tauri::async_runtime::handle().inner().clone();
            let events = Arc::new(TauriEvents(app.handle().clone()));
            let state = Arc::new(AppState::new(Capabilities::desktop(), executor, events)?);
            state.mcp.attach(&state);
            if let Err(error) = state.mcp.apply() {
                tracing::warn!(%error, "the MCP server did not start");
            }
            app.manage(state);
            show_window_if_frontend_stalls(app);
            Ok(())
        })
        .invoke_handler(invoke_handler())
        .build(tauri::generate_context!())
        .expect("failed to start Fosta Studio")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<Arc<AppState>>().tasks.stop_all();
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
