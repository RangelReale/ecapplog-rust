//! What the layer puts on the wire for a record that came through the `log` facade.
//!
//! The bridge is the one input this layer cannot see with its own eyes: `tracing-log` hands it an
//! event whose metadata names the target `log` and nothing else, with the record's own target,
//! module path, file and line carried as fields. Everything this file asserts is the recovery of
//! those, and none of it can be checked from inside `on_event` — a test has to install the bridge
//! and read the record back off a socket.
//!
//! **Loopback and an ephemeral port**, so nothing here asks a firewall for anything.
//!
//! **A scoped subscriber, not a global one**, because `set_global_default` takes for the life of
//! the process and this binary holds more than one test. `LogTracer` is the exception: it takes the
//! global logger, which `log` offers only once, so a `Once` guards it and every test here shares the
//! one install. That costs nothing, the bridge dispatching to whichever subscriber is current rather
//! than to one it captured.

use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Once;
use std::time::Duration;

use ecapplog::{Client, TracingLayerBuilder};
use serde_json::Value;
use tracing_subscriber::layer::SubscriberExt as _;

/// The banner every connection opens with.
const CMD_BANNER: u8 = 99;
/// One log record.
const CMD_LOG: u8 = 0;

/// Puts the `log` facade on the bridge, once for the whole binary.
fn bridge() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing_log::LogTracer::init().expect("nothing else has taken the global logger");
    });
}

/// A viewer that is listening, and the records it heard.
fn collect(emit: impl FnOnce()) -> Vec<Value> {
    collect_with(|builder| builder, emit)
}

/// The same, for a test that configures the layer differently.
fn collect_with(
    configure: impl FnOnce(TracingLayerBuilder) -> TracingLayerBuilder,
    emit: impl FnOnce(),
) -> Vec<Value> {
    bridge();

    let listener =
        TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("bind loopback");
    let address = listener
        .local_addr()
        .expect("read back the port")
        .to_string();

    // The address belongs to the client, so the layer takes one already pointed at the listener.
    let client = Client::builder()
        .app_name("bridge-test")
        .address(address)
        .build();
    let layer = configure(ecapplog::TracingLayer::builder().client(client.clone())).build();
    let subscriber = tracing_subscriber::registry().with(layer);

    let accepted = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));

    tracing::subscriber::with_default(subscriber, emit);
    // Drains what was queued, which is what makes the read below end at a known point rather than
    // at a timeout.
    client.flush(Duration::from_secs(5));

    let stream = accepted
        .join()
        .expect("the listener thread")
        .expect("a connection");
    read_records(stream)
}

/// Reads frames until the client stops talking, keeping the log records.
fn read_records(mut stream: TcpStream) -> Vec<Value> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set a read timeout");

    let mut records = Vec::new();
    let mut header = [0_u8; 5];
    while stream.read_exact(&mut header).is_ok() {
        let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).expect("a whole payload");
        match header[0] {
            CMD_BANNER => {}
            CMD_LOG => {
                let payload = String::from_utf8(payload).expect("utf-8");
                records.push(serde_json::from_str(&payload).expect("a JSON record"));
            }
            other => panic!("unknown command {other}"),
        }
    }
    records
}

/// The `source` payload of one record, parsed.
fn source(record: &Value) -> Value {
    serde_json::from_str(record["source"].as_str().expect("a source string"))
        .expect("the source is JSON")
}

/// A record from the facade is filed under its own target, not under `log`.
///
/// The module path is beside it as the original category, which is what the GUI draws as
/// `category [original_category]`, and the call site is the one the `log` macro recorded rather
/// than nothing at all.
#[test]
fn a_facade_record_is_filed_under_its_own_target() {
    let records = collect(|| {
        log::debug!(target: "some_crate::inner", "through the facade");
    });

    assert_eq!(records.len(), 1, "one record, one entry");
    let record = &records[0];

    assert_eq!(record["category"], "some_crate::inner");
    assert_eq!(record["message"], "through the facade");
    assert_eq!(record["priority"], "DEBUG");
    // The `log` macro takes the module path from where it was written, which is this file.
    assert_eq!(record["original_category"], module_path!());

    let at = source(record)["at"]
        .as_str()
        .expect("a call site")
        .to_owned();
    assert!(
        at.contains("tracing_log_bridge.rs:"),
        "{at} is not the call site the macro recorded"
    );
}

/// The four fields the bridge carries its metadata in do not also reach the details pane.
///
/// They are the category, the original category and the call site by the time the record is built,
/// so a copy of each in `source.fields` would be the same fact drawn twice. A record with nothing
/// else on it therefore has no `fields` at all.
#[test]
fn the_bridges_own_fields_do_not_reach_the_details_pane() {
    let records = collect(|| {
        log::info!(target: "some_crate", "counted");
    });

    let source = source(&records[0]);
    assert!(
        source.get("fields").is_none(),
        "{source} carries fields nobody wrote"
    );
}

/// A category function sees the recovered target, which is the whole point of recovering it.
///
/// Filing by the crate rather than by the module is what an application wants from one of these,
/// and it cannot write it against a target that is always `log`.
#[test]
fn a_category_function_sees_the_recovered_target() {
    let records = collect_with(
        |builder| {
            builder.category_fn(|metadata| {
                metadata
                    .target()
                    .split_once("::")
                    .map_or(metadata.target(), |(crate_name, _)| crate_name)
                    .to_owned()
            })
        },
        || log::warn!(target: "some_crate::inner", "careful"),
    );

    assert_eq!(records[0]["category"], "some_crate");
}

/// An event `tracing` emitted is untouched by any of this.
///
/// Its metadata is already the right one and it carries no `log.` fields, so the recovery has to be
/// a no-op on it rather than something the ordinary path pays for.
#[test]
fn a_tracing_event_is_unaffected() {
    let records = collect(|| {
        tracing::info!(port = 8080, "listening");
    });

    assert_eq!(records[0]["category"], module_path!());
    assert_eq!(records[0]["message"], "listening");
    assert_eq!(source(&records[0])["fields"]["port"], 8080);
}
