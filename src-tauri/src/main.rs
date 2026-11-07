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
        fn $name(state: State<'_, AppState>, $($arg: $type),*) -> $output {
            $function(&state, $($arg),*)
        }
    };
    ([async] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty = $function:path) => {
        // A wrapper has the command's arguments plus the state (apply_changes has seven).
        #[allow(clippy::too_many_arguments)]
        #[tauri::command]
        async fn $name(state: State<'_, AppState>, $($arg: $type),*) -> $output {
            $function(&state, $($arg),*).await
        }
    };
}

/// Defines one wrapper per shared command, then the invoke handler listing them plus the desktop-only ones.
macro_rules! desktop_commands {
    ($([$kind:tt] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty = $function:path;)*) => {
        $(tauri_command!([$kind] $name ($($arg : $type),*) -> $output = $function);)*

        fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
            tauri::generate_handler![$($name,)* ai_chat, save_download, reveal_file]
        }
    };
}

fosta_studio::for_each_command!(desktop_commands);

/// Streams one assistant reply over `on_event`; resolves with the full text.
#[tauri::command]
async fn ai_chat(state: State<'_, AppState>, request: ChatRequest, on_event: Channel<ChatEvent>) -> AppResult<String> {
    let sink = move |event: ChatEvent| {
        let _ = on_event.send(event);
    };
    commands::ai::ai_chat(&state, request, &sink).await
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

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let executor = tauri::async_runtime::handle().inner().clone();
            let events = Arc::new(TauriEvents(app.handle().clone()));
            app.manage(AppState::new(Capabilities::desktop(), executor, events)?);
            show_window_if_frontend_stalls(app);
            Ok(())
        })
        .invoke_handler(invoke_handler())
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
