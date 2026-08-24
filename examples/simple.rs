//! The counterpart of `ecapplog-go/sample/simple/main.go`.
//!
//! Start the ECAppLog GUI, then `cargo run --example simple`. It works in the other order too: the
//! entries queue up and are delivered once the GUI appears.

use std::time::Duration;

use ecapplog::{Client, Priority, Record};

fn main() {
    let client = Client::builder()
        .app_name("ECALRS-SAMPLE")
        // Neither sibling client reports anything at all when the GUI is unreachable, which makes
        // an empty window impossible to explain. This is the seam that fixes that.
        .on_error(|error| eprintln!("ecapplog: {error}"))
        .build();

    for i in 0..30 {
        client.log(
            Record::new(Priority::Debug, "app", format!("First log: {i}"))
                // The GUI renders this as `app [app.internal]`.
                .original_category("app.internal"),
        );

        client.info("app", format!("Second log: {i}"));

        client.log(
            // Also copied into an `app_third` tab, and into the GUI's synthetic ERROR tab.
            Record::new(Priority::Error, "app", format!("Third log: {i}"))
                .extra_category("app_third"),
        );
    }

    // Unlike the Go and C++ samples, no trailing sleep is needed to avoid losing the tail: dropping
    // the client drains the queue first. This only reports whether the wait succeeded.
    if !client.flush(Duration::from_secs(5)) {
        eprintln!("ecapplog: could not deliver everything within 5s -- is the GUI running?");
    }

    println!("sent 90 entries; {} dropped", client.dropped_count());
}
