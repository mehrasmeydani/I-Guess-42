//! Round boundaries.
//!
//! A round runs from one 12:42 Europe/Vienna deadline to the next, and is
//! identified by the Vienna calendar date its deadline falls on. So the round
//! labelled `2026-09-06` is open from 2026-09-05 12:42 until 2026-09-06 12:42.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

pub const TZ: Tz = chrono_tz::Europe::Vienna;
pub const CUTOFF_HOUR: u32 = 12;
pub const CUTOFF_MIN: u32 = 42;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Round {
    /// Vienna date the deadline falls on; also the `round_date` column value.
    pub date: NaiveDate,
    pub deadline: DateTime<Utc>,
}

/// The 12:42 Europe/Vienna instant on `date`, as UTC.
pub fn cutoff_on(date: NaiveDate) -> DateTime<Utc> {
    let naive = date
        .and_hms_opt(CUTOFF_HOUR, CUTOFF_MIN, 0)
        .expect("12:42:00 is always a valid wall-clock time");
    // Vienna's DST transitions happen at 02:00/03:00, so 12:42 is never
    // skipped or repeated. Fall back to the earliest candidate anyway rather
    // than panicking if that ever stops being true.
    TZ.from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| TZ.from_utc_datetime(&naive))
        .with_timezone(&Utc)
}

impl Round {
    /// The round that is currently accepting guesses.
    pub fn current(now: DateTime<Utc>) -> Self {
        let today = now.with_timezone(&TZ).date_naive();
        let today_cutoff = cutoff_on(today);
        if now < today_cutoff {
            Self {
                date: today,
                deadline: today_cutoff,
            }
        } else {
            let tomorrow = today
                .succ_opt()
                .expect("date is far from the calendar limit");
            Self {
                date: tomorrow,
                deadline: cutoff_on(tomorrow),
            }
        }
    }

    /// The most recently closed round: the one whose results are on display.
    pub fn previous_date(&self) -> NaiveDate {
        self.date
            .pred_opt()
            .expect("date is far from the calendar limit")
    }

    /// How long ago the round before this one closed, in seconds. The open
    /// round began at that instant, so this is also how long it has been
    /// running. Read off the server's clock on purpose: whether the result of
    /// 12:42 is still fresh must not depend on the visitor's own clock.
    pub fn since_previous_close(&self, now: DateTime<Utc>) -> i64 {
        (now - cutoff_on(self.previous_date())).num_seconds().max(0)
    }

    pub fn key(&self) -> String {
        self.date.format("%Y-%m-%d").to_string()
    }

    pub fn seconds_left(&self, now: DateTime<Utc>) -> i64 {
        (self.deadline - now).num_seconds().max(0)
    }

    /// e.g. "Sunday 6 September 2026 at 12:42 CEST"
    pub fn deadline_human(&self) -> String {
        self.deadline
            .with_timezone(&TZ)
            .format("%A %-d %B %Y at %H:%M %Z")
            .to_string()
    }
}

/// How long is left, the way people say it: `18h 32m`, then `4m 05s` in
/// the last hour, then `9s`. app.js counts down in the same format.
pub fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// The round page's clock, ticking: "21:25:07", or "4:07" in the last hour.
pub fn clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The same, spelled out for a screen reader: "18 hours 32 minutes".
pub fn spoken_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (h, m) = (seconds / 3600, (seconds % 3600) / 60);
    let unit = |n: i64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    match (h, m) {
        (0, 0) => "less than a minute".to_string(),
        (0, m) => unit(m, "minute"),
        (h, 0) => unit(h, "hour"),
        (h, m) => format!("{} {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// Format a stored `round_date` string for display, falling back to the raw
/// value if it is somehow not a date.
pub fn format_round_date(key: &str) -> String {
    NaiveDate::parse_from_str(key, "%Y-%m-%d")
        .map(|d| d.format("%-d %B %Y").to_string())
        .unwrap_or_else(|_| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn before_cutoff_the_round_closes_today() {
        // 2026-09-06 10:00 Vienna (CEST = UTC+2) -> 08:00 UTC
        let r = Round::current(utc("2026-09-06T08:00:00Z"));
        assert_eq!(r.key(), "2026-09-06");
        assert_eq!(r.deadline, utc("2026-09-06T10:42:00Z"));
    }

    #[test]
    fn after_cutoff_the_round_rolls_to_tomorrow() {
        // 2026-09-06 13:00 Vienna -> 11:00 UTC, just past the 12:42 deadline.
        let r = Round::current(utc("2026-09-06T11:00:00Z"));
        assert_eq!(r.key(), "2026-09-07");
        assert_eq!(r.deadline, utc("2026-09-07T10:42:00Z"));
    }

    #[test]
    fn the_deadline_instant_itself_belongs_to_the_next_round() {
        let r = Round::current(utc("2026-09-06T10:42:00Z"));
        assert_eq!(r.key(), "2026-09-07");
    }

    #[test]
    fn winter_time_shifts_the_deadline_by_an_hour() {
        // 2026-01-15 is CET (UTC+1), so 12:42 local is 11:42 UTC.
        let r = Round::current(utc("2026-01-15T09:00:00Z"));
        assert_eq!(r.key(), "2026-01-15");
        assert_eq!(r.deadline, utc("2026-01-15T11:42:00Z"));
    }

    #[test]
    fn formats_the_remaining_time() {
        assert_eq!(clock(77_227), "21:27:07");
        assert_eq!(clock(3_600), "1:00:00");
        assert_eq!(clock(247), "4:07");
        assert_eq!(clock(-3), "0:00");
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(-5), "0s");
        assert_eq!(format_duration(59), "59s");
        assert_eq!(format_duration(724), "12m 04s");
        assert_eq!(format_duration(11_524), "3h 12m");
        assert_eq!(format_duration(66_720), "18h 32m");
        assert_eq!(spoken_duration(66_720), "18 hours 32 minutes");
        assert_eq!(spoken_duration(3_600), "1 hour");
        assert_eq!(spoken_duration(61), "1 minute");
        assert_eq!(spoken_duration(30), "less than a minute");
    }

    #[test]
    fn the_previous_round_closed_when_this_one_opened() {
        // 2026-09-06 12:52 Vienna: ten minutes past the deadline, so the round
        // that closed at 12:42 is the one labelled 2026-09-06 and the open one
        // is 2026-09-07.
        let r = Round::current(utc("2026-09-06T10:52:00Z"));
        assert_eq!(r.key(), "2026-09-07");
        assert_eq!(r.since_previous_close(utc("2026-09-06T10:52:00Z")), 600);

        // Just before the deadline the open round is still today's, and the
        // one before it closed nearly a full day ago.
        let r = Round::current(utc("2026-09-06T10:41:00Z"));
        assert_eq!(r.key(), "2026-09-06");
        assert_eq!(
            r.since_previous_close(utc("2026-09-06T10:41:00Z")),
            24 * 60 * 60 - 60
        );
    }

    #[test]
    fn previous_date_is_the_round_before() {
        let r = Round::current(utc("2026-09-06T08:00:00Z"));
        assert_eq!(
            r.previous_date().format("%Y-%m-%d").to_string(),
            "2026-09-05"
        );
    }
}
