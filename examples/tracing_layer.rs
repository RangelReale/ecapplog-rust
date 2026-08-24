//! Routing `tracing` events, with their structured fields and span context, to the GUI.
//!
//! `cargo run --example tracing_layer --features tracing`
//!
//! The protocol carries one flat message string and has no notion of fields, so the layer puts the
//! event's fields and the enclosing spans into `source` as JSON. The GUI pretty-prints JSON in its
//! details pane, so click a row to see it laid out -- that is where the structured data lives.

use std::time::Duration;

use ecapplog::TracingLayer;
use tracing_subscriber::prelude::*;

#[tracing::instrument]
fn handle_request(request_id: &str, attempt: u32) {
    tracing::info!(method = "GET", path = "/health", "request received");

    // Fields of every kind, to show how each is rendered in the JSON.
    tracing::debug!(
        rows = 42,
        elapsed_ms = 3.5,
        cached = false,
        key = "user:1",
        "query finished"
    );

    query_database();
}

#[tracing::instrument]
fn query_database() {
    // Two spans deep: the `spans` array in `source` shows both, outermost first, each with the
    // arguments `#[instrument]` recorded.
    tracing::warn!(retries = 2, "slow query");

    let error: Box<dyn std::error::Error> = "connection reset".into();
    tracing::error!(error = error.as_ref(), "giving up");
}

fn main() {
    let layer = TracingLayer::builder().app_name("ECALRS-TRACING").build();
    let client = layer.client().clone();

    tracing_subscriber::registry()
        // For real level control, compose a filter instead of the layer's own `max_level`:
        //     .with(layer.with_filter(tracing_subscriber::EnvFilter::from_default_env()))
        .with(layer)
        .init();

    tracing::info!("starting up");
    handle_request("abc123", 2);
    tracing::info!(shutdown = true, "done");

    // A layer handed to a global subscriber is never dropped, so nothing flushes it implicitly.
    if !client.flush(Duration::from_secs(5)) {
        eprintln!("ecapplog: could not deliver everything within 5s -- is the GUI running?");
    }
}
