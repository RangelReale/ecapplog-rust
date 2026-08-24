//! The client handle and its background worker thread.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::{Error, OnError};
use crate::priority::Priority;
use crate::proto::{self, LogPayload, MAX_PAYLOAD_SIZE};
use crate::queue::{Pop, Queue};
use crate::record::Record;

/// The address the ECAppLog GUI listens on. The port is not configurable server-side.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:13991";

/// The default application name, shown as the GUI's connection tab.
pub const DEFAULT_APP_NAME: &str = "ECAPPLOG-RUST";

/// The default queue capacity, matching the Go client.
pub const DEFAULT_BUFFER_SIZE: usize = 1000;

/// How long the worker waits for an entry before re-checking its own deadlines. Only a safety net:
/// a push or a shutdown wakes it immediately.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

struct Config {
    app_name: String,
    address: String,
    flush_on_close: bool,
    flush_timeout: Duration,
    connect_timeout: Duration,
    reconnect_delay: Duration,
    on_error: Option<OnError>,
}

impl Config {
    fn report(&self, error: &Error) {
        if let Some(on_error) = &self.on_error {
            on_error(error);
        }
    }
}

/// Builds a [`Client`].
///
/// ```
/// let client = ecapplog::Client::builder()
///     .app_name("my-service")
///     .buffer_size(5_000)
///     .on_error(|e| eprintln!("ecapplog: {e}"))
///     .build();
/// ```
pub struct ClientBuilder {
    app_name: String,
    address: String,
    buffer_size: usize,
    flush_on_close: bool,
    flush_timeout: Duration,
    connect_timeout: Duration,
    reconnect_delay: Duration,
    on_error: Option<OnError>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            app_name: DEFAULT_APP_NAME.to_owned(),
            address: DEFAULT_ADDRESS.to_owned(),
            buffer_size: DEFAULT_BUFFER_SIZE,
            // On by default, unlike both sibling clients. C++ actively discards whatever is queued
            // (its quit sentinel is pushed to the *front*), and Go makes draining opt-in, which is
            // why both of their samples end in a bare sleep to avoid losing the tail.
            flush_on_close: true,
            flush_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(10),
            reconnect_delay: Duration::from_secs(5),
            on_error: None,
        }
    }
}

impl ClientBuilder {
    /// The name the GUI labels this connection with. Default `ECAPPLOG-RUST`.
    ///
    /// The server appends a sequence number, so the tab reads `my-service:1`. Reconnecting gets a
    /// new number, and therefore a new tab.
    pub fn app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = app_name.into();
        self
    }

    /// Where to connect, as `host:port`. Default `127.0.0.1:13991`.
    ///
    /// The GUI binds loopback unless its `listen_all_interfaces` setting is turned on, so a
    /// non-local address only works if it has been.
    pub fn address(mut self, address: impl Into<String>) -> Self {
        self.address = address.into();
        self
    }

    /// How many entries may queue up while the GUI is unreachable. Default 1000.
    ///
    /// Once full, the **oldest** queued entry is discarded to make room, matching the Go client.
    /// Discards are reported through [`on_error`](ClientBuilder::on_error) and counted by
    /// [`Client::dropped_count`].
    pub fn buffer_size(mut self, buffer_size: usize) -> Self {
        self.buffer_size = buffer_size;
        self
    }

    /// Whether [`Client::close`] tries to drain the queue before returning. Default `true`.
    ///
    /// Draining needs a live connection: if the GUI is unreachable when the client shuts down,
    /// whatever is queued is lost either way, and close returns without waiting.
    pub fn flush_on_close(mut self, flush_on_close: bool) -> Self {
        self.flush_on_close = flush_on_close;
        self
    }

    /// How long a close may spend draining. Default 5 seconds.
    pub fn flush_timeout(mut self, flush_timeout: Duration) -> Self {
        self.flush_timeout = flush_timeout;
        self
    }

    /// How long to wait for a connection to be established. Default 10 seconds.
    ///
    /// Also used as the socket write timeout, so a half-open connection cannot wedge the worker
    /// indefinitely.
    pub fn connect_timeout(mut self, connect_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self
    }

    /// How long to wait between connection attempts. Default 5 seconds, as in both siblings.
    ///
    /// Deliberately a fixed delay rather than exponential backoff: the usual case is an application
    /// already running when the GUI is opened, and backoff would make attaching take progressively
    /// longer for no benefit on a loopback connection.
    pub fn reconnect_delay(mut self, reconnect_delay: Duration) -> Self {
        self.reconnect_delay = reconnect_delay;
        self
    }

    /// Installs a callback for problems the worker hits.
    ///
    /// Nothing is reported anywhere without this: a logging library that writes to stderr on its
    /// own initiative is a nuisance, and one that does so while it *is* the `log` backend risks
    /// recursion. But the sibling clients discard these entirely, which makes "no logs are
    /// appearing" undiagnosable from inside the application, so the seam exists.
    ///
    /// Called from the worker thread, or from a logging thread when a queue overflow happens. Keep
    /// it cheap and non-blocking.
    pub fn on_error(mut self, on_error: impl Fn(&Error) + Send + Sync + 'static) -> Self {
        self.on_error = Some(Arc::new(on_error));
        self
    }

    /// Builds the client and starts its worker thread.
    ///
    /// There is no separate `open()` to remember, unlike the Go and C++ clients: a client that
    /// silently discards everything until opened is a trap, and Rust has `Drop` to handle the other
    /// end.
    pub fn build(self) -> Client {
        let config = Arc::new(Config {
            app_name: self.app_name,
            address: self.address,
            flush_on_close: self.flush_on_close,
            flush_timeout: self.flush_timeout,
            connect_timeout: self.connect_timeout,
            reconnect_delay: self.reconnect_delay,
            on_error: self.on_error,
        });
        let queue = Arc::new(Queue::new(self.buffer_size));

        let handle = {
            let config = Arc::clone(&config);
            let queue = Arc::clone(&queue);
            std::thread::Builder::new()
                .name("ecapplog".to_owned())
                .spawn(move || worker(config, queue))
                .ok()
        };

        Client {
            inner: Arc::new(Inner {
                config,
                queue,
                handle: Mutex::new(handle),
            }),
        }
    }
}

struct Inner {
    config: Arc<Config>,
    queue: Arc<Queue>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Inner {
    fn close(&self) {
        self.queue.shutdown();
        // Taking the handle makes this idempotent, and makes a manual `close()` followed by the
        // implicit one from `Drop` harmless.
        let handle = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}

/// A connection to the ECAppLog GUI.
///
/// Cloning is cheap and shares one worker thread and one queue, so a clone can be handed to every
/// part of an application. The worker shuts down, draining what it can, when the last clone is
/// dropped -- or earlier, on an explicit [`Client::close`].
///
/// Logging never blocks on the network. Entries go into a bounded queue that the worker drains;
/// while the GUI is unreachable they accumulate there and are sent once it appears.
///
/// ```no_run
/// use ecapplog::{Client, Priority, Record};
///
/// let client = Client::builder().app_name("my-service").build();
///
/// client.info("app", "started");
/// client.log(
///     Record::new(Priority::Error, "app", "could not reach the database")
///         .original_category("app.storage")
///         .extra_category("db"),
/// );
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client with every default, already running.
    pub fn new() -> Self {
        ClientBuilder::default().build()
    }

    /// Starts configuring a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Queues an entry.
    ///
    /// Never blocks on the network and never fails. If the queue is full the oldest entry is
    /// discarded and reported through the `on_error` callback.
    pub fn log(&self, record: Record) {
        let capacity = self.inner.queue.capacity();
        if let Some(total) = self.inner.queue.push(record) {
            // Reported here rather than inside the queue so the callback never runs under the lock.
            self.inner
                .config
                .report(&Error::QueueFull { capacity, total });
        }
    }

    /// Queues an entry stamped with the current time.
    pub fn log_now(
        &self,
        priority: Priority,
        category: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.log(Record::new(priority, category, message));
    }

    /// Logs at [`Priority::Trace`].
    pub fn trace(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Trace, category, message);
    }

    /// Logs at [`Priority::Debug`].
    pub fn debug(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Debug, category, message);
    }

    /// Logs at [`Priority::Information`].
    pub fn info(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Information, category, message);
    }

    /// Logs at [`Priority::Notice`].
    pub fn notice(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Notice, category, message);
    }

    /// Logs at [`Priority::Warning`].
    pub fn warning(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Warning, category, message);
    }

    /// Logs at [`Priority::Error`].
    pub fn error(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Error, category, message);
    }

    /// Logs at [`Priority::Critical`].
    pub fn critical(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Critical, category, message);
    }

    /// Logs at [`Priority::Fatal`].
    pub fn fatal(&self, category: impl Into<String>, message: impl Into<String>) {
        self.log_now(Priority::Fatal, category, message);
    }

    /// Blocks until every queued entry has been written, or `timeout` elapses. Returns whether it
    /// drained.
    ///
    /// "Written" means handed to the operating system. The ECAppLog server never acknowledges
    /// anything -- it never writes a byte back to the client -- so no stronger guarantee is
    /// available to any client of this protocol.
    pub fn flush(&self, timeout: Duration) -> bool {
        self.inner.queue.flush(timeout)
    }

    /// How many entries this client has discarded because the queue was full.
    ///
    /// A non-zero count almost always means the GUI is not running.
    pub fn dropped_count(&self) -> u64 {
        self.inner.queue.dropped_count()
    }

    /// Shuts the worker down, draining the queue first if
    /// [`flush_on_close`](ClientBuilder::flush_on_close) is set.
    ///
    /// Idempotent, and called automatically when the last clone of the client is dropped. Entries
    /// logged after this are counted as dropped.
    pub fn close(&self) {
        self.inner.close();
    }
}

/// Why a write did not happen.
enum WriteFailure {
    /// The entry itself is unsendable. Report it and move on; retrying cannot help.
    Discard(Error),
    /// The connection is broken. Report it, put the entry back, reconnect.
    Connection(Error),
}

fn worker(config: Arc<Config>, queue: Arc<Queue>) {
    let banner = proto::banner_payload(&config.app_name);
    let mut frame = Vec::new();
    let mut json = Vec::new();

    // Set the first time shutdown is observed, and from then on it bounds everything the worker
    // does -- the connect attempt included -- so `close()` can never take longer than the
    // configured flush timeout.
    let mut flush_deadline: Option<Instant> = None;

    'reconnect: loop {
        if queue.is_shutdown() {
            // Nothing queued, or the caller does not want a drain: leave straight away.
            if !config.flush_on_close || queue.is_empty() {
                break;
            }
            let deadline =
                *flush_deadline.get_or_insert_with(|| Instant::now() + config.flush_timeout);
            if Instant::now() >= deadline {
                break;
            }
            // Otherwise fall through and connect, even though shutdown was requested. A short
            // program that logs and exits immediately never gets its worker connected otherwise,
            // and flushing on close would deliver nothing at all.
        }

        let mut stream = match connect(&config, flush_deadline) {
            Ok(stream) => stream,
            Err(error) => {
                config.report(&error);
                if queue.sleep_or_shutdown(config.reconnect_delay) {
                    break 'reconnect;
                }
                continue 'reconnect;
            }
        };

        // The banner must be the first thing on a fresh connection, and must be sent exactly once
        // per connection: the server closes the socket on a second one.
        proto::encode_frame(proto::CMD_BANNER, &banner, &mut frame);
        if let Err(source) = stream.write_all(&frame) {
            config.report(&Error::Write { source });
            if queue.sleep_or_shutdown(config.reconnect_delay) {
                break 'reconnect;
            }
            continue 'reconnect;
        }

        loop {
            if queue.is_shutdown() {
                if !config.flush_on_close {
                    break 'reconnect;
                }
                let deadline =
                    *flush_deadline.get_or_insert_with(|| Instant::now() + config.flush_timeout);
                if Instant::now() >= deadline {
                    break 'reconnect;
                }
            }

            match queue.pop(POLL_INTERVAL) {
                Pop::Item(record) => {
                    match write_record(&mut stream, &record, &mut json, &mut frame) {
                        Ok(()) => queue.finish(),
                        Err(WriteFailure::Discard(error)) => {
                            config.report(&error);
                            queue.finish();
                        }
                        Err(WriteFailure::Connection(error)) => {
                            config.report(&error);
                            // Put it back at the head so a failed write costs no entries, unlike
                            // the Go client, which loses whatever was in flight.
                            let capacity = queue.capacity();
                            let requeue = queue.push_front(record);
                            queue.finish();
                            if let Some(total) = requeue {
                                config.report(&Error::QueueFull { capacity, total });
                            }
                            if queue.sleep_or_shutdown(config.reconnect_delay) {
                                break 'reconnect;
                            }
                            continue 'reconnect;
                        }
                    }
                }
                // Drained, and shutdown was requested.
                Pop::Shutdown => break 'reconnect,
                // Timed out with nothing to do; loop round to re-check the flush deadline.
                Pop::Idle => {}
            }
        }
    }
}

/// Connects, with the attempt bounded by `flush_deadline` when one is in force.
///
/// During a flush the connect timeout cannot be allowed to outlast the flush budget, or `close()`
/// would block for the full connect timeout on an address that is merely black-holed.
fn connect(config: &Config, flush_deadline: Option<Instant>) -> Result<TcpStream, Error> {
    let timeout = match flush_deadline {
        Some(deadline) => deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
            .min(config.connect_timeout)
            // `connect_timeout` rejects a zero duration, and a flush that has just run out of
            // budget is about to give up anyway.
            .max(Duration::from_millis(1)),
        None => config.connect_timeout,
    };

    let addresses: Vec<SocketAddr> = config
        .address
        .to_socket_addrs()
        .map_err(|source| Error::Resolve {
            address: config.address.clone(),
            source,
        })?
        .collect();

    let mut last_error = None;
    for address in &addresses {
        match TcpStream::connect_timeout(address, timeout) {
            Ok(stream) => {
                // Log entries are small and latency matters more than packet count here; the Go
                // client sets this too, the C++ one does not.
                let _ = stream.set_nodelay(true);
                // Without this a half-open connection blocks the worker forever, taking `close()`
                // down with it.
                let _ = stream.set_write_timeout(Some(config.connect_timeout));
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(Error::Connect {
        address: config.address.clone(),
        source: last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "the address resolved to no socket addresses",
            )
        }),
    })
}

fn write_record(
    stream: &mut TcpStream,
    record: &Record,
    json: &mut Vec<u8>,
    frame: &mut Vec<u8>,
) -> Result<(), WriteFailure> {
    json.clear();
    serde_json::to_writer(&mut *json, &LogPayload::from_record(record))
        .map_err(|source| WriteFailure::Discard(Error::Serialize { source }))?;

    // A frame over the limit makes the server close the connection, which would take down every
    // entry queued behind this one as well. Dropping the one oversized entry is the lesser loss.
    if json.len() > MAX_PAYLOAD_SIZE {
        return Err(WriteFailure::Discard(Error::PayloadTooLarge {
            size: json.len(),
            limit: MAX_PAYLOAD_SIZE,
        }));
    }

    proto::encode_frame(proto::CMD_LOG, json, frame);
    stream
        .write_all(frame)
        .map_err(|source| WriteFailure::Connection(Error::Write { source }))
}
