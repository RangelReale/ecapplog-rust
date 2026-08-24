# ECAppLog-rust

Rust client for the [ECAppLog](https://github.com/RangelReale/ecapplog) logging GUI, alongside the
[Go](https://github.com/RangelReale/ecapplog-go) and
[C++](https://github.com/RangelReale/ecapplog-cpp) clients.

```toml
[dependencies]
ecapplog = "1.0"
```

## Example

```rust
use ecapplog::{Client, Priority, Record};

fn main() {
    let client = Client::builder().app_name("ECALRS-SAMPLE").build();

    for i in 0..30 {
        client.log(
            Record::new(Priority::Debug, "app", format!("First log: {i}"))
                .original_category("app.internal"),
        );
        client.info("app", format!("Second log: {i}"));
        client.log(
            Record::new(Priority::Error, "app", format!("Third log: {i}"))
                .extra_category("app_third"),
        );
    }
}
```

No trailing sleep is needed, unlike the Go and C++ samples: dropping the client drains the queue
first.

## Standard logging facades

Both adapters are optional features, so an application that only wants the direct API does not pull
in a logging facade it never calls.

### `log`

```toml
ecapplog = { version = "1.0", features = ["log"] }
```

```rust
ecapplog::LogLogger::builder()
    .app_name("my-service")
    .max_level(log::LevelFilter::Debug)
    .init()?;

log::info!("listening on {}", 8080);
```

The category — the GUI tab — defaults to the record's target, so tabs mirror the module tree. Use
`.category("myapp")` for a single tab, or `.category_fn(..)` for anything else. The call site goes
into `source`, and the module path into `original_category` when it is not already the category, so
the GUI renders `category [module::path]`.

### `tracing`

```toml
ecapplog = { version = "1.0", features = ["tracing"] }
```

```rust
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(ecapplog::TracingLayer::builder().app_name("my-service").build())
    .init();

tracing::info!(port = 8080, "listening");
```

The protocol carries one flat message string and has no notion of structured fields. What makes
`tracing` fit anyway is that the GUI pretty-prints a `source` that happens to contain JSON, so the
layer puts the event's fields and the enclosing spans there:

```json
{
  "at": "src/http.rs:41",
  "fields": { "method": "GET", "path": "/health" },
  "spans": [ { "name": "handle_request", "fields": { "request_id": "abc123" } } ]
}
```

Click a row in the GUI to see it laid out in the details pane. For level filtering, compose a real
filter — `layer.with_filter(EnvFilter::from_default_env())` — rather than the layer's own
`max_level`.

## Behaviour

* **Logging never blocks on the network.** Entries go into a bounded queue that a background thread
  drains.
* **Entries survive the GUI not being there.** While it is unreachable they accumulate in the queue
  and are delivered as soon as it appears, so an application can be started first and attached to
  later. This also covers restarting the GUI mid-run.
* **Past capacity the oldest entry is discarded**, keeping the most recent context. The default
  capacity is 1000, matching the Go client. Install `on_error` or read `dropped_count()` to notice.
* **Delivery is best-effort by design.** The protocol is one-directional — the server never writes a
  byte back — so nothing can be acknowledged and a rejected entry is invisible to the client.

## Options

| Builder method | Default | |
|---|---|---|
| `app_name` | `ECAPPLOG-RUST` | The GUI labels the connection `<app_name>:<n>`. |
| `address` | `127.0.0.1:13991` | The GUI listens on loopback unless its `listen_all_interfaces` setting is on. |
| `buffer_size` | `1000` | Entries held while disconnected before the oldest are discarded. |
| `flush_on_close` | `true` | Whether closing drains the queue first. |
| `flush_timeout` | 5 s | Ceiling on how long closing may take. |
| `connect_timeout` | 10 s | Also the socket write timeout. |
| `reconnect_delay` | 5 s | Fixed, not exponential — see below. |
| `on_error` | none | Connect/write failures and dropped entries. |

Reconnect is a fixed delay on purpose. The usual case is an application already running when the GUI
is opened, and backoff would make attaching take progressively longer for no benefit on a loopback
connection.

## Priorities

`TRACE`, `DEBUG`, `INFORMATION`, `NOTICE`, `WARNING`, `FATAL`, `CRITICAL`, `ERROR` — note that the
informational one is spelled `INFORMATION`, not `INFO`. `Priority::Custom` sends an arbitrary
string, which the server accepts but gives no colour and does not route to its `ERROR` tab.

Only `WARNING`, `ERROR`, `CRITICAL` and `FATAL` are copied into that tab. `Priority::is_error` and
`is_error_or_warning` mirror the server's own classification.

Category names to avoid, because the server uses them itself: `ALL`, `ERROR`, `ECAPPLOG` and
`<unknown>` (which is what an empty category becomes). The GUI also sorts all-uppercase category
names ahead of the rest, so casing affects tab order.

## Protocol notes

Plain TCP. Each frame is a `u8` command, a `u32` big-endian payload length, then the payload:

```text
99  <u32 len>  "ECAPPLOG <app name>"     -- banner, once per connection
 0  <u32 len>  {"time":...,"priority":...,"category":...,"message":...}
```

Two details bite anyone reimplementing this:

* **Timestamps are not RFC 3339.** The server parses the literal Qt format
  `yyyy-MM-ddThh:mm:ss.zzz` in UTC, rejecting any `Z` or numeric offset and any fractional precision
  other than exactly three digits. A rejected timestamp is *not* an error — the server silently
  substitutes its own arrival time, so a client emitting RFC 3339 looks like it works while every
  row shows the wrong time.
* **A frame over 16 MiB closes the connection**, taking everything queued behind it. Oversized
  entries are dropped client-side here instead.

`cargo run --example mock_server` prints what a client actually puts on the wire, applying the same
validation the real server does, which is handy when the GUI is not built.

## Comparison with the other clients

Behaviour is deliberately the same where it matters — same protocol, same defaults for buffer size
and reconnect delay, same priorities. It differs where the sibling implementations have sharp edges:

| | Go | C++ | Rust |
|---|---|---|---|
| Start | explicit `Open()` | explicit `open()` | `build()` starts it |
| Shutdown | `Close()`, draining opt-in | pending entries discarded | drains by default, deadline-bounded |
| Queue | ring, 1000, drop-oldest | unbounded, never drops | bounded, drop-oldest, counted |
| Errors | reporting hook with an empty body | swallowed | `on_error` + `dropped_count()` |
| Entry in flight when a write fails | lost | re-queued | re-queued |
| `original_category` | yes | unsupported | yes |
| Logging-facade adapters | none | none | `log`, `tracing` |

## License

MIT

## Author

Rangel Reale (rangelreale@gmail.com)
