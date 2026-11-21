//! What both providers build their timelines from.

use serde_json::Value;
use tmux_agent_protocol::v1::{
    self, ChatItemKind as Kind, ChatToolKind as ToolKind, ChatToolStatus as Status,
};

pub(super) fn text(id: impl Into<String>, kind: Kind, text: impl Into<String>) -> v1::ChatItem {
    v1::ChatItem {
        id: id.into(),
        kind: kind.into(),
        text: text.into(),
        ..Default::default()
    }
}

pub(super) fn tool(
    id: impl Into<String>,
    kind: ToolKind,
    title: &str,
    detail: impl Into<String>,
) -> v1::ChatItem {
    v1::ChatItem {
        id: id.into(),
        kind: Kind::Tool.into(),
        tool: Some(v1::ChatTool {
            kind: kind.into(),
            title: title.to_owned(),
            detail: detail.into(),
            status: Status::Running.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(super) fn is_running(item: &v1::ChatItem) -> bool {
    item.tool
        .as_ref()
        .is_some_and(|tool| tool.status == i32::from(Status::Running))
}

/// Ends a tool that will say nothing more, because its turn was stopped.
pub(super) fn stop(item: &mut v1::ChatItem) {
    item.streaming = false;
    if let Some(tool) = &mut item.tool
        && tool.status == i32::from(Status::Running)
    {
        tool.status = Status::Failed.into();
        tool.result_summary = "stopped".to_owned();
    }
}

/// `value[name]` as text; empty when it is not a string.
pub(super) fn string<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// How many lines a unified diff adds and removes.
pub(super) fn diff_counts(diff: &str) -> (u32, u32) {
    let mut counts = (0, 0);
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            counts.0 += 1;
        } else if line.starts_with('-') {
            counts.1 += 1;
        }
    }
    counts
}

pub(super) fn line_count(text: &str) -> u32 {
    u32::try_from(text.lines().count()).unwrap_or(u32::MAX)
}

/// The unix time, in milliseconds, of an RFC 3339 UTC timestamp such as
/// `2026-01-31T12:00:00.250Z`. Zero when it is not one.
pub(super) fn unix_millis(timestamp: &str) -> i64 {
    fn parse(timestamp: &str) -> Option<i64> {
        let bytes = timestamp.as_bytes();
        let number = |range: std::ops::Range<usize>| -> Option<i64> {
            let digits = timestamp.get(range)?;
            digits
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| digits.parse().ok())?
        };
        if bytes.len() < 20
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
            || !timestamp.ends_with('Z')
        {
            return None;
        }
        let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
        let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
        let millis = match timestamp[19..timestamp.len() - 1].strip_prefix('.') {
            Some(fraction) if !fraction.is_empty() => {
                let digits: String = fraction.chars().chain("000".chars()).take(3).collect();
                digits.parse::<i64>().ok()?
            }
            Some(_) => return None,
            None if timestamp.len() == 20 => 0,
            None => return None,
        };
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
            return None;
        }
        // Days since 1970-01-01 of a date in the proleptic Gregorian
        // calendar (Howard Hinnant's `days_from_civil`).
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year.rem_euclid(400);
        let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;
        Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000 + millis)
    }
    parse(timestamp).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_read_as_unix_milliseconds() {
        assert_eq!(unix_millis("1970-01-01T00:00:00Z"), 0);
        assert_eq!(unix_millis("2026-01-01T00:00:00.000Z"), 1_767_225_600_000);
        assert_eq!(unix_millis("2024-02-29T12:30:15.25Z"), 1_709_209_815_250);
        assert_eq!(unix_millis("2026-11-14T06:58:11.951Z"), 1_794_639_491_951);
        for unreadable in [
            "",
            "yesterday",
            "2026-01-01 00:00:00Z",
            "2026-01-01T00:00:00+01:00",
        ] {
            assert_eq!(unix_millis(unreadable), 0);
        }
    }

    #[test]
    fn a_diff_counts_its_lines_and_not_its_headers() {
        let diff = "--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+more\n";
        assert_eq!(diff_counts(diff), (2, 1));
    }
}
