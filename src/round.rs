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
fn cutoff_on(date: NaiveDate) -> DateTime<Utc> {
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

/// `12:04`, or `3:12:04` once there is more than an hour to go.
pub fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
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
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(-5), "0:00");
        assert_eq!(format_duration(59), "0:59");
        assert_eq!(format_duration(724), "12:04");
        assert_eq!(format_duration(11_524), "3:12:04");
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
