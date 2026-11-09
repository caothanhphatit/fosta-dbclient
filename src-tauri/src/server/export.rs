//! Web exports without building the file in the page: `POST /api/invoke/export_prepare` keeps the request under a
//! one-time ticket, and the page then navigates a download to `GET /api/export/<ticket>`, which streams the file as
//! the response body while the pages are read (the browser's download manager writes it to disk). A GET carries no
//! CSRF header, so the ticket, issued to a signed-in page through a CSRF-checked POST, is what authorizes it; the
//! session cookie is still required, and must be the one the ticket was issued to. Closing the download stops the export.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use fosta_studio::{
    commands::gateway_context,
    contract::CancellationToken,
    error::{AppError, AppResult},
    export::{self, ChunkSink, ExportRequest},
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{api::InvokeError, auth, web::Server};

/// How long a ticket waits for its download to start.
const TICKET_TTL: Duration = Duration::from_secs(60);
/// Chunks in flight between the export and the response: the export waits while the browser is slower.
const CHUNKS_IN_FLIGHT: usize = 4;

struct Ticket {
    /// The session cookie of the page that asked for it (none with authentication off): only that session redeems it.
    session: Option<String>,
    connection_id: String,
    request: ExportRequest,
    issued: Instant,
}

static TICKETS: LazyLock<Mutex<HashMap<String, Ticket>>> = LazyLock::new(Mutex::default);

fn tickets() -> std::sync::MutexGuard<'static, HashMap<String, Ticket>> {
    TICKETS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The session cookie a request carries.
pub fn session_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| auth::cookie_value(cookies, auth::COOKIE_NAME))
        .map(str::to_owned)
}

/// `export_prepare(connectionId, request)` → the ticket for `GET /api/export/<ticket>`, redeemable by `session` only.
pub fn prepare(args: Value, session: Option<String>) -> Response {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Arguments {
        connection_id: String,
        request: ExportRequest,
    }
    let Arguments { connection_id, request } = match serde_json::from_value::<Arguments>(args) {
        Ok(arguments) => arguments,
        Err(error) => return InvokeError::Arguments(format!("Invalid arguments for export_prepare: {error}")).into_response(),
    };
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return InvokeError::Command(AppError::new("Cannot create a download ticket")).into_response();
    }
    let ticket: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut all = tickets();
    all.retain(|_, waiting| waiting.issued.elapsed() < TICKET_TTL);
    all.insert(
        ticket.clone(),
        Ticket {
            session,
            connection_id,
            request,
            issued: Instant::now(),
        },
    );
    Json(Value::String(ticket)).into_response()
}

/// Sends each chunk to the response; fails once the download was closed.
struct ChannelSink(mpsc::Sender<Result<Bytes, std::io::Error>>);

#[async_trait::async_trait]
impl ChunkSink for ChannelSink {
    async fn write(&mut self, chunk: Vec<u8>) -> AppResult<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.0
            .send(Ok(Bytes::from(chunk)))
            .await
            .map_err(|_| AppError::new("The download was closed"))
    }
}

/// Stops the export when the response is dropped (the download was cancelled).
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// `GET /api/export/<ticket>`: the file, streamed.
pub async fn download(State(server): State<Arc<Server>>, Path(ticket): Path<String>, headers: HeaderMap) -> Response {
    // Removed before any check: a ticket is spent by its first use, also a refused one.
    let Some(Ticket {
        session,
        connection_id,
        request,
        issued,
        ..
    }) = tickets().remove(&ticket)
    else {
        return InvokeError::Command(export::unknown_ticket()).into_response();
    };
    if issued.elapsed() >= TICKET_TTL || session != session_of(&headers) {
        return InvokeError::Command(export::unknown_ticket()).into_response();
    }
    let state = Arc::clone(&server.state);
    let context = match gateway_context(&state, &connection_id) {
        Ok(context) => context,
        Err(error) => return InvokeError::Command(error).into_response(),
    };
    let (sender, mut receiver) = mpsc::channel::<Result<Bytes, std::io::Error>>(CHUNKS_IN_FLIGHT);
    let cancel = CancellationToken::new();
    let format = request.format;
    let file_name = request.file_name.clone();
    let export_cancel = cancel.clone();
    tokio::spawn(async move {
        let (registered, _guard) = state.engines.requests.register(request.request_id.as_deref());
        let stop = registered.clone();
        let watch = export_cancel.clone();
        // A cancel from either side (the download closed, `cancel_request`) stops the export.
        let relay = tokio::spawn(async move {
            watch.cancelled().await;
            stop.cancel();
        });
        let mut sink = ChannelSink(sender.clone());
        let outcome = export::export(
            &state.engines,
            &context,
            &connection_id,
            &request,
            &registered,
            &mut sink,
            &mut |_| (),
        )
        .await;
        relay.abort();
        if let Err(error) = outcome {
            // Ends the body with an error, so the browser marks the download failed instead of keeping a cut file.
            let _ = sender.send(Err(std::io::Error::other(error.to_string()))).await;
        }
    });
    // The first chunk (the header, or the first error) decides the status.
    let first = match receiver.recv().await {
        Some(Ok(chunk)) => chunk,
        Some(Err(error)) => return (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()).into_response(),
        None => Bytes::new(),
    };
    let guard = CancelOnDrop(cancel);
    let body = futures_util::stream::unfold((Some(first), receiver, guard), |(first, mut receiver, guard)| async move {
        if let Some(chunk) = first {
            return Some((Ok(chunk), (None, receiver, guard)));
        }
        let next = receiver.recv().await?;
        Some((next, (None, receiver, guard)))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, format.content_type())
        .header(header::CONTENT_DISPOSITION, content_disposition(&file_name))
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Cannot stream the export").into_response())
}

/// `attachment` with an ASCII fallback name and the exact UTF-8 one.
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '/') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_survive_the_header() {
        assert_eq!(
            content_disposition("orders \"2026\"/é.csv"),
            "attachment; filename=\"orders _2026___.csv\"; filename*=UTF-8''orders%20%222026%22%2F%C3%A9.csv"
        );
    }
}
