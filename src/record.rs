//! A single log entry, and the builder that assembles one.

use std::time::SystemTime;

use crate::priority::Priority;

/// One log entry.
///
/// Built with a chained builder rather than the positional parameters `ecapplog-cpp` uses. That is
/// not just taste: when colour support was added to the C++ client the two colour parameters were
/// inserted *before* the existing `extraCategories` parameter, so every existing call that passed
/// extra categories positionally silently changed meaning.
///
/// ```
/// use ecapplog::{Priority, Record};
///
/// let record = Record::new(Priority::Error, "app", "could not open the database")
///     .original_category("app.storage")
///     .extra_category("db")
///     .color("#ff5555");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub(crate) time: SystemTime,
    pub(crate) priority: Priority,
    pub(crate) category: String,
    pub(crate) message: String,
    pub(crate) source: String,
    pub(crate) original_category: String,
    pub(crate) extra_categories: Vec<String>,
    pub(crate) color: String,
    pub(crate) bg_color: String,
}

impl Record {
    /// A new entry stamped with the current time.
    ///
    /// `category` becomes the tab name in the GUI. Avoid the names the server reserves for itself:
    /// `ALL`, `ERROR`, `ECAPPLOG` and `<unknown>`. An empty category is rendered as `<unknown>`.
    ///
    /// Note also that the GUI sorts all-uppercase category names ahead of the rest, so the casing
    /// you choose here affects tab order.
    pub fn new(
        priority: Priority,
        category: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            time: SystemTime::now(),
            priority,
            category: category.into(),
            message: message.into(),
            source: String::new(),
            original_category: String::new(),
            extra_categories: Vec::new(),
            color: String::new(),
            bg_color: String::new(),
        }
    }

    /// Overrides the timestamp.
    ///
    /// The wire format carries UTC with millisecond precision; anything finer is truncated.
    ///
    /// Both `chrono` and the `time` crate convert into `SystemTime`, so their users can write
    /// `.at(datetime.into())` without this crate depending on either.
    pub fn at(mut self, time: SystemTime) -> Self {
        self.time = time;
        self
    }

    /// Free-form text shown in the GUI's `Source` column and Details pane.
    ///
    /// The pane pretty-prints this when it happens to be JSON, which makes it a good home for
    /// structured context that the protocol has nowhere else to put. The list view collapses
    /// whitespace, so multi-line values read correctly only in the Details pane.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// The entry's "real" category, when [`category`](Record::new) is a coarser grouping.
    ///
    /// The GUI renders the pair as `category [original_category]`.
    pub fn original_category(mut self, original_category: impl Into<String>) -> Self {
        self.original_category = original_category.into();
        self
    }

    /// Adds one more category tab that should also receive this entry.
    pub fn extra_category(mut self, category: impl Into<String>) -> Self {
        self.extra_categories.push(category.into());
        self
    }

    /// Adds several extra category tabs.
    pub fn extra_categories<I, S>(mut self, categories: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_categories
            .extend(categories.into_iter().map(Into::into));
        self
    }

    /// Overrides the foreground colour, which otherwise follows the priority.
    ///
    /// Anything Qt's colour parser accepts works: `#rgb`, `#rrggbb`, SVG names like `steelblue`,
    /// or `transparent`. An unparseable value is ignored by the server, silently.
    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.color = color.into();
        self
    }

    /// Sets a background colour. Accepts the same syntax as [`color`](Record::color).
    pub fn bg_color(mut self, bg_color: impl Into<String>) -> Self {
        self.bg_color = bg_color.into();
        self
    }
}
