use axum::response::sse::{Event, Sse};
use futures_core::stream::Stream;
use serde::Serialize;
use std::{convert::Infallible, time::Duration};
use tokio::sync::broadcast;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Msg {
    Begin(&'static str),
    Success(&'static str),
    Info(&'static str),
    Fail(&'static str, String),
    Done,
}

#[tracing::instrument(skip_all)]
pub async fn sse_progress(
    axum::extract::State(app): axum::extract::State<crate::state::AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // If no run in progress, create a dummy channel and emit a short note.
    let rx = match app.progress.read().unwrap().as_ref() {
        Some(tx) => tx.subscribe(),
        None => {
            // Create a one-shot channel just to say "no run"
            let (tx, _) = broadcast::channel(1);
            let _ = tx.send(Msg::Info("no install run active"));
            tx.subscribe()
        }
    };

    let stream = BroadcastStream::new(rx).map(|item| {
        match item {
            Ok(msg) => {
                // Serialize once and push as data
                let json = serde_json::to_string(&msg)
                    .unwrap_or_else(|_| "{\"type\":\"Fail\",\"data\":\"encode\"}".into());
                Ok(Event::default().data(json))
            }
            Err(_lagged) => {
                // If we lag/drop, emit a heartbeat
                Ok(Event::default().event("ping").data("💓"))
            }
        }
    });

    // Keep-alives help some proxies
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
