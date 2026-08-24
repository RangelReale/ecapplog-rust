//! Per-entry colour overrides, and every priority the protocol defines.
//!
//! `cargo run --example colors`

use std::time::Duration;

use ecapplog::{Client, Priority, Record};

fn main() {
    let client = Client::builder()
        .app_name("ECALRS-COLORS")
        .on_error(|error| eprintln!("ecapplog: {error}"))
        .build();

    // All eight priorities. Note that only WARNING, ERROR, CRITICAL and FATAL are copied into the
    // GUI's synthetic ERROR tab, and that the informational one is spelled INFORMATION.
    for priority in Priority::ALL {
        let in_error_tab = if priority.is_error_or_warning() {
            " (also in the ERROR tab)"
        } else {
            ""
        };
        client.log_now(
            priority.clone(),
            "priorities",
            format!("{priority}{in_error_tab}"),
        );
    }

    // `color` replaces the priority's own colour; `bg_color` paints the row behind it. Anything
    // Qt's colour parser takes works, which is broader than the `#rrggbb` the protocol docs
    // mention -- an unparseable value is ignored by the server, silently.
    let swatches = [
        ("#ff5555", "", "hex foreground"),
        ("", "#f9f9f9", "hex background"),
        ("#ffffff", "#8b0000", "both"),
        ("steelblue", "", "an SVG colour name"),
        ("not-a-colour", "", "invalid: the server ignores it"),
    ];

    for (color, bg_color, what) in swatches {
        client.log(
            Record::new(Priority::Information, "colors", what)
                .color(color)
                .bg_color(bg_color),
        );
    }

    // The details pane pretty-prints a `source` that happens to be JSON, which makes it a good
    // place for context the flat message string has no room for.
    client.log(
        Record::new(
            Priority::Notice,
            "colors",
            "see the Source column and details pane",
        )
        .source(r#"{"request_id":"abc123","attempt":2,"upstream":"db-1"}"#),
    );

    if !client.flush(Duration::from_secs(5)) {
        eprintln!("ecapplog: could not deliver everything within 5s -- is the GUI running?");
    }
}
