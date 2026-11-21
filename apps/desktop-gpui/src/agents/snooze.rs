//! Snooze wake times: what Settings stores, what the Snooze menu offers, and
//! when each one is. Times are local, as the person setting them reads a
//! clock.
//!
//! One spelling serves Settings, the menu and "Pick a time…":
//!
//! - `30m`, `1h`, `2d`: that long from now;
//! - `18:30`: the next time the clock reads that, today or tomorrow;
//! - `tomorrow 09:00`;
//! - `mon 09:00` (any weekday, short or long): the next such day, never today.

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Weekday};

/// What Snooze offers until Settings says otherwise.
pub const DEFAULT_TIMES: [&str; 4] = ["1h", "3h", "tomorrow 09:00", "mon 09:00"];
/// The most a menu lists.
pub const MAX_TIMES: usize = 8;
/// How far ahead a time can be, as the host allows.
const MAX_AHEAD_DAYS: i64 = 365;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeTime {
    /// That many minutes from now.
    After(i64),
    /// The next time the clock reads this.
    At(NaiveTime),
    Tomorrow(NaiveTime),
    Next(Weekday, NaiveTime),
}

const WEEKDAYS: [(Weekday, &str, &str); 7] = [
    (Weekday::Mon, "mon", "monday"),
    (Weekday::Tue, "tue", "tuesday"),
    (Weekday::Wed, "wed", "wednesday"),
    (Weekday::Thu, "thu", "thursday"),
    (Weekday::Fri, "fri", "friday"),
    (Weekday::Sat, "sat", "saturday"),
    (Weekday::Sun, "sun", "sunday"),
];

fn clock(text: &str) -> Option<NaiveTime> {
    let (hours, minutes) = text.split_once(':')?;
    if minutes.len() != 2 || hours.is_empty() || hours.len() > 2 {
        return None;
    }
    NaiveTime::from_hms_opt(hours.parse().ok()?, minutes.parse().ok()?, 0)
}

pub fn parse(text: &str) -> Option<WakeTime> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.strip_prefix("in ").unwrap_or(&text).trim();
    if let Some(time) = clock(text) {
        return Some(WakeTime::At(time));
    }
    if let Some((day, time)) = text.split_once(char::is_whitespace) {
        if day == "tomorrow" {
            return Some(WakeTime::Tomorrow(clock(time.trim())?));
        }
        if let Some((weekday, _, _)) = WEEKDAYS
            .iter()
            .find(|(_, short, long)| day == *short || day == *long)
        {
            return Some(WakeTime::Next(*weekday, clock(time.trim())?));
        }
    }
    // "3h", "3 hours".
    let text: String = text.split_whitespace().collect();
    let text = text.as_str();
    let unit_at = text.find(|c: char| !c.is_ascii_digit())?;
    let (count, unit) = text.split_at(unit_at);
    let count: i64 = count.parse().ok().filter(|count| *count > 0)?;
    let minutes = match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => count,
        "h" | "hour" | "hours" => count.checked_mul(60)?,
        "d" | "day" | "days" => count.checked_mul(24 * 60)?,
        _ => return None,
    };
    (minutes <= MAX_AHEAD_DAYS * 24 * 60).then_some(WakeTime::After(minutes))
}

/// The spelling Settings stores.
pub fn spelling(time: WakeTime) -> String {
    let clock = |time: NaiveTime| time.format("%H:%M").to_string();
    match time {
        WakeTime::After(minutes) if minutes % (24 * 60) == 0 => format!("{}d", minutes / (24 * 60)),
        WakeTime::After(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        WakeTime::After(minutes) => format!("{minutes}m"),
        WakeTime::At(time) => clock(time),
        WakeTime::Tomorrow(time) => format!("tomorrow {}", clock(time)),
        WakeTime::Next(day, time) => {
            let short = WEEKDAYS
                .iter()
                .find(|(weekday, _, _)| *weekday == day)
                .map_or("mon", |(_, short, _)| short);
            format!("{short} {}", clock(time))
        }
    }
}

/// What the menu says, as the mock words it: "In 1 hour", "Tomorrow, 09:00".
pub fn label(time: WakeTime) -> String {
    let clock = |time: NaiveTime| time.format("%H:%M").to_string();
    let plural = |count: i64, unit: &str| {
        if count == 1 {
            format!("In 1 {unit}")
        } else {
            format!("In {count} {unit}s")
        }
    };
    match time {
        WakeTime::After(minutes) if minutes % (24 * 60) == 0 => plural(minutes / (24 * 60), "day"),
        WakeTime::After(minutes) if minutes % 60 == 0 => plural(minutes / 60, "hour"),
        WakeTime::After(minutes) => plural(minutes, "minute"),
        WakeTime::At(time) => format!("At {}", clock(time)),
        WakeTime::Tomorrow(time) => format!("Tomorrow, {}", clock(time)),
        WakeTime::Next(day, time) => {
            let long = WEEKDAYS
                .iter()
                .find(|(weekday, _, _)| *weekday == day)
                .map_or("monday", |(_, _, long)| long);
            let mut chars = long.chars();
            let name: String = chars
                .next()
                .map(|first| first.to_ascii_uppercase())
                .into_iter()
                .chain(chars)
                .collect();
            format!("{name}, {}", clock(time))
        }
    }
}

/// When `time` is, from `now`, in Unix milliseconds.
pub fn wake_at<Tz: TimeZone>(time: WakeTime, now: &DateTime<Tz>) -> i64 {
    let local = |date: chrono::NaiveDate, clock: NaiveTime| {
        now.timezone()
            .from_local_datetime(&date.and_time(clock))
            .earliest()
            // A clock time skipped by a daylight-saving jump: an hour later.
            .or_else(|| {
                now.timezone()
                    .from_local_datetime(&(date.and_time(clock) + Duration::hours(1)))
                    .earliest()
            })
            .map_or(now.timestamp_millis(), |at| at.timestamp_millis())
    };
    let today = now.date_naive();
    match time {
        WakeTime::After(minutes) => (now.clone() + Duration::minutes(minutes)).timestamp_millis(),
        WakeTime::At(clock) => {
            let at = local(today, clock);
            if at > now.timestamp_millis() {
                at
            } else {
                local(today + Duration::days(1), clock)
            }
        }
        WakeTime::Tomorrow(clock) => local(today + Duration::days(1), clock),
        WakeTime::Next(day, clock) => {
            let ahead = (i64::from(day.num_days_from_monday())
                - i64::from(today.weekday().num_days_from_monday()))
            .rem_euclid(7);
            let ahead = if ahead == 0 { 7 } else { ahead };
            local(today + Duration::days(ahead), clock)
        }
    }
}

pub fn wake_from_now(time: WakeTime) -> i64 {
    wake_at(time, &Local::now())
}

/// Settings' field: the times it understood, in its spelling, at most
/// [`MAX_TIMES`]. Nothing understood is the defaults.
pub fn parse_list(text: &str) -> Vec<String> {
    let mut times: Vec<String> = Vec::new();
    for time in text.split(',').filter_map(parse).map(spelling) {
        if !times.contains(&time) && times.len() < MAX_TIMES {
            times.push(time);
        }
    }
    if times.is_empty() {
        DEFAULT_TIMES
            .iter()
            .map(|time| (*time).to_owned())
            .collect()
    } else {
        times
    }
}

/// What the menu lists for the stored times.
pub fn menu_times(stored: &[String]) -> Vec<WakeTime> {
    stored.iter().filter_map(|time| parse(time)).collect()
}

/// "wakes Mon 09:00", "wakes 18:30", "wakes 12 Nov 09:00" for a snoozed row.
pub fn wake_label<Tz: TimeZone>(at_millis: i64, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = now.timezone().timestamp_millis_opt(at_millis).single() else {
        return "snoozed".into();
    };
    let days = (at.date_naive() - now.date_naive()).num_days();
    let format = match days {
        0 => "%H:%M",
        1..=6 => "%a %H:%M",
        _ => "%-d %b %H:%M",
    };
    format!("wakes {}", at.format(format))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate};

    fn now() -> DateTime<FixedOffset> {
        // A Wednesday, 14:20.
        FixedOffset::east_opt(0)
            .unwrap()
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 11, 11)
                    .unwrap()
                    .and_hms_opt(14, 20, 0)
                    .unwrap(),
            )
            .unwrap()
    }

    fn at(day: u32, hour: u32, minute: u32) -> i64 {
        FixedOffset::east_opt(0)
            .unwrap()
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 11, day)
                    .unwrap()
                    .and_hms_opt(hour, minute, 0)
                    .unwrap(),
            )
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn the_defaults_read_as_the_mock_words_them() {
        let labels: Vec<String> = menu_times(&parse_list("")).into_iter().map(label).collect();
        assert_eq!(
            labels,
            [
                "In 1 hour",
                "In 3 hours",
                "Tomorrow, 09:00",
                "Monday, 09:00"
            ]
        );
    }

    #[test]
    fn each_spelling_wakes_when_it_says() {
        let now = now();
        let wake = |text: &str| wake_at(parse(text).unwrap(), &now);
        assert_eq!(wake("1m"), now.timestamp_millis() + 60_000);
        assert_eq!(wake("in 3 hours"), at(11, 17, 20));
        assert_eq!(wake("18:30"), at(11, 18, 30));
        // Past today's: tomorrow's.
        assert_eq!(wake("09:00"), at(12, 9, 0));
        assert_eq!(wake("tomorrow 09:00"), at(12, 9, 0));
        assert_eq!(wake("mon 09:00"), at(16, 9, 0));
        // Never today, even later today.
        assert_eq!(wake("Wednesday 18:00"), at(18, 18, 0));
    }

    #[test]
    fn settings_keep_what_they_understand_in_one_spelling() {
        assert_eq!(
            parse_list("90m, 2 hours, nonsense, Friday 9:15, 1h, 120m"),
            ["90m", "2h", "fri 09:15", "1h"]
        );
        assert_eq!(parse_list("nonsense"), DEFAULT_TIMES);
        assert_eq!(parse("25:00"), None);
        assert_eq!(parse("0h"), None);
        assert_eq!(parse("400d"), None);
    }

    #[test]
    fn a_snoozed_row_says_when_it_wakes() {
        let now = now();
        assert_eq!(wake_label(at(11, 18, 30), &now), "wakes 18:30");
        assert_eq!(wake_label(at(16, 9, 0), &now), "wakes Mon 09:00");
        assert_eq!(wake_label(at(30, 9, 0), &now), "wakes 30 Nov 09:00");
    }
}
