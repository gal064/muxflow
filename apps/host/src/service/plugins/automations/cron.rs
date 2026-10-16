//! The cron dialect automations are scheduled with.
//!
//! Five fields — minute, hour, day of month, month, day of week — with `*`,
//! numbers, `a-b` ranges, `,` lists, `/n` steps, three-letter month and day
//! names, and `7` as a second Sunday. Nothing else: no macros, no `L`/`W`/`#`,
//! no seconds. Matching is against a local wall-clock minute supplied by the
//! caller, so this module never reads a clock or a time zone.

/// One local wall-clock minute. `weekday` counts from Sunday = 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LocalMinute {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub weekday: u8,
}

impl LocalMinute {
    /// The minute as it reads on the host's clock. Two different instants can
    /// share a key when daylight saving time repeats an hour; the scheduler
    /// relies on exactly that to fire the repeated minute once.
    pub(super) fn key(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Schedule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    /// Vixie cron: when both day fields are restricted, a day matches if
    /// either does. A field written with a leading `*` counts as unrestricted,
    /// even with a step.
    days_restricted: bool,
    weekdays_restricted: bool,
}

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
/// How far ahead a schedule must be able to fire. `0 0 31 2 *` never does.
const HORIZON_YEARS: i32 = 5;

/// Parses a schedule and refuses one that cannot fire within five years of
/// `this_year`.
pub(super) fn parse(expression: &str, this_year: i32) -> Result<Schedule, String> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "schedule needs 5 fields (minute hour day month weekday), found {}",
            fields.len()
        ));
    }
    let minutes = field(fields[0], "minute", 0, 59, None)?;
    let hours = field(fields[1], "hour", 0, 23, None)?;
    let days = field(fields[2], "day of month", 1, 31, None)?;
    let months = field(fields[3], "month", 1, 12, Some(&MONTH_NAMES))?;
    let weekdays = field(fields[4], "day of week", 0, 7, Some(&DAY_NAMES))?;
    // 7 is Sunday's second spelling.
    let weekdays = (weekdays | (weekdays >> 7)) & 0x7f;
    let schedule = Schedule {
        minutes,
        hours: hours as u32,
        days: days as u32,
        months: months as u16,
        weekdays: weekdays as u8,
        days_restricted: !fields[2].starts_with('*'),
        weekdays_restricted: !fields[4].starts_with('*'),
    };
    if !schedule.fires_within(this_year, HORIZON_YEARS) {
        return Err(format!(
            "schedule never fires in the next {HORIZON_YEARS} years"
        ));
    }
    Ok(schedule)
}

impl Schedule {
    pub(super) fn matches(&self, at: &LocalMinute) -> bool {
        self.minutes & (1 << at.minute) != 0
            && self.hours & (1 << at.hour) != 0
            && self.matches_day(at.month, at.day, at.weekday)
    }

    fn matches_day(&self, month: u8, day: u8, weekday: u8) -> bool {
        if self.months & (1 << month) == 0 {
            return false;
        }
        let by_day = self.days & (1 << day) != 0;
        let by_weekday = self.weekdays & (1 << weekday) != 0;
        if self.days_restricted && self.weekdays_restricted {
            by_day || by_weekday
        } else {
            by_day && by_weekday
        }
    }

    /// The next `count` local minutes after `after_secs` that match, read
    /// through `local`. Whole days and hours that cannot match are stepped
    /// over, two clock reads to a day, so even a schedule that runs once in
    /// four years costs a few thousand. A minute daylight saving time repeats
    /// is listed once.
    pub(super) fn next_runs(
        &self,
        after_secs: i64,
        count: usize,
        local: impl Fn(i64) -> Option<LocalMinute>,
    ) -> Vec<LocalMinute> {
        let mut found: Vec<LocalMinute> = Vec::new();
        let mut minute = after_secs.div_euclid(60) + 1;
        let end = minute + i64::from(HORIZON_YEARS) * 366 * 24 * 60;
        while found.len() < count && minute < end {
            let Some(at) = local(minute * 60) else {
                break;
            };
            let into_day = i64::from(at.hour) * 60 + i64::from(at.minute);
            let step = if !self.matches_day(at.month, at.day, at.weekday) {
                // To an hour before local midnight, then to midnight, so a day
                // daylight saving time shortens is never stepped past.
                let to_midnight = 24 * 60 - into_day;
                if to_midnight > 60 {
                    to_midnight - 60
                } else {
                    to_midnight
                }
            } else if self.hours & (1 << at.hour) == 0 {
                60 - i64::from(at.minute)
            } else {
                if self.minutes & (1 << at.minute) != 0
                    && !found.iter().any(|earlier| earlier.key() == at.key())
                {
                    found.push(at);
                }
                1
            };
            minute += step;
        }
        found
    }

    /// Minutes and hours always have at least one value, so a schedule fires
    /// if and only if some calendar day matches.
    fn fires_within(&self, from_year: i32, years: i32) -> bool {
        (from_year..from_year + years).any(|year| {
            (1..=12u8).any(|month| {
                (1..=days_in_month(year, month))
                    .any(|day| self.matches_day(month, day, weekday(year, month, day)))
            })
        })
    }
}

/// One field as a bitset of the values it allows, bit `n` for value `n`.
fn field(
    text: &str,
    label: &str,
    min: u32,
    max: u32,
    names: Option<&[&str]>,
) -> Result<u64, String> {
    let mut bits = 0u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .ok()
                    .filter(|step| *step > 0)
                    .ok_or_else(|| format!("{label}: bad step in '{item}'"))?;
                (range, step)
            }
            None => (item, 1),
        };
        let (first, last) = if range == "*" {
            (min, max)
        } else if let Some((first, last)) = range.split_once('-') {
            (
                value(first, label, min, max, names)?,
                value(last, label, min, max, names)?,
            )
        } else {
            let first = value(range, label, min, max, names)?;
            // `5/15` means from 5 to the end in steps of 15, as in Vixie cron.
            (first, if item.contains('/') { max } else { first })
        };
        if first > last {
            return Err(format!("{label}: range '{range}' runs backwards"));
        }
        let mut current = first;
        while current <= last {
            bits |= 1 << current;
            current = current.saturating_add(step);
        }
    }
    Ok(bits)
}

fn value(
    text: &str,
    label: &str,
    min: u32,
    max: u32,
    names: Option<&[&str]>,
) -> Result<u32, String> {
    if let Some(names) = names
        && let Some(index) = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(text))
    {
        // Months are named from 1, weekdays from 0.
        return Ok(index as u32 + min);
    }
    let number: u32 = text
        .parse()
        .map_err(|_| format!("{label}: '{text}' is not a number"))?;
    if number < min || number > max {
        return Err(format!("{label}: {number} is outside {min}-{max}"));
    }
    Ok(number)
}

pub(super) fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Sunday = 0, for the proleptic Gregorian calendar.
pub(super) fn weekday(year: i32, month: u8, day: u8) -> u8 {
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let year = if month < 3 { year - 1 } else { year };
    let value = year + year / 4 - year / 100
        + year / 400
        + OFFSETS[usize::from(month) - 1]
        + i32::from(day);
    value.rem_euclid(7) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> LocalMinute {
        LocalMinute {
            year,
            month,
            day,
            hour,
            minute,
            weekday: weekday(year, month, day),
        }
    }

    fn schedule(expression: &str) -> Schedule {
        parse(expression, 2026).unwrap()
    }

    #[test]
    fn every_field_form_is_understood() {
        let every_minute = schedule("* * * * *");
        assert!(every_minute.matches(&at(2026, 10, 13, 0, 0)));
        assert!(every_minute.matches(&at(2026, 10, 13, 23, 59)));

        let weekdays_nine = schedule("0 9 * * 1-5");
        assert!(weekdays_nine.matches(&at(2026, 10, 13, 9, 0)), "Tuesday");
        assert!(!weekdays_nine.matches(&at(2026, 10, 18, 9, 0)), "Sunday");
        assert!(!weekdays_nine.matches(&at(2026, 10, 13, 9, 1)));

        let list_and_steps = schedule("0,30 */6 1-10/3 * *");
        assert!(list_and_steps.matches(&at(2026, 10, 4, 12, 30)));
        assert!(!list_and_steps.matches(&at(2026, 10, 5, 12, 30)));
        assert!(!list_and_steps.matches(&at(2026, 10, 4, 13, 0)));

        let start_with_step = schedule("5/20 * * * *");
        for minute in [5, 25, 45] {
            assert!(start_with_step.matches(&at(2026, 1, 1, 0, minute)));
        }
        assert!(!start_with_step.matches(&at(2026, 1, 1, 0, 0)));
    }

    /// A UTC clock: minute `n` of 2026-10-13 is `START + n * 60`.
    fn utc(unix_secs: i64) -> Option<LocalMinute> {
        const START: i64 = 1_791_849_600; // 2026-10-13T00:00Z, a Tuesday
        let offset = unix_secs - START;
        let days = offset.div_euclid(86_400);
        let into = offset.rem_euclid(86_400);
        // Good for the rest of October, which is all the test needs.
        let day = 13 + u8::try_from(days).ok()?;
        Some(at(
            2026,
            10,
            day,
            (into / 3600) as u8,
            (into % 3600 / 60) as u8,
        ))
    }

    #[test]
    fn next_runs_step_over_days_and_hours_that_cannot_match() {
        let start = 1_791_849_600 + 9 * 3600; // Tuesday 09:00
        let keys = |expression: &str| -> Vec<String> {
            schedule(expression)
                .next_runs(start, 3, utc)
                .iter()
                .map(LocalMinute::key)
                .collect()
        };
        assert_eq!(
            keys("*/20 * * * *"),
            ["2026-10-13T09:20", "2026-10-13T09:40", "2026-10-13T10:00"],
            "the current minute is not next"
        );
        assert_eq!(
            keys("30 7 * * sat,sun"),
            ["2026-10-17T07:30", "2026-10-18T07:30", "2026-10-24T07:30"]
        );
        assert_eq!(keys("0 9 * * 1-5")[0], "2026-10-14T09:00");
    }

    #[test]
    fn names_are_case_insensitive_and_seven_is_sunday() {
        let named = schedule("30 7 * JAN-mar Mon,fri");
        assert!(named.matches(&at(2027, 1, 4, 7, 30)), "Monday in January");
        assert!(!named.matches(&at(2027, 4, 5, 7, 30)), "Monday in April");

        let sunday = schedule("0 0 * * 7");
        assert!(sunday.matches(&at(2026, 10, 18, 0, 0)));
        assert_eq!(sunday, schedule("0 0 * * 0"));
        assert_eq!(sunday, schedule("0 0 * * sun"));
    }

    #[test]
    fn restricted_day_fields_combine_with_or() {
        // The 1st of the month, or any Monday.
        let either = schedule("0 0 1 * mon");
        assert!(
            either.matches(&at(2026, 10, 1, 0, 0)),
            "the 1st, a Thursday"
        );
        assert!(either.matches(&at(2026, 10, 12, 0, 0)), "a Monday");
        assert!(!either.matches(&at(2026, 10, 13, 0, 0)));

        // A starred day field stays unrestricted even with a step, so the
        // weekday alone decides within the stepped days.
        let stepped = schedule("0 0 */2 * mon");
        assert!(
            stepped.matches(&at(2026, 10, 5, 0, 0)),
            "odd day and a Monday"
        );
        assert!(
            !stepped.matches(&at(2026, 10, 12, 0, 0)),
            "a Monday on an even day"
        );
        assert!(
            !stepped.matches(&at(2026, 10, 7, 0, 0)),
            "odd day, not a Monday"
        );
    }

    #[test]
    fn malformed_schedules_are_refused() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "@hourly",
            "0 0 L * *",
            "0 0 ? * *",
            "*/0 * * * *",
            "10-5 * * * *",
            "x * * * *",
        ] {
            assert!(parse(bad, 2026).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_schedule_must_be_able_to_fire() {
        assert!(
            parse("0 0 31 2 *", 2026)
                .unwrap_err()
                .contains("never fires")
        );
        assert!(parse("0 0 30 feb *", 2026).is_err());
        // Leap days exist within five years of 2026.
        assert!(parse("0 0 29 2 *", 2026).is_ok());
    }

    #[test]
    fn keys_are_zero_padded() {
        assert_eq!(at(2026, 3, 8, 2, 5).key(), "2026-03-08T02:05");
    }

    #[test]
    fn weekday_matches_known_dates() {
        assert_eq!(weekday(2026, 10, 13), 2, "Tuesday");
        assert_eq!(weekday(2000, 1, 1), 6, "Saturday");
        assert_eq!(weekday(2024, 2, 29), 4, "Thursday");
    }
}
