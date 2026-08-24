//! End-to-end checks against a stand-in server on loopback.
//!
//! These exist because the real server never acknowledges anything, so nothing about the byte
//! stream can be verified from inside the client at runtime. A listener that decodes frames the way
//! `ecapplog/src/Server.cpp` does is the only way to catch a framing regression before it turns
//! into "the GUI shows nothing" on someone's machine.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use ecapplog::{Client, Priority, Record};

/// One decoded frame: the command byte and its payload.
struct Frame {
    command: u8,
    payload: Vec<u8>,
}

/// Reads frames the way the server does: `u8` command, `u32` big-endian length, then the payload.
///
/// Stops once `wanted` frames have arrived or `deadline` passes, so a failure is a useful assertion
/// rather than a hung test.
fn read_frames(stream: &mut TcpStream, wanted: usize, deadline: Duration) -> Vec<Frame> {
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .expect("set_read_timeout");

    let start = Instant::now();
    let mut buffer = Vec::new();
    let mut frames = Vec::new();
    let mut chunk = [0u8; 8192];

    while frames.len() < wanted && start.elapsed() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => break, // the client hung up
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("read failed: {e}"),
        }

        // Decode everything complete that has accumulated. Frames may be split or coalesced
        // arbitrarily across reads, which is exactly what this loop has to tolerate.
        let mut offset = 0;
        while buffer.len() >= offset + 5 {
            let length = u32::from_be_bytes([
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
                buffer[offset + 4],
            ]) as usize;
            if buffer.len() < offset + 5 + length {
                break;
            }
            frames.push(Frame {
                command: buffer[offset],
                payload: buffer[offset + 5..offset + 5 + length].to_vec(),
            });
            offset += 5 + length;
        }
        buffer.drain(..offset);
    }

    frames
}

fn listener() -> (TcpListener, String) {
    // Port 0 lets the OS pick, so the tests never collide with a real ECAppLog instance.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("local_addr").to_string();
    (listener, address)
}

#[test]
fn sends_a_banner_then_log_frames() {
    let (listener, address) = listener();

    let client = Client::builder()
        .app_name("TEST-APP")
        .address(&address)
        .build();
    client.info("app", "first");
    client.log(
        Record::new(Priority::Error, "app", "second")
            .original_category("app.inner")
            .extra_category("other"),
    );

    let (mut stream, _) = listener.accept().expect("accept");
    let frames = read_frames(&mut stream, 3, Duration::from_secs(10));
    assert_eq!(frames.len(), 3, "expected a banner and two log frames");

    // The banner: command 99, the 9-byte prefix including its trailing space, then the app name.
    assert_eq!(frames[0].command, 99);
    assert_eq!(frames[0].payload, b"ECAPPLOG TEST-APP");

    assert_eq!(frames[1].command, 0);
    let first: serde_json::Value = serde_json::from_slice(&frames[1].payload).expect("valid JSON");
    assert_eq!(first["priority"], "INFORMATION");
    assert_eq!(first["category"], "app");
    assert_eq!(first["message"], "first");
    assert!(first.get("source").is_none(), "empty fields are omitted");

    assert_eq!(frames[2].command, 0);
    let second: serde_json::Value = serde_json::from_slice(&frames[2].payload).expect("valid JSON");
    assert_eq!(second["priority"], "ERROR");
    assert_eq!(second["original_category"], "app.inner");
    assert_eq!(second["extra_categories"], serde_json::json!(["other"]));
}

/// The timestamp is the one field the server accepts silently and then ignores when it is wrong, so
/// verify its exact shape on the wire rather than only in the formatter's own unit tests.
#[test]
fn timestamps_are_in_the_format_the_server_parses() {
    let (listener, address) = listener();

    let client = Client::builder().address(&address).build();
    client.info("app", "x");

    let (mut stream, _) = listener.accept().expect("accept");
    let frames = read_frames(&mut stream, 2, Duration::from_secs(10));
    assert_eq!(frames.len(), 2);

    let entry: serde_json::Value = serde_json::from_slice(&frames[1].payload).expect("valid JSON");
    let time = entry["time"].as_str().expect("a string timestamp");

    // "yyyy-MM-ddThh:mm:ss.zzz" -- no zone designator, exactly three fractional digits.
    assert_eq!(time.len(), 23, "{time}");
    assert!(!time.ends_with('Z'), "{time} must not be RFC 3339");
    assert!(!time.contains('+'), "{time} must not carry an offset");
    let (date, rest) = time.split_once('T').expect("a T separator");
    assert_eq!(date.len(), 10, "{time}");
    assert_eq!(rest.split('.').nth(1).map(str::len), Some(3), "{time}");
    assert!(
        rest.chars()
            .all(|c| c.is_ascii_digit() || c == ':' || c == '.'),
        "{time}"
    );
}

/// The behaviour that makes the client usable: entries logged while the GUI is not running are held
/// and delivered once it appears. Mirrors the Go client, whose ring buffer holds up to
/// `bufferSize` entries across a disconnect.
#[test]
fn queues_while_unreachable_and_delivers_on_connect() {
    // Bind to claim a port, then drop the listener so nothing is listening on it.
    let (listener, address) = listener();
    drop(listener);

    let client = Client::builder()
        .app_name("BACKLOG")
        .address(&address)
        // Keep the retry brisk so the test does not sit through the 5 second default.
        .reconnect_delay(Duration::from_millis(50))
        .connect_timeout(Duration::from_millis(500))
        .build();

    for i in 0..10 {
        client.info("app", format!("queued {i}"));
    }
    assert_eq!(
        client.dropped_count(),
        0,
        "nothing should be dropped below capacity, connected or not"
    );

    // Now start listening. The worker should connect on its next attempt and drain the backlog.
    let listener = TcpListener::bind(&address).expect("rebind");
    let (mut stream, _) = listener.accept().expect("accept");
    let frames = read_frames(&mut stream, 11, Duration::from_secs(15));

    assert_eq!(frames.len(), 11, "expected the banner plus all 10 entries");
    assert_eq!(frames[0].payload, b"ECAPPLOG BACKLOG");

    // In order, and complete.
    for (i, frame) in frames[1..].iter().enumerate() {
        let entry: serde_json::Value = serde_json::from_slice(&frame.payload).expect("valid JSON");
        assert_eq!(entry["message"], format!("queued {i}"));
    }
}

/// Past capacity the *oldest* entries go, not the newest -- the Go client's drop-oldest ring buffer
/// semantics, which keep the most recent context when a burst overruns the queue.
#[test]
fn drops_the_oldest_when_the_queue_overruns() {
    let (listener, address) = listener();
    drop(listener);

    let client = Client::builder()
        .app_name("OVERRUN")
        .address(&address)
        .buffer_size(4)
        .reconnect_delay(Duration::from_millis(50))
        .connect_timeout(Duration::from_millis(500))
        .build();

    for i in 0..10 {
        client.info("app", format!("entry {i}"));
    }
    assert_eq!(client.dropped_count(), 6, "10 logged into a queue of 4");

    let listener = TcpListener::bind(&address).expect("rebind");
    let (mut stream, _) = listener.accept().expect("accept");
    let frames = read_frames(&mut stream, 5, Duration::from_secs(15));

    assert_eq!(frames.len(), 5, "the banner plus the surviving 4");
    let messages: Vec<String> = frames[1..]
        .iter()
        .map(|f| {
            serde_json::from_slice::<serde_json::Value>(&f.payload).expect("valid JSON")["message"]
                .as_str()
                .expect("a string message")
                .to_owned()
        })
        .collect();
    assert_eq!(messages, ["entry 6", "entry 7", "entry 8", "entry 9"]);
}

/// A dropped entry must be reported, not swallowed the way both sibling clients swallow it.
#[test]
fn reports_dropped_entries_through_the_error_callback() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (listener, address) = listener();
    drop(listener);

    let drops = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&drops);

    let client = Client::builder()
        .address(&address)
        .buffer_size(2)
        .reconnect_delay(Duration::from_millis(50))
        .connect_timeout(Duration::from_millis(500))
        .on_error(move |error| {
            if matches!(error, ecapplog::Error::QueueFull { .. }) {
                seen.fetch_add(1, Ordering::Relaxed);
            }
        })
        .build();

    for i in 0..5 {
        client.info("app", format!("entry {i}"));
    }

    assert_eq!(drops.load(Ordering::Relaxed), 3);
    assert_eq!(client.dropped_count(), 3);
}

/// Closing must not block on the reconnect delay. The C++ client can hang here indefinitely when
/// nothing is listening.
#[test]
fn close_returns_promptly_when_the_server_is_unreachable() {
    let (listener, address) = listener();
    drop(listener);

    let client = Client::builder()
        .address(&address)
        .reconnect_delay(Duration::from_secs(30))
        .connect_timeout(Duration::from_millis(500))
        .build();
    client.info("app", "never delivered");

    // Let the worker fail its first connection attempt and settle into the 30 second delay.
    std::thread::sleep(Duration::from_millis(600));

    let start = Instant::now();
    client.close();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "close waited out the reconnect delay ({:?})",
        start.elapsed()
    );
}

/// Dropping the last clone shuts the worker down and flushes, so a short-lived program does not
/// need the trailing sleep both sibling samples resort to.
#[test]
fn dropping_the_client_flushes_what_is_queued() {
    let (listener, address) = listener();

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        read_frames(&mut stream, 4, Duration::from_secs(10))
            .into_iter()
            .filter(|f| f.command == 0)
            .count()
    });

    {
        let client = Client::builder().address(&address).build();
        let clone = client.clone();
        client.info("app", "one");
        clone.info("app", "two");
        clone.info("app", "three");
        // Both handles go out of scope here; the last drop closes and flushes.
    }

    assert_eq!(handle.join().expect("listener thread"), 3);
}

/// A GUI restart mid-run must be picked up, with a fresh banner on the new connection.
///
/// The banner is per-connection, not per-process: the server rejects a second one on the same
/// socket, but a reconnect that omits it lands in a tab named after the ephemeral port instead of
/// the application.
#[test]
fn reconnects_and_re_sends_the_banner() {
    let (listener, address) = listener();

    let client = Client::builder()
        .app_name("RECONNECT")
        .address(&address)
        .reconnect_delay(Duration::from_millis(50))
        .connect_timeout(Duration::from_millis(500))
        .build();

    client.info("app", "before");
    let (mut stream, _) = listener.accept().expect("first accept");
    let frames = read_frames(&mut stream, 2, Duration::from_secs(10));
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].command, 99);
    assert_eq!(frames[0].payload, b"ECAPPLOG RECONNECT");

    // Hang up on the client, the way closing the GUI would.
    drop(stream);

    // A write into a socket the peer has closed can succeed once -- it only has to reach the local
    // send buffer -- so the worker does not learn the connection is gone until a later write fails.
    // Keep logging while polling for the reconnect rather than assuming one entry is enough.
    listener.set_nonblocking(true).expect("set_nonblocking");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reconnected = None;
    while Instant::now() < deadline {
        client.info("app", "after");
        match listener.accept() {
            Ok((stream, _)) => {
                reconnected = Some(stream);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => panic!("second accept failed: {e}"),
        }
    }

    let mut stream = reconnected.expect("the client never reconnected");
    // Back to blocking reads now that the socket is in hand.
    stream.set_nonblocking(false).expect("set_nonblocking");

    let banner = read_frames(&mut stream, 1, Duration::from_secs(10))
        .into_iter()
        .next()
        .expect("a banner on the new connection");
    assert_eq!(
        banner.command, 99,
        "the banner must be re-sent on reconnect"
    );
    assert_eq!(banner.payload, b"ECAPPLOG RECONNECT");

    // And entries keep flowing on the new connection.
    client.info("app", "later");
    let frames = read_frames(&mut stream, 1, Duration::from_secs(10));
    assert!(!frames.is_empty(), "no entries after reconnecting");
    assert!(frames.iter().all(|f| f.command == 0));
}
