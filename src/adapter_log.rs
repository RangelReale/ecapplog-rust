//! A [`log::Log`] implementation, so `log::info!` and friends reach the GUI.

use std::time::Duration;

use log::{Level, LevelFilter, Metadata, SetLoggerError};

use crate::client::Client;
use crate::priority::Priority;
use crate::record::Record;

/// How to choose a category -- that is, a GUI tab -- for each entry.
enum CategorySource {
    /// The record's target, which is the module path unless a call site overrode it. Routing by
    /// module means tabs mirror the shape of the program.
    Target,
    /// One fixed category for everything from this logger.
    Fixed(String),
    /// Whatever the closure decides.
    Custom(Box<dyn Fn(&log::Record<'_>) -> String + Send + Sync>),
}

/// Routes the [`log`] facade to ECAppLog.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// ecapplog::LogLogger::builder()
///     .app_name("my-service")
///     .max_level(log::LevelFilter::Debug)
///     .init()?;
///
/// log::info!("listening on {}", 8080);
/// # Ok(())
/// # }
/// ```
pub struct LogLogger {
    client: Client,
    level: LevelFilter,
    category: CategorySource,
    flush_timeout: Duration,
}

/// Builds a [`LogLogger`].
pub struct LogLoggerBuilder {
    client: Option<Client>,
    app_name: Option<String>,
    level: LevelFilter,
    category: CategorySource,
    flush_timeout: Duration,
}

impl Default for LogLoggerBuilder {
    fn default() -> Self {
        Self {
            client: None,
            app_name: None,
            level: LevelFilter::Trace,
            category: CategorySource::Target,
            flush_timeout: Duration::from_secs(5),
        }
    }
}

impl LogLoggerBuilder {
    /// Uses an existing client, so the application can log through both this facade and the direct
    /// API over one connection.
    pub fn client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Sets the app name on the client this builder creates.
    ///
    /// Ignored when [`client`](LogLoggerBuilder::client) supplied one already.
    pub fn app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// The most verbose level to forward. Default [`LevelFilter::Trace`].
    ///
    /// [`init`](LogLoggerBuilder::init) also passes this to [`log::set_max_level`], which is what
    /// lets the facade skip disabled call sites before they format their arguments.
    pub fn max_level(mut self, level: LevelFilter) -> Self {
        self.level = level;
        self
    }

    /// Sends everything to one category, instead of splitting tabs by module path.
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = CategorySource::Fixed(category.into());
        self
    }

    /// Derives the category from each record.
    pub fn category_fn(
        mut self,
        f: impl Fn(&log::Record<'_>) -> String + Send + Sync + 'static,
    ) -> Self {
        self.category = CategorySource::Custom(Box::new(f));
        self
    }

    /// How long [`log::Log::flush`] may spend draining. Default 5 seconds.
    pub fn flush_timeout(mut self, flush_timeout: Duration) -> Self {
        self.flush_timeout = flush_timeout;
        self
    }

    /// Builds the logger without registering it.
    ///
    /// Useful for composing with a fan-out logger; most applications want
    /// [`init`](LogLoggerBuilder::init).
    pub fn build(self) -> LogLogger {
        let client = self.client.unwrap_or_else(|| {
            let mut builder = Client::builder();
            if let Some(app_name) = self.app_name {
                builder = builder.app_name(app_name);
            }
            builder.build()
        });

        LogLogger {
            client,
            level: self.level,
            category: self.category,
            flush_timeout: self.flush_timeout,
        }
    }

    /// Builds the logger and installs it as the global one.
    pub fn init(self) -> Result<(), SetLoggerError> {
        let level = self.level;
        log::set_boxed_logger(Box::new(self.build()))?;
        log::set_max_level(level);
        Ok(())
    }
}

impl LogLogger {
    /// Starts configuring a logger.
    pub fn builder() -> LogLoggerBuilder {
        LogLoggerBuilder::default()
    }

    /// Installs a logger with every default, forwarding all levels.
    pub fn init() -> Result<(), SetLoggerError> {
        Self::builder().init()
    }

    /// The client this logger writes through.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn category_for(&self, record: &log::Record<'_>) -> String {
        match &self.category {
            CategorySource::Target => record.target().to_owned(),
            CategorySource::Fixed(category) => category.clone(),
            CategorySource::Custom(f) => f(record),
        }
    }
}

/// Maps a [`log`] level onto a protocol priority.
///
/// `log` has five levels and the protocol has eight, so `NOTICE`, `CRITICAL` and `FATAL` are
/// unreachable through this facade. They stay available through [`Client`]'s own methods.
fn priority_for(level: Level) -> Priority {
    match level {
        Level::Trace => Priority::Trace,
        Level::Debug => Priority::Debug,
        // Not `INFO`: the protocol spells it out.
        Level::Info => Priority::Information,
        Level::Warn => Priority::Warning,
        Level::Error => Priority::Error,
    }
}

impl log::Log for LogLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let category = self.category_for(record);
        let mut entry = Record::new(
            priority_for(record.level()),
            category.clone(),
            record.args().to_string(),
        );

        // `source` is shown in its own column and in the details pane, which makes it the natural
        // home for the call site.
        if let Some(file) = record.file() {
            entry = match record.line() {
                Some(line) => entry.source(format!("{file}:{line}")),
                None => entry.source(file),
            };
        }

        // When the category is not already the module path, keeping the module path as the original
        // category gets the GUI to render `category [module::path]` for free.
        if let Some(module_path) = record.module_path()
            && module_path != category
        {
            entry = entry.original_category(module_path);
        }

        self.client.log(entry);
    }

    fn flush(&self) {
        self.client.flush(self.flush_timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_mapping_uses_the_protocol_spelling() {
        assert_eq!(priority_for(Level::Trace).as_str(), "TRACE");
        assert_eq!(priority_for(Level::Debug).as_str(), "DEBUG");
        assert_eq!(priority_for(Level::Info).as_str(), "INFORMATION");
        assert_eq!(priority_for(Level::Warn).as_str(), "WARNING");
        assert_eq!(priority_for(Level::Error).as_str(), "ERROR");
    }

    #[test]
    fn error_levels_map_onto_priorities_the_server_treats_as_errors() {
        // Otherwise these would not reach the GUI's synthetic ERROR tab.
        assert!(priority_for(Level::Error).is_error());
        assert!(priority_for(Level::Warn).is_error_or_warning());
        assert!(!priority_for(Level::Info).is_error_or_warning());
    }
}
