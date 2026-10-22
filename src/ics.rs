//! The calendar file for a round's reveal: one event at the close time,
//! linking to that round's results, so a player who just locked a number is
//! reminded to come back for it. RFC 5545, kept to what Apple Calendar,
//! Google Calendar and Outlook all read the same way.

use chrono::{DateTime, Utc};

/// The `.ics` for a reveal at `closes_at`. `uid_host` makes the event's UID
/// unique to this site, and stable per round and pool, so adding it twice
/// updates the event rather than duplicating it.
pub fn reveal_event(
    round: &str,
    pool_key: &str,
    closes_at: DateTime<Utc>,
    results_url: &str,
    uid_host: &str,
    now: DateTime<Utc>,
) -> String {
    let stamp = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
    let lines = [
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//i guess low//round reveal//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "METHOD:PUBLISH".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:round-{round}-{pool_key}@{uid_host}"),
        format!("DTSTAMP:{}", stamp(now)),
        format!("DTSTART:{}", stamp(closes_at)),
        format!("DTEND:{}", stamp(closes_at + chrono::Duration::minutes(5))),
        format!("SUMMARY:{}", escape("i guess low — results")),
        format!("DESCRIPTION:{}", escape(&format!("the numbers are out. see who won: {results_url}"))),
        format!("URL:{results_url}"),
        "TRANSP:TRANSPARENT".to_string(),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
    ];
    lines.iter().map(|l| fold(l) + "\r\n").collect()
}

/// TEXT values escape backslashes, commas, semicolons and newlines.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(';', "\\;")
        .replace('\n', "\\n")
}

/// Lines longer than 75 octets continue on the next line after a space,
/// never splitting a UTF-8 character.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in line.chars() {
        let n = ch.len_utf8();
        if width + n > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_event_is_at_the_close_and_links_the_results() {
        let ics = reveal_event(
            "2026-10-21",
            "global",
            utc("2026-10-21T10:42:00Z"),
            "https://iguesslow.com/g/day/2026-10-21",
            "iguesslow.com",
            utc("2026-10-20T15:00:00Z"),
        );
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert!(ics.contains("\r\nDTSTART:20261021T104200Z\r\n"));
        assert!(ics.contains("\r\nDTEND:20261021T104700Z\r\n"));
        assert!(ics.contains("\r\nUID:round-2026-10-21-global@iguesslow.com\r\n"));
        assert!(ics.contains("\r\nURL:https://iguesslow.com/g/day/2026-10-21\r\n"));
        // Every line ends in CRLF, and none is longer than 75 octets.
        for line in ics.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.len() <= 75, "{line:?}");
            assert!(!line.contains('\n'));
        }
        // Unfolded, the description is whole.
        let unfolded = ics.replace("\r\n ", "");
        assert!(unfolded.contains("DESCRIPTION:the numbers are out. see who won: https://iguesslow.com/g/day/2026-10-21"));
    }

    #[test]
    fn text_is_escaped_and_folding_never_splits_a_character() {
        assert_eq!(escape("a, b; c\\d"), "a\\, b\\; c\\\\d");
        let long = "SUMMARY:".to_string() + &"é".repeat(60);
        let folded = fold(&long);
        for part in folded.split("\r\n") {
            assert!(part.len() <= 75);
            assert!(std::str::from_utf8(part.as_bytes()).is_ok());
        }
        assert_eq!(folded.replace("\r\n ", ""), long);
    }
}
