//! A Rust client for [ECAppLog](https://github.com/RangelReale/ecapplog), a GUI for viewing debug
//! logs, alongside the [Go](https://github.com/RangelReale/ecapplog-go) and
//! [C++](https://github.com/RangelReale/ecapplog-cpp) clients.
//!
//! # Getting started
//!
//! ```no_run
//! use ecapplog::{Client, Priority, Record};
//!
//! let client = Client::builder().app_name("my-service").build();
//!
//! client.info("app", "started");
//!
//! client.log(
//!     Record::new(Priority::Error, "app", "could not reach the database")
//!         .original_category("app.storage")
//!         .extra_category("db")
//!         .color("#ff5555"),
//! );
//! ```
//!
//! The client is cheap to clone, shares one worker thread, and shuts down on the last drop.
//!
//! # Behaviour worth knowing
//!
//! Logging never touches the network on the calling thread. Entries go into a bounded queue --
//! 1000 by default -- that a background thread drains. While the GUI is not running they
//! accumulate there and are delivered as soon as it appears, so an application can be started
//! first and attached to later. Once the queue is full the **oldest** entry is discarded to make
//! room; install [`ClientBuilder::on_error`] or read [`Client::dropped_count`] to notice.
//!
//! Delivery is best-effort by design. The protocol is one-directional -- the server never writes a
//! byte back -- so nothing can be acknowledged, and a rejected entry is invisible to the client.
//!
//! # Standard logging facades
//!
//! Two optional adapters route an application's existing log calls to the GUI, so no call sites
//! have to change:
//!
//! * `log` feature -- [`LogLogger`], implementing [`log::Log`].
//! * `tracing` feature -- [`TracingLayer`], a `tracing_subscriber` layer.
//!
//! ```toml
//! ecapplog = { version = "1.0", features = ["tracing"] }
//! ```
//!
//! # The protocol
//!
//! Frames are `u8` command, `u32` big-endian length, then the payload. Command `99` is the banner
//! that names the connection; command `0` carries a JSON log entry. The default endpoint is
//! `127.0.0.1:13991`, and the GUI listens on loopback only unless configured otherwise.
//!
//! Two details bite anyone reimplementing this:
//!
//! * Timestamps are **not** RFC 3339. The server parses the literal format
//!   `yyyy-MM-ddThh:mm:ss.zzz` in UTC, rejecting any zone suffix and any fractional precision other
//!   than exactly three digits -- and on rejection it silently substitutes its own arrival time
//!   rather than reporting anything. This crate handles the formatting.
//! * A frame over 16 MiB makes the server close the connection. Oversized entries are dropped
//!   here instead.

#![doc(html_root_url = "https://docs.rs/ecapplog")]

mod client;
mod error;
mod priority;
mod proto;
mod queue;
mod record;
mod timestamp;

pub use client::{Client, ClientBuilder, DEFAULT_ADDRESS, DEFAULT_APP_NAME, DEFAULT_BUFFER_SIZE};
pub use error::{Error, OnError};
pub use priority::Priority;
pub use record::Record;

#[cfg(feature = "log")]
mod adapter_log;
#[cfg(feature = "log")]
pub use adapter_log::{LogLogger, LogLoggerBuilder};

#[cfg(feature = "tracing")]
mod adapter_tracing;
#[cfg(feature = "tracing")]
pub use adapter_tracing::{TracingLayer, TracingLayerBuilder};
