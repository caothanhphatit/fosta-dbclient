//! Events the backend pushes to the UI without being asked (task status and log lines).
//! The desktop app emits them as Tauri events; the web server sends them to every open tab
//! over Server-Sent Events. Either way the UI receives `{ event, payload }` with the same JSON.

use serde_json::Value;

pub trait EventEmitter: Send + Sync {
    fn emit(&self, event: &str, payload: Value);
}

/// Drops every event (tests, and anything that runs without a UI).
pub struct NoEvents;

impl EventEmitter for NoEvents {
    fn emit(&self, _event: &str, _payload: Value) {}
}

/// Where a streaming command sends its events (`run`'s `StreamEvent`s). Returns false once the receiver is gone (the
/// tab closed, the HTTP response was dropped), so the command can stop.
pub type EventSink<E> = std::sync::Arc<dyn Fn(E) -> bool + Send + Sync>;
