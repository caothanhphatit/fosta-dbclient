//! `POST /api/invoke/<command>` and `GET /api/events`: Tauri's `invoke` and `listen` over HTTP.
//!
//! The request body holds the same arguments as `invoke(command, args)` (camelCase keys) and a successful
//! response the same JSON the Tauri command returns. Errors are `{ "error": "<message>" }`: 404 for an
//! unknown command, 400 for arguments that do not parse, 422 when the command itself fails.
//!
//! `ai_chat` answers with NDJSON instead: one `{ "event": ChatEvent }` line per streamed event, then
//! `{ "result": "<reply>" }` or `{ "error": "<message>" }`. Closing the response stops the run.

use std::{convert::Infallible, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use fosta_studio::{
    ai::{ChatEvent, ChatRequest},
    commands::{self, CommandOutput},
    error::AppError,
    events::EventEmitter,
    AppState,
};
use futures_util::{stream, Stream};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc};

use crate::web::Server;

/// An event for every open tab: `(name, payload)`.
pub type WebEvent = (String, Value);

/// Task events go to every `/api/events` stream.
pub struct WebEvents(pub broadcast::Sender<WebEvent>);

impl EventEmitter for WebEvents {
    fn emit(&self, event: &str, payload: Value) {
        // No open tab is not an error: the UI reads current statuses when it connects.
        let _ = self.0.send((event.to_owned(), payload));
    }
}

pub enum InvokeError {
    Unknown(String),
    Arguments(String),
    Command(AppError),
}

impl IntoResponse for InvokeError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unknown(command) => (StatusCode::NOT_FOUND, format!("Unknown command '{command}'")),
            Self::Arguments(message) => (StatusCode::BAD_REQUEST, message),
            Self::Command(error) => (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
        };
        error_response(status, &message)
    }
}

pub fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn parse_args<T: DeserializeOwned>(command: &str, args: Value) -> Result<T, InvokeError> {
    serde_json::from_value(args).map_err(|error| InvokeError::Arguments(format!("Invalid arguments for '{command}': {error}")))
}

macro_rules! call {
    ([sync] $call:expr) => {
        $call
    };
    ([async] $call:expr) => {
        $call.await
    };
}

/// `dispatch(state, command, args)`: runs one shared command with its JSON arguments.
macro_rules! web_commands {
    ($([$kind:tt] $name:ident ($($arg:ident : $type:ty),*) -> $output:ty = $function:path;)*) => {
        /// Every command the server answers through `dispatch` (`ai_chat` is handled separately).
        #[cfg(test)]
        pub const COMMANDS: &[&str] = &[$(stringify!($name)),*];

        #[allow(clippy::let_unit_value)]
        pub async fn dispatch(state: &AppState, command: &str, args: Value) -> Result<Value, InvokeError> {
            match command {
                $(stringify!($name) => {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Arguments {
                        $($arg: $type,)*
                    }
                    let Arguments { $($arg,)* } = parse_args(command, args)?;
                    let output: $output = call!([$kind] $function(state, $($arg),*));
                    output.into_json().map_err(InvokeError::Command)
                })*
                _ => Err(InvokeError::Unknown(command.to_owned())),
            }
        }
    };
}

fosta_studio::for_each_command!(web_commands);

pub async fn invoke(State(server): State<Arc<Server>>, Path(command): Path<String>, body: Bytes) -> Response {
    let args = if body.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Null) => json!({}),
            Ok(args @ Value::Object(_)) => args,
            Ok(_) => return InvokeError::Arguments("The arguments must be a JSON object".into()).into_response(),
            Err(error) => return InvokeError::Arguments(format!("Invalid JSON: {error}")).into_response(),
        }
    };
    if command == "ai_chat" {
        return ai_chat(Arc::clone(&server.state), args);
    }
    match dispatch(&server.state, &command, args).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Cancels the chat run when the response is dropped (the tab closed the request).
struct CancelOnDrop {
    state: Arc<AppState>,
    id: String,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.state.ai.cancel(&self.id);
    }
}

fn ai_chat(state: Arc<AppState>, args: Value) -> Response {
    #[derive(Deserialize)]
    struct Arguments {
        request: ChatRequest,
    }
    let request = match parse_args::<Arguments>("ai_chat", args) {
        Ok(arguments) => arguments.request,
        Err(error) => return error.into_response(),
    };
    let (sender, receiver) = mpsc::unbounded_channel::<String>();
    let guard = CancelOnDrop {
        state: Arc::clone(&state),
        id: request.id.clone(),
    };
    tokio::spawn(async move {
        let events = sender.clone();
        let sink = move |event: ChatEvent| {
            let _ = events.send(line(&json!({ "event": event })));
        };
        let last = match commands::ai::ai_chat(&state, request, &sink).await {
            Ok(text) => json!({ "result": text }),
            Err(error) => json!({ "error": error.to_string() }),
        };
        let _ = sender.send(line(&last));
    });
    let body = stream::unfold((receiver, guard), |(mut receiver, guard)| async move {
        let next = receiver.recv().await?;
        Some((Ok::<_, Infallible>(next), (receiver, guard)))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Cannot stream the reply"))
}

fn line(value: &Value) -> String {
    format!("{value}\n")
}

/// `GET /api/events`: task events as Server-Sent Events (`event: task-status`, `data: <json>`).
pub async fn events(State(server): State<Arc<Server>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = server.events.subscribe();
    let stream = stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok((name, payload)) => return Some((Ok(Event::default().event(name).data(payload.to_string())), receiver)),
                // A slow tab missed some log lines; keep going with the newest.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::COMMANDS;

    #[test]
    fn every_ui_command_is_routed() {
        // Commands `src/core/api.ts` calls, except the desktop-only file commands and the streamed ai_chat.
        let source = include_str!("../../../src/core/api.ts");
        let mut missing = Vec::new();
        for piece in source.split("invoke<").skip(1) {
            let Some(name) = piece.split('"').nth(1) else { continue };
            if !COMMANDS.contains(&name) && !matches!(name, "save_download" | "reveal_file" | "ai_chat") {
                missing.push(name.to_owned());
            }
        }
        assert!(missing.is_empty(), "commands the server does not route: {missing:?}");
    }
}
