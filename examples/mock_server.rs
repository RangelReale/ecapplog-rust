//! A stand-in for the ECAppLog GUI, for checking what a client actually puts on the wire.
//!
//! Run it, then run any other example in another terminal:
//!
//! ```text
//! cargo run --example mock_server
//! cargo run --example simple
//! ```
//!
//! It decodes frames the way `ecapplog/src/Server.cpp` does and prints them, so the bytes can be
//! inspected without building the Qt GUI. It also applies the server's own validation, so a client
//! bug that the real GUI would swallow silently shows up here as a complaint.

use std::io::Read;
use std::net::{TcpListener, TcpStream};

const CMD_BANNER: u8 = 99;
const CMD_LOG: u8 = 0;
const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

fn main() {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:13991".to_owned());

    let listener = TcpListener::bind(&address).expect("bind");
    println!("listening on {address}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || handle(stream));
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

fn handle(mut stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "<unknown>".to_owned());
    println!("--- connected: {peer}");

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut seen_banner = false;
    let mut count = 0usize;

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            // A client exiting without a graceful shutdown resets the connection, which is
            // normal rather than a fault -- the real server treats any disconnect the same way.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(e) => {
                eprintln!("read failed: {e}");
                break;
            }
        }

        // Frames may be split or coalesced across reads, so decode whatever is complete and leave
        // the remainder for the next read.
        let mut offset = 0;
        while buffer.len() >= offset + 5 {
            let command = buffer[offset];
            let length = u32::from_be_bytes([
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
                buffer[offset + 4],
            ]) as usize;

            if length > MAX_PAYLOAD_SIZE {
                println!(
                    "!!! payload of {length} bytes exceeds the limit; the real server would disconnect here"
                );
                return;
            }
            if buffer.len() < offset + 5 + length {
                break;
            }
            let payload = &buffer[offset + 5..offset + 5 + length];
            offset += 5 + length;

            match command {
                CMD_BANNER => {
                    if seen_banner {
                        println!("!!! a second banner; the real server would disconnect here");
                        return;
                    }
                    seen_banner = true;
                    match payload.strip_prefix(b"ECAPPLOG ") {
                        Some(name) => {
                            println!("banner: app name {:?}", String::from_utf8_lossy(name));
                        }
                        None => {
                            println!(
                                "!!! banner without the `ECAPPLOG ` prefix; the real server would disconnect here"
                            );
                            return;
                        }
                    }
                }
                CMD_LOG => {
                    count += 1;
                    match serde_json::from_slice::<serde_json::Value>(payload) {
                        Ok(entry) => {
                            let get = |k: &str| {
                                entry
                                    .get(k)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_owned()
                            };
                            let time = get("time");
                            // The server parses this literal format and silently falls back to its
                            // own arrival time when it does not match, so flag it loudly here.
                            let plausible = time.len() == 23
                                && time.as_bytes()[10] == b'T'
                                && time.as_bytes()[19] == b'.';
                            let warning = if plausible { "" } else { "  <-- BAD TIMESTAMP" };
                            println!(
                                "{time} [{}] {}: {}{warning}",
                                get("priority"),
                                get("category"),
                                get("message"),
                            );
                            for key in ["source", "original_category", "color", "bgcolor"] {
                                let value = get(key);
                                if !value.is_empty() {
                                    println!("        {key}: {value}");
                                }
                            }
                            if let Some(extra) = entry.get("extra_categories") {
                                println!("        extra_categories: {extra}");
                            }
                        }
                        Err(e) => println!(
                            "!!! invalid JSON ({e}): {:?}",
                            String::from_utf8_lossy(payload)
                        ),
                    }
                }
                other => println!("!!! unknown command {other}"),
            }
        }
        buffer.drain(..offset);
    }

    println!("--- disconnected: {peer} after {count} entries");
}
