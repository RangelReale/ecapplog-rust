//! A [`tracing_subscriber`] layer, so `tracing::info!` and span context reach the GUI.
//!
//! The protocol has no notion of structured fields: an entry carries one flat message string. What
//! makes `tracing` map onto it well anyway is the `source` field, which the GUI pretty-prints when
//! it happens to contain JSON (`ecapplog/src/JsonFormat.cpp`). So the event's message becomes the
//! message, and its fields plus the enclosing spans are serialised as JSON into `source`, where the
//! details pane renders them properly instead of being flattened into the message text.

use std::fmt;
use std::time::Duration;

use serde_json::{Map, Value};
use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record as SpanRecord};
use tracing_core::{Event, Level, LevelFilter, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::client::Client;
use crate::priority::Priority;
use crate::record::Record as LogRecord;

/// Fields recorded on a span, stashed in the span's extensions until an event needs them.
struct SpanFields(Map<String, Value>);

/// How to choose a category -- that is, a GUI tab -- for each event.
enum CategorySource {
    /// The event's target, which is the module path unless the call site overrode it.
    Target,
    /// One fixed category for everything from this layer.
    Fixed(String),
    /// Whatever the closure decides.
    Custom(Box<dyn Fn(&Metadata<'_>) -> String + Send + Sync>),
}

/// Routes [`tracing`](https://docs.rs/tracing) events to ECAppLog.
///
/// ```no_run
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(ecapplog::TracingLayer::builder().app_name("my-service").build())
///     .init();
///
/// tracing::info!(port = 8080, "listening");
/// ```
pub struct TracingLayer {
    client: Client,
    level: LevelFilter,
    category: CategorySource,
    include_spans: bool,
    include_call_site: bool,
}

/// Builds a [`TracingLayer`].
pub struct TracingLayerBuilder {
    client: Option<Client>,
    app_name: Option<String>,
    level: LevelFilter,
    category: CategorySource,
    include_spans: bool,
    include_call_site: bool,
}

impl Default for TracingLayerBuilder {
    fn default() -> Self {
        Self {
            client: None,
            app_name: None,
            level: LevelFilter::TRACE,
            category: CategorySource::Target,
            include_spans: true,
            include_call_site: true,
        }
    }
}

impl TracingLayerBuilder {
    /// Uses an existing client, so an application can log through both this layer and the direct
    /// API over one connection.
    pub fn client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Sets the app name on the client this builder creates.
    ///
    /// Ignored when [`client`](TracingLayerBuilder::client) supplied one already.
    pub fn app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// The most verbose level to forward. Default [`LevelFilter::TRACE`].
    ///
    /// For anything more nuanced, leave this alone and compose a real filter --
    /// `layer.with_filter(EnvFilter::from_default_env())` -- rather than reimplementing one here.
    pub fn max_level(mut self, level: impl Into<LevelFilter>) -> Self {
        self.level = level.into();
        self
    }

    /// Sends everything to one category, instead of splitting tabs by target.
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = CategorySource::Fixed(category.into());
        self
    }

    /// Derives the category from each event's metadata.
    pub fn category_fn(
        mut self,
        f: impl Fn(&Metadata<'_>) -> String + Send + Sync + 'static,
    ) -> Self {
        self.category = CategorySource::Custom(Box::new(f));
        self
    }

    /// Whether to include the enclosing spans and their fields. Default `true`.
    pub fn include_spans(mut self, include_spans: bool) -> Self {
        self.include_spans = include_spans;
        self
    }

    /// Whether to include the call site (`file:line`). Default `true`.
    pub fn include_call_site(mut self, include_call_site: bool) -> Self {
        self.include_call_site = include_call_site;
        self
    }

    /// Builds the layer.
    pub fn build(self) -> TracingLayer {
        let client = self.client.unwrap_or_else(|| {
            let mut builder = Client::builder();
            if let Some(app_name) = self.app_name {
                builder = builder.app_name(app_name);
            }
            builder.build()
        });

        TracingLayer {
            client,
            level: self.level,
            category: self.category,
            include_spans: self.include_spans,
            include_call_site: self.include_call_site,
        }
    }
}

impl TracingLayer {
    /// Starts configuring a layer.
    pub fn builder() -> TracingLayerBuilder {
        TracingLayerBuilder::default()
    }

    /// The client this layer writes through.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Blocks until queued entries have been written, or `timeout` elapses.
    ///
    /// A layer has no `flush` hook of its own, so call this before exiting if the process ends
    /// promptly after its last event. Dropping the client flushes too; this is for when the layer
    /// has been handed to a global subscriber and so is never dropped.
    pub fn flush(&self, timeout: Duration) -> bool {
        self.client.flush(timeout)
    }

    fn category_for(&self, metadata: &Metadata<'_>) -> String {
        match &self.category {
            CategorySource::Target => metadata.target().to_owned(),
            CategorySource::Fixed(category) => category.clone(),
            CategorySource::Custom(f) => f(metadata),
        }
    }
}

/// Maps a [`tracing`](https://docs.rs/tracing) level onto a protocol priority.
///
/// `tracing` has five levels and the protocol has eight, so `NOTICE`, `CRITICAL` and `FATAL` are
/// unreachable through this layer. They stay available through [`Client`]'s own methods.
fn priority_for(level: &Level) -> Priority {
    match *level {
        Level::TRACE => Priority::Trace,
        Level::DEBUG => Priority::Debug,
        // Not `INFO`: the protocol spells it out.
        Level::INFO => Priority::Information,
        Level::WARN => Priority::Warning,
        Level::ERROR => Priority::Error,
    }
}

impl<S> Layer<S> for TracingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        *metadata.level() <= self.level
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.level)
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !self.include_spans {
            return;
        }
        let Some(span) = ctx.span(id) else { return };

        let mut fields = Map::new();
        let mut message = None;
        attrs.record(&mut JsonVisitor {
            fields: &mut fields,
            message: &mut message,
        });
        // A span opened as `span!(..., "text")` puts its text in a `message` field; keep it rather
        // than discarding it, since it is part of that span's identity.
        if let Some(message) = message {
            fields.insert("message".to_owned(), Value::String(message));
        }

        span.extensions_mut().insert(SpanFields(fields));
    }

    fn on_record(&self, id: &Id, values: &SpanRecord<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        let Some(SpanFields(fields)) = extensions.get_mut::<SpanFields>() else {
            return;
        };
        let mut message = None;
        values.record(&mut JsonVisitor {
            fields,
            message: &mut message,
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();

        let mut fields = Map::new();
        let mut message = None;
        event.record(&mut JsonVisitor {
            fields: &mut fields,
            message: &mut message,
        });

        let category = self.category_for(metadata);
        let mut entry = LogRecord::new(
            priority_for(metadata.level()),
            category.clone(),
            message.unwrap_or_default(),
        );

        // The GUI renders `category [original_category]`, which gives the module path a home
        // whenever it is not already the tab name.
        if let Some(module_path) = metadata.module_path()
            && module_path != category
        {
            entry = entry.original_category(module_path);
        }

        // Everything structured is nested under its own key so that a user field can never collide
        // with the layer's own metadata.
        let mut source = Map::new();

        if self.include_call_site
            && let Some(file) = metadata.file()
        {
            let at = match metadata.line() {
                Some(line) => format!("{file}:{line}"),
                None => file.to_owned(),
            };
            source.insert("at".to_owned(), Value::String(at));
        }

        if !fields.is_empty() {
            source.insert("fields".to_owned(), Value::Object(fields));
        }

        if self.include_spans {
            let mut spans = Vec::new();
            if let Some(scope) = ctx.event_scope(event) {
                // Outermost first, so the JSON reads the way the call stack did.
                for span in scope.from_root() {
                    let mut object = Map::new();
                    object.insert("name".to_owned(), Value::String(span.name().to_owned()));
                    if let Some(SpanFields(span_fields)) = span.extensions().get::<SpanFields>()
                        && !span_fields.is_empty()
                    {
                        object.insert("fields".to_owned(), Value::Object(span_fields.clone()));
                    }
                    spans.push(Value::Object(object));
                }
            }
            if !spans.is_empty() {
                source.insert("spans".to_owned(), Value::Array(spans));
            }
        }

        if !source.is_empty() {
            // Compact: the GUI formats JSON itself in the details pane, and the list view collapses
            // whitespace anyway.
            if let Ok(json) = serde_json::to_string(&Value::Object(source)) {
                entry = entry.source(json);
            }
        }

        self.client.log(entry);
    }
}

/// Collects `tracing` fields into a JSON object, pulling out the `message` field separately.
struct JsonVisitor<'a> {
    fields: &'a mut Map<String, Value>,
    message: &'a mut Option<String>,
}

impl JsonVisitor<'_> {
    fn insert(&mut self, field: &Field, value: Value) {
        self.fields.insert(field.name().to_owned(), value);
    }
}

impl Visit for JsonVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            *self.message = Some(value.to_owned());
        } else {
            self.insert(field, Value::String(value.to_owned()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // The message of `info!("x = {x}")` arrives here as a `format_args!`, whose Debug is the
        // formatted text without quotes -- which is exactly what should become the message.
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            *self.message = Some(rendered);
        } else {
            self.insert(field, Value::String(rendered));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        // JSON numbers cannot hold the full 128-bit range, so these become strings rather than
        // losing precision silently.
        self.insert(field, Value::String(value.to_string()));
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.insert(field, Value::String(value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // NaN and the infinities have no JSON representation; `Number::from_f64` returns None.
        match serde_json::Number::from_f64(value) {
            Some(number) => self.insert(field, Value::Number(number)),
            None => self.insert(field, Value::String(value.to_string())),
        }
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, Value::String(value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_mapping_uses_the_protocol_spelling() {
        assert_eq!(priority_for(&Level::TRACE).as_str(), "TRACE");
        assert_eq!(priority_for(&Level::DEBUG).as_str(), "DEBUG");
        assert_eq!(priority_for(&Level::INFO).as_str(), "INFORMATION");
        assert_eq!(priority_for(&Level::WARN).as_str(), "WARNING");
        assert_eq!(priority_for(&Level::ERROR).as_str(), "ERROR");
    }

    #[test]
    fn error_levels_map_onto_priorities_the_server_treats_as_errors() {
        assert!(priority_for(&Level::ERROR).is_error());
        assert!(priority_for(&Level::WARN).is_error_or_warning());
        assert!(!priority_for(&Level::INFO).is_error_or_warning());
    }

    #[test]
    fn the_visitor_separates_the_message_from_the_fields() {
        let mut fields = Map::new();
        let mut message = None;
        {
            let mut visitor = JsonVisitor {
                fields: &mut fields,
                message: &mut message,
            };
            // Field values are normally supplied by the macros; drive the visitor directly.
            let callsite = tracing_core::callsite::Identifier(&TEST_CALLSITE);
            let field_set = tracing_core::field::FieldSet::new(&["message", "count"], callsite);
            let mut iter = field_set.iter();
            let message_field = iter.next().unwrap();
            let count_field = iter.next().unwrap();
            visitor.record_str(&message_field, "hello");
            visitor.record_u64(&count_field, 7);
        }
        assert_eq!(message.as_deref(), Some("hello"));
        assert_eq!(fields.get("count"), Some(&Value::from(7u64)));
        assert!(!fields.contains_key("message"));
    }

    /// A minimal callsite, only so the test can build a `FieldSet` without the macros.
    struct TestCallsite;
    impl tracing_core::callsite::Callsite for TestCallsite {
        fn set_interest(&self, _: tracing_core::subscriber::Interest) {}
        fn metadata(&self) -> &Metadata<'_> {
            unimplemented!("the visitor never reads the callsite metadata")
        }
    }
    static TEST_CALLSITE: TestCallsite = TestCallsite;
}
