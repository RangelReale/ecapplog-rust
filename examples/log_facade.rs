//! Routing the `log` facade to the GUI, so existing `log::info!` call sites need no changes.
//!
//! `cargo run --example log_facade --features log`

use std::time::Duration;

use ecapplog::LogLogger;

mod storage {
    pub fn connect() {
        // The category defaults to the record's target, which is the module path -- so this lands
        // in a `log_facade::storage` tab rather than being mixed in with everything else.
        log::debug!("opening a connection");
        log::error!("connection refused");
    }
}

mod http {
    pub fn serve() {
        log::info!("listening on port {}", 8080);
        log::warn!("no TLS certificate configured");
    }
}

fn main() {
    let logger = LogLogger::builder()
        .app_name("ECALRS-LOG")
        .max_level(log::LevelFilter::Trace)
        .build();

    // Keep a handle so the example can flush at the end; `init()` alone would move it into the
    // global logger, where nothing can reach it again.
    let client = logger.client().clone();
    log::set_max_level(log::LevelFilter::Trace);
    log::set_boxed_logger(Box::new(logger)).expect("no logger installed yet");

    log::trace!("starting up");
    http::serve();
    storage::connect();

    // A fixed category instead of per-module tabs would be:
    //     LogLogger::builder().category("myapp").init()
    // and an arbitrary mapping is available through `.category_fn(..)`.

    log::logger().flush();
    if !client.flush(Duration::from_secs(5)) {
        eprintln!("ecapplog: could not deliver everything within 5s -- is the GUI running?");
    }
}
