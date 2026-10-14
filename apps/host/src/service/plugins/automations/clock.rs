//! The host's local wall clock, which is what cron schedules are written in.

use super::cron::LocalMinute;

unsafe extern "C" {
    /// POSIX; the `libc` crate binds `localtime_r` but not this.
    fn tzset();
}

/// The local minute containing `unix_secs`, in the host's current time zone.
///
/// `tzset` runs first so a time zone changed on a long-running host is picked
/// up: glibc's `localtime_r` is not required to re-read it.
pub(super) fn local_minute(unix_secs: i64) -> Option<LocalMinute> {
    let time = libc::time_t::try_from(unix_secs).ok()?;
    // SAFETY: `tm` is plain old data, fully written by `localtime_r` on
    // success; both pointers are valid for the duration of the calls, and
    // `tzset` has no preconditions.
    let tm = unsafe {
        tzset();
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalMinute {
        year: tm.tm_year + 1900,
        month: u8::try_from(tm.tm_mon + 1).ok()?,
        day: u8::try_from(tm.tm_mday).ok()?,
        hour: u8::try_from(tm.tm_hour).ok()?,
        minute: u8::try_from(tm.tm_min).ok()?,
        weekday: u8::try_from(tm.tm_wday).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::plugins::automations::cron::{days_in_month, weekday};

    #[test]
    fn the_local_minute_is_a_real_calendar_minute() {
        let minute = local_minute(1_792_046_400).unwrap();
        assert!((1..=12).contains(&minute.month));
        assert!((1..=days_in_month(minute.year, minute.month)).contains(&minute.day));
        assert!(minute.hour < 24 && minute.minute < 60);
        assert_eq!(
            minute.weekday,
            weekday(minute.year, minute.month, minute.day)
        );
        // Sixty seconds later is the next minute, whatever the zone.
        assert_ne!(local_minute(1_792_046_460).unwrap(), minute);
    }
}
