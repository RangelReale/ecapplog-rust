//! Log priorities, as the ECAppLog server understands them.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

/// The priority of a log entry.
///
/// Priorities travel as case-sensitive strings, not numbers, and the server compares them
/// literally (`ecapplog/src/Config.cpp`). Note that the informational level is spelled
/// `INFORMATION`, not `INFO`.
///
/// The variant order below follows the two sibling clients for familiarity; it is deliberately
/// *not* a severity ordering, and this type intentionally does not implement `Ord`, because the
/// protocol defines no ranking. Use [`Priority::is_error`] and
/// [`Priority::is_error_or_warning`] to reason about severity the way the server does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Priority {
    /// `TRACE`
    Trace,
    /// `DEBUG`
    Debug,
    /// `INFORMATION` -- note the spelling.
    Information,
    /// `NOTICE`
    Notice,
    /// `WARNING`
    Warning,
    /// `FATAL`
    Fatal,
    /// `CRITICAL`
    Critical,
    /// `ERROR`
    Error,
    /// Any other string.
    ///
    /// The server accepts arbitrary priorities without complaint, but they get no colour of their
    /// own in the GUI and are not copied into the synthetic `ERROR` tab. Prefer the named variants
    /// unless you are forwarding levels from a system that has its own vocabulary.
    Custom(Cow<'static, str>),
}

impl Priority {
    /// The exact string sent on the wire.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Information => "INFORMATION",
            Self::Notice => "NOTICE",
            Self::Warning => "WARNING",
            Self::Fatal => "FATAL",
            Self::Critical => "CRITICAL",
            Self::Error => "ERROR",
            Self::Custom(s) => s,
        }
    }

    /// Whether the server classifies this priority as an error.
    ///
    /// Mirrors `Priority::isError` in `ecapplog/src/Config.cpp`, so callers can predict which
    /// entries the GUI copies into its `ERROR` tab.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Fatal | Self::Critical | Self::Error)
    }

    /// Whether the server classifies this priority as an error or a warning.
    ///
    /// Mirrors `Priority::isErrorOrWarning`. These are exactly the entries that reach the `ERROR`
    /// tab; a [`Priority::Custom`] value never does, whatever it spells.
    pub fn is_error_or_warning(&self) -> bool {
        self.is_error() || matches!(self, Self::Warning)
    }

    /// Every priority the protocol names, in declaration order.
    pub const ALL: [Priority; 8] = [
        Priority::Trace,
        Priority::Debug,
        Priority::Information,
        Priority::Notice,
        Priority::Warning,
        Priority::Fatal,
        Priority::Critical,
        Priority::Error,
    ];
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Priority {
    type Err = std::convert::Infallible;

    /// Parses one of the eight protocol names, falling back to [`Priority::Custom`].
    ///
    /// Matching is case-insensitive on the way in as a convenience, but note the server itself is
    /// case-sensitive: a `Custom("warn")` is not a `WARNING` as far as the GUI is concerned.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_uppercase().as_str() {
            "TRACE" => Self::Trace,
            "DEBUG" => Self::Debug,
            "INFORMATION" => Self::Information,
            "NOTICE" => Self::Notice,
            "WARNING" => Self::Warning,
            "FATAL" => Self::Fatal,
            "CRITICAL" => Self::Critical,
            "ERROR" => Self::Error,
            _ => Self::Custom(Cow::Owned(s.to_owned())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_server_constants() {
        // Verbatim from ecapplog/src/Config.cpp.
        let expected = [
            "TRACE",
            "DEBUG",
            "INFORMATION",
            "NOTICE",
            "WARNING",
            "FATAL",
            "CRITICAL",
            "ERROR",
        ];
        let actual: Vec<&str> = Priority::ALL.iter().map(Priority::as_str).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn error_classification_matches_the_server() {
        for p in Priority::ALL {
            let expect_error = matches!(p, Priority::Fatal | Priority::Critical | Priority::Error);
            assert_eq!(p.is_error(), expect_error, "{p}");
            assert_eq!(
                p.is_error_or_warning(),
                expect_error || p == Priority::Warning,
                "{p}"
            );
        }
    }

    #[test]
    fn a_custom_priority_is_never_error_classified() {
        // The server compares literally, so a lowercase "error" reaches no ERROR tab.
        let p: Priority = "error".parse().unwrap();
        assert_eq!(p, Priority::Error, "parsing is case-insensitive");

        let p = Priority::Custom("SEVERE".into());
        assert!(!p.is_error());
        assert!(!p.is_error_or_warning());
        assert_eq!(p.as_str(), "SEVERE");
    }

    #[test]
    fn round_trips_through_display_and_from_str() {
        for p in Priority::ALL {
            assert_eq!(p.to_string().parse::<Priority>().unwrap(), p);
        }
    }
}
