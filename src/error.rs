//! Errors reported by the background worker.
//!
//! The ECAppLog protocol is one-directional: the server never writes a byte back, so nothing here
//! can be an error *returned* to a `log` call. These arrive asynchronously instead, through the
//! callback installed with [`ClientBuilder::on_error`](crate::ClientBuilder::on_error).
//!
//! Both sibling clients discard this information entirely -- Go's `handleError` has an empty body
//! and C++ wraps its worker in `catch (std::exception&) { /* ignore */ }` -- which makes "no logs
//! are appearing" impossible to diagnose from inside the application.

use std::io;
use std::sync::Arc;

/// Something the background worker could not do.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The configured address could not be resolved.
    #[error("could not resolve ECAppLog address `{address}`: {source}")]
    Resolve {
        /// The address as configured.
        address: String,
        /// The underlying resolver error.
        source: io::Error,
    },

    /// No connection could be established. The worker retries after the reconnect delay.
    #[error("could not connect to ECAppLog at `{address}`: {source}")]
    Connect {
        /// The address as configured.
        address: String,
        /// The underlying connect error.
        source: io::Error,
    },

    /// A frame could not be written. The connection is dropped and re-established, and the entry
    /// that failed is put back at the head of the queue rather than lost.
    #[error("could not write to ECAppLog: {source}")]
    Write {
        /// The underlying write error.
        source: io::Error,
    },

    /// An entry could not be serialised to JSON, and was discarded.
    #[error("could not serialise a log entry: {source}")]
    Serialize {
        /// The underlying serialisation error.
        source: serde_json::Error,
    },

    /// An entry serialised to more than the server's 16 MiB frame limit, and was discarded.
    ///
    /// Sending it would have made the server close the connection, taking every entry queued
    /// behind it down as well, so dropping the one oversized entry is the lesser loss.
    #[error("a log entry serialised to {size} bytes, over the server's {limit} byte limit")]
    PayloadTooLarge {
        /// The size the entry serialised to.
        size: usize,
        /// The server's limit.
        limit: usize,
    },

    /// The queue was full, so the oldest entry was discarded to make room for a new one.
    ///
    /// Almost always means the GUI is not running, or is not keeping up. The running total is also
    /// available from [`Client::dropped_count`](crate::Client::dropped_count).
    #[error("log queue full ({capacity} entries), discarded the oldest; {total} dropped so far")]
    QueueFull {
        /// The configured queue capacity.
        capacity: usize,
        /// How many entries this client has dropped in total.
        total: u64,
    },
}

/// A callback invoked when the background worker hits an [`Error`].
///
/// Called from the worker thread, or from whichever thread was logging when a queue overflow
/// happened -- never while a lock is held, so it is free to log, panic-free work aside. Keep it
/// cheap: it runs on the path that is already struggling.
pub type OnError = Arc<dyn Fn(&Error) + Send + Sync + 'static>;
