//! Rendering a [`SystemTime`] into the exact string the ECAppLog server accepts.
//!
//! The server parses timestamps with the literal Qt format `"yyyy-MM-ddThh:mm:ss.zzz"`
//! (`ecapplog/src/Data.cpp`), which is *not* RFC 3339: a trailing `Z` or numeric offset makes the
//! parse fail, and so does any fractional precision other than exactly three digits. A failed
//! parse is not an error — the server silently substitutes its own arrival time. A client that
//! emitted `chrono`-style RFC 3339 would therefore look like it worked while every row in the GUI
//! showed the wrong time, which is why this formatting lives in one place with tests on it.
//!
//! Converting a `SystemTime` to a calendar date needs `civil_from_days`, which is the only reason
//! `ecapplog-cpp` vendors all 8233 lines of Howard Hinnant's `date.h`. In Rust it is short enough
//! to inline, which keeps the crate free of a third-party time dependency and keeps third-party
//! types out of the public API.

use std::time::{SystemTime, UNIX_EPOCH};

/// Formats `time` as `YYYY-MM-DDTHH:MM:SS.mmm` in UTC, with no timezone suffix.
///
/// Sub-millisecond precision is truncated, not rounded, matching the Go client's `.000` layout and
/// the C++ client's `floor<milliseconds>`.
pub(crate) fn format_utc(time: SystemTime) -> String {
    // Signed nanoseconds since the epoch. `duration_since` reports pre-epoch instants as an error
    // carrying the absolute distance, so the sign has to be reapplied by hand.
    let nanos: i128 = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };

    // Euclidean division floors rather than truncating toward zero. For the positive case that is
    // the same truncation the other two clients do; for pre-epoch instants it is what keeps the
    // rendering monotonic (0.5s before the epoch is 1969-12-31T23:59:59.500, not ...:59.-500).
    let total_millis = nanos.div_euclid(1_000_000);
    let millis = total_millis.rem_euclid(1_000) as u32;
    let secs = total_millis.div_euclid(1_000);

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;

    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic Gregorian date.
///
/// Shifts the epoch to 0000-03-01 so that the leap day lands at the end of the 400-year era,
/// which is what removes every special case from the arithmetic.
fn civil_from_days(days: i128) -> (i128, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097; // day of era, [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // March-based month, [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32; // [1, 12]
    let year = yoe + era * 400 + i128::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64, nanos: u32) -> String {
        format_utc(UNIX_EPOCH + Duration::new(secs, nanos))
    }

    /// The fixture `ecapplog-cpp/tests/test_chrono.cpp` asserts, so both clients are pinned to the
    /// same byte-for-byte rendering.
    #[test]
    fn matches_the_cpp_client_fixture() {
        assert_eq!(at(1_581_761_840, 120_000_000), "2020-02-15T10:17:20.120");
    }

    #[test]
    fn epoch() {
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000");
    }

    /// The server rejects anything but exactly three fractional digits, and rejects a zone suffix
    /// outright. This is the assertion that would catch someone "helpfully" switching to RFC 3339.
    #[test]
    fn has_no_zone_suffix_and_exactly_three_fractional_digits() {
        let s = at(1_581_761_840, 120_000_000);
        assert!(!s.ends_with('Z'), "{s} must not carry a zone designator");
        assert!(!s.contains('+'), "{s} must not carry a numeric offset");
        let frac = s.split('.').nth(1).expect("a fractional part");
        assert_eq!(frac.len(), 3, "{s} must have exactly 3 fractional digits");
        assert_eq!(s.len(), 23, "{s} must be exactly 23 characters");
    }

    #[test]
    fn sub_millisecond_precision_truncates_rather_than_rounds() {
        // 999999ns is 0.999999ms: rounding would carry into the next millisecond.
        assert_eq!(at(0, 999_999), "1970-01-01T00:00:00.000");
        assert_eq!(at(0, 1_999_999), "1970-01-01T00:00:00.001");
    }

    #[test]
    fn leap_day() {
        // 2020-02-29T00:00:00Z
        assert_eq!(at(1_582_934_400, 0), "2020-02-29T00:00:00.000");
        // 2000-02-29 -- divisible by 400, so a leap year despite being a century.
        assert_eq!(at(951_782_400, 0), "2000-02-29T00:00:00.000");
        assert_eq!(at(1_583_020_799, 0), "2020-02-29T23:59:59.000");
        // 1900 was divisible by 4 but not by 400, so it was not a leap year: the day after
        // 1900-02-28 must be 1900-03-01. This is pre-epoch, which exercises the negative path too.
        assert_eq!(
            format_utc(UNIX_EPOCH - Duration::from_secs(2_203_977_600)),
            "1900-02-28T00:00:00.000"
        );
        assert_eq!(
            format_utc(UNIX_EPOCH - Duration::from_secs(2_203_977_600 - 86_400)),
            "1900-03-01T00:00:00.000"
        );
    }

    #[test]
    fn end_of_year_rolls_over() {
        assert_eq!(at(1_609_459_199, 999_000_000), "2020-12-31T23:59:59.999");
        assert_eq!(at(1_609_459_200, 0), "2021-01-01T00:00:00.000");
    }

    /// A `SystemTime` from before 1970 must render rather than panic; `duration_since` returns an
    /// error for these and an unchecked `unwrap` would take the caller's process down.
    #[test]
    fn pre_epoch_does_not_panic() {
        let t = UNIX_EPOCH - Duration::from_millis(500);
        assert_eq!(format_utc(t), "1969-12-31T23:59:59.500");
        let t = UNIX_EPOCH - Duration::from_secs(86_400);
        assert_eq!(format_utc(t), "1969-12-31T00:00:00.000");
    }
}
