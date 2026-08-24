//! The wire protocol: frame layout and the JSON log payload.
//!
//! Verified against the server's decoder (`ecapplog/src/Server.cpp`, `src/Data.cpp`) as well as
//! both sibling clients. A frame is
//!
//! ```text
//! [u8  command]
//! [u32 payload length, big-endian]
//! [payload bytes]
//! ```
//!
//! with no delimiter and nothing to read back. The server reads frames inside a `QDataStream`
//! transaction, so splitting a frame across writes is safe -- but it costs syscalls for no gain,
//! so this module builds the whole frame in one buffer. Both sibling clients issue three writes
//! per frame instead.

use serde::Serialize;

use crate::record::Record;
use crate::timestamp;

/// Names the connection. Sent once, immediately after connecting.
pub(crate) const CMD_BANNER: u8 = 99;

/// Carries a JSON log entry.
pub(crate) const CMD_LOG: u8 = 0;

/// The mandatory banner prefix. The trailing space is part of it: the server checks
/// `data.startsWith("ECAPPLOG ")` and then strips exactly 9 bytes.
pub(crate) const BANNER_PREFIX: &[u8] = b"ECAPPLOG ";

/// The server closes the connection on any frame declaring more than this
/// (`MAX_PAYLOAD_SIZE` in `ecapplog/src/Config.h`). The check there is `size > MAX_PAYLOAD_SIZE`,
/// so exactly this many bytes is still accepted.
pub(crate) const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// The JSON object sent as a [`CMD_LOG`] payload.
///
/// Field order matches `ecapplog-go/cmd.go` so the two clients produce byte-identical payloads.
/// The server reads by key, so this is parity for its own sake, not a requirement.
///
/// The first four keys are always emitted, even when empty, again matching Go's struct tags. The
/// rest are omitted when empty -- Go's `omitempty` and C++'s `if (!x.empty())`.
#[derive(Serialize)]
pub(crate) struct LogPayload<'a> {
    time: String,
    priority: &'a str,
    category: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "is_empty")]
    source: &'a str,
    #[serde(skip_serializing_if = "is_empty")]
    original_category: &'a str,
    #[serde(skip_serializing_if = "is_empty_slice")]
    extra_categories: &'a [String],
    #[serde(skip_serializing_if = "is_empty")]
    color: &'a str,
    /// One word, no underscore -- unlike `original_category`.
    #[serde(rename = "bgcolor", skip_serializing_if = "is_empty")]
    bg_color: &'a str,
}

fn is_empty(s: &str) -> bool {
    s.is_empty()
}

fn is_empty_slice(v: &[String]) -> bool {
    v.is_empty()
}

impl<'a> LogPayload<'a> {
    pub(crate) fn from_record(record: &'a Record) -> Self {
        Self {
            time: timestamp::format_utc(record.time),
            priority: record.priority.as_str(),
            category: &record.category,
            message: &record.message,
            source: &record.source,
            original_category: &record.original_category,
            extra_categories: &record.extra_categories,
            color: &record.color,
            bg_color: &record.bg_color,
        }
    }
}

/// Writes `command` and `payload` into `out` as a complete frame, replacing whatever `out` held.
pub(crate) fn encode_frame(command: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(5 + payload.len());
    out.push(command);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
}

/// The banner payload for `app_name`: `"ECAPPLOG "` followed by the name, UTF-8, unterminated.
pub(crate) fn banner_payload(app_name: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(BANNER_PREFIX.len() + app_name.len());
    payload.extend_from_slice(BANNER_PREFIX);
    payload.extend_from_slice(app_name.as_bytes());
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::priority::Priority;
    use std::time::{Duration, UNIX_EPOCH};

    fn fixed_record() -> Record {
        Record::new(Priority::Information, "app", "hello")
            .at(UNIX_EPOCH + Duration::new(1_581_761_840, 120_000_000))
    }

    fn json(record: &Record) -> String {
        serde_json::to_string(&LogPayload::from_record(record)).unwrap()
    }

    #[test]
    fn banner_frame_bytes() {
        let mut out = Vec::new();
        encode_frame(CMD_BANNER, &banner_payload("ECAPPLOG-RUST"), &mut out);

        // 0x63 = 99, then a big-endian length of 9 + 13 = 22 = 0x16.
        let mut expected = vec![0x63, 0x00, 0x00, 0x00, 0x16];
        expected.extend_from_slice(b"ECAPPLOG ECAPPLOG-RUST");
        assert_eq!(out, expected);
    }

    #[test]
    fn banner_prefix_has_its_trailing_space() {
        // The server strips exactly 9 bytes and rejects anything not matching this prefix.
        assert_eq!(BANNER_PREFIX.len(), 9);
        assert_eq!(BANNER_PREFIX, b"ECAPPLOG ");
        assert_eq!(banner_payload(""), b"ECAPPLOG ");
    }

    #[test]
    fn length_is_big_endian() {
        let mut out = Vec::new();
        encode_frame(CMD_LOG, &vec![0u8; 0x0102], &mut out);
        assert_eq!(&out[..5], &[0x00, 0x00, 0x00, 0x01, 0x02]);
    }

    #[test]
    fn encode_frame_replaces_previous_contents() {
        // The worker reuses one buffer across frames; a stale tail would corrupt the stream.
        let mut out = Vec::new();
        encode_frame(CMD_LOG, b"aaaaaaaa", &mut out);
        encode_frame(CMD_LOG, b"b", &mut out);
        assert_eq!(out, vec![0x00, 0x00, 0x00, 0x00, 0x01, b'b']);
    }

    #[test]
    fn mandatory_fields_are_emitted_even_when_empty() {
        let record = Record::new(Priority::Information, "", "").at(UNIX_EPOCH);
        assert_eq!(
            json(&record),
            r#"{"time":"1970-01-01T00:00:00.000","priority":"INFORMATION","category":"","message":""}"#
        );
    }

    #[test]
    fn optional_fields_are_omitted_when_empty() {
        let out = json(&fixed_record());
        assert_eq!(
            out,
            r#"{"time":"2020-02-15T10:17:20.120","priority":"INFORMATION","category":"app","message":"hello"}"#
        );
        for key in [
            "source",
            "original_category",
            "extra_categories",
            "color",
            "bgcolor",
        ] {
            assert!(
                !out.contains(key),
                "{key} should have been omitted from {out}"
            );
        }
    }

    #[test]
    fn field_order_matches_the_go_client() {
        let record = fixed_record()
            .source("src")
            .original_category("app.inner")
            .extra_category("other")
            .color("#ff0000")
            .bg_color("#f9f9f9");
        assert_eq!(
            json(&record),
            concat!(
                r##"{"time":"2020-02-15T10:17:20.120","priority":"INFORMATION","category":"app","##,
                r##""message":"hello","source":"src","original_category":"app.inner","##,
                r##""extra_categories":["other"],"color":"#ff0000","bgcolor":"#f9f9f9"}"##
            )
        );
    }

    #[test]
    fn a_custom_priority_is_sent_verbatim() {
        let record = Record::new(Priority::Custom("SEVERE".into()), "app", "x").at(UNIX_EPOCH);
        assert!(json(&record).contains(r#""priority":"SEVERE""#));
    }

    #[test]
    fn messages_are_json_escaped() {
        let record = Record::new(Priority::Debug, "app", "a \"quoted\"\nline\t").at(UNIX_EPOCH);
        assert!(json(&record).contains(r#""message":"a \"quoted\"\nline\t""#));
    }
}
