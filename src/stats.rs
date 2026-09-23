//! Per-day and multi-day statistics for closed rounds, and their charts.
//! Everything here is pure: it takes the round's `(value, count)` tallies
//! and returns numbers and strings, so it is tested without a database and
//! the template only has to loop.

use std::collections::HashMap;

/// How many players picked one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct Tally {
    pub value: i64,
    pub count: i64,
}

/// How many numbers the "most" and "least picked" lists show.
const TOP: usize = 5;

#[derive(Debug, PartialEq, Eq)]
pub struct DayStats {
    pub total: i64,
    pub distinct: usize,
    /// The lowest value exactly one player picked.
    pub winner: Option<i64>,
    /// The lowest positive number nobody picked at all.
    pub lowest_unpicked: i64,
    /// Most-picked first; ties go to the lower number.
    pub most: Vec<Tally>,
    /// Least-picked first, among numbers somebody did pick; ties go to the
    /// lower number.
    pub least: Vec<Tally>,
}

/// `tallies` must be sorted by value, one row per distinct value, as the
/// database returns them.
pub fn analyse(tallies: &[Tally]) -> DayStats {
    let winner = tallies.iter().find(|t| t.count == 1).map(|t| t.value);

    // Walk up from 1; the first value that is not the next expected number
    // is a hole.
    let mut lowest_unpicked = 1;
    for t in tallies {
        if t.value != lowest_unpicked {
            break;
        }
        lowest_unpicked += 1;
    }

    let mut most = tallies.to_vec();
    most.sort_by(|a, b| b.count.cmp(&a.count).then(a.value.cmp(&b.value)));
    most.truncate(TOP);

    let mut least = tallies.to_vec();
    least.sort_by(|a, b| a.count.cmp(&b.count).then(a.value.cmp(&b.value)));
    least.truncate(TOP);

    DayStats {
        total: tallies.iter().map(|t| t.count).sum(),
        distinct: tallies.len(),
        winner,
        lowest_unpicked,
        most,
        least,
    }
}


// ------------------------------------------------------------------ chart

/// The chart covers the numbers where this share of all picks (in tenths of
/// a percent) falls. Everything above is listed number by number beneath it
/// as the long tail, so nothing is grouped and nothing is dropped.
const CHART_PERMILLE: i64 = 950;
/// At least this many columns, so a quiet day is not one fat bar...
const MIN_COLS: i64 = 10;
/// ...and at most this many, so every column stays wide enough to read.
const MAX_COLS: i64 = 100;
/// Roughly how many numbers get a label along the axis.
const LABELS: i64 = 12;

/// One number's column.
pub struct Column {
    pub label: String,
    /// Whether the label is printed under the axis. Every column still has
    /// its number in the hover text.
    pub show_label: bool,
    /// "win", "once", "many" or "none" on a day page; "win", "picked" or
    /// "none" on the trends page.
    pub kind: &'static str,
    /// Bar height as a share of the tallest column, 0-100.
    pub pct: i64,
    pub count: i64,
    /// Hover text.
    pub title: String,
}

/// A number above the chart, listed with its count.
pub struct TailPick {
    pub label: String,
    pub count: i64,
    pub won: bool,
    pub title: String,
}

pub struct Chart {
    pub columns: Vec<Column>,
    /// The tallest column's count, printed as the top of the scale.
    pub max: i64,
    /// The last number with a column.
    pub span: i64,
    /// Every number picked above `span`, lowest first.
    pub tail: Vec<TailPick>,
    /// How many picks the tail holds.
    pub tail_picks: i64,
}

/// The smallest "nice" step (1, 2, 5, 10, 20, 50, ...) that splits `max`
/// into at most `parts` pieces.
fn nice_step(max: i64, parts: i64) -> i64 {
    let mut base = 1;
    loop {
        for m in [1, 2, 5] {
            let step = base * m;
            if max <= step * parts {
                return step;
            }
        }
        base *= 10;
    }
}

fn picks_label(n: i64) -> String {
    match n {
        0 => "nobody".to_string(),
        1 => "1 pick".to_string(),
        n => format!("{n} picks"),
    }
}

/// Lays out one column per number from 1 to the span, and lists the rest.
/// `style` gives a number's kind and an extra hover note; `keep` is a number
/// that should get a column (the winner) if it is within MAX_COLS.
fn build(
    tallies: &[Tally],
    keep: i64,
    style: impl Fn(i64, i64) -> (&'static str, String),
) -> Chart {
    let total: i64 = tallies.iter().map(|t| t.count).sum();
    let need = (total * CHART_PERMILLE + 999) / 1000;
    let mut covered = 0;
    let mut reach = 1;
    for t in tallies {
        covered += t.count;
        reach = t.value;
        if covered >= need {
            break;
        }
    }
    let keep = if keep <= MAX_COLS { keep } else { 0 };
    let span = reach.max(keep).clamp(MIN_COLS, MAX_COLS);
    let step = nice_step(span, LABELS);

    let mut by_value = tallies.iter().filter(|t| t.value <= span).peekable();
    let mut columns: Vec<Column> = (1..=span)
        .map(|value| {
            let count = match by_value.peek() {
                Some(t) if t.value == value => by_value.next().map_or(0, |t| t.count),
                _ => 0,
            };
            let (kind, note) = style(value, count);
            Column {
                label: value.to_string(),
                show_label: value == 1 || value % step == 0,
                kind,
                pct: 0,
                count,
                title: format!("{value}: {}{note}", picks_label(count)),
            }
        })
        .collect();

    let max = columns.iter().map(|c| c.count).max().unwrap_or(0).max(1);
    for c in &mut columns {
        // Rounded up, so a single pick always shows.
        c.pct = (c.count * 100 + max - 1) / max;
    }

    let tail: Vec<TailPick> = tallies
        .iter()
        .filter(|t| t.value > span)
        .map(|t| {
            let (kind, note) = style(t.value, t.count);
            TailPick {
                label: crate::templates::group_digits(t.value),
                count: t.count,
                won: kind == "win",
                title: format!("{}: {}{note}", t.value, picks_label(t.count)),
            }
        })
        .collect();

    Chart {
        columns,
        max,
        span,
        tail_picks: tail.iter().map(|t| t.count).sum(),
        tail,
    }
}

/// One day's spread: which numbers were shared, which were alone, and which
/// one won.
pub fn day_chart(tallies: &[Tally], stats: &DayStats) -> Chart {
    build(tallies, stats.winner.unwrap_or(0), |value, count| match count {
        0 => ("none", String::new()),
        1 if Some(value) == stats.winner => ("win", ", the winner".to_string()),
        1 => ("once", String::new()),
        _ => ("many", String::new()),
    })
}

/// Several days added together. Numbers that won at least one of those days
/// are marked, with how often in the hover text.
pub fn range_chart(trend: &Trend) -> Chart {
    let wins = |value: i64| trend.wins.iter().find(|w| w.value == value).map(|w| w.count);
    let keep = trend.wins.iter().map(|w| w.value).max().unwrap_or(0);
    build(&trend.totals, keep, |value, count| match (count, wins(value)) {
        (0, _) => ("none", String::new()),
        (_, Some(1)) => ("win", ", won 1 day".to_string()),
        (_, Some(n)) => ("win", format!(", won {n} days")),
        (_, None) => ("picked", String::new()),
    })
}

// ----------------------------------------------------------------- trends

/// How many players picked one value on one day.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DayTally {
    pub round_date: String,
    pub value: i64,
    pub count: i64,
}

/// One closed round, reduced to the numbers the timeline shows.
pub struct DayLine {
    pub date: String,
    pub players: i64,
    pub winner: Option<i64>,
    pub lowest_free: i64,
}

/// A number whose share of the picks moved between the two halves of the
/// range. Shares are in tenths of a percent, so they stay integers.
pub struct Mover {
    pub value: i64,
    pub before_permille: i64,
    pub after_permille: i64,
}

/// A number people kept coming back to.
pub struct Regular {
    pub value: i64,
    pub rounds: usize,
    /// How often it was picked across the whole range.
    pub picks: i64,
    /// Those picks as a share of all picks, in tenths of a percent.
    pub share_permille: i64,
}

/// Averages over the older and the newer half of the range, in hundredths
/// so they stay integers. `None` where a half had no winners at all.
pub struct Halves {
    /// First and last date of each half, `YYYY-MM-DD`.
    pub older: (String, String),
    pub newer: (String, String),
    pub players: (i64, i64),
    pub winner: (Option<i64>, Option<i64>),
    pub lowest_free: (i64, i64),
}

pub struct Trend {
    /// Oldest first.
    pub days: Vec<DayLine>,
    /// Every pick in the range, added up per value.
    pub totals: Vec<Tally>,
    /// How many days each winning value won, lowest value first.
    pub wins: Vec<Tally>,
    pub rising: Vec<Mover>,
    pub falling: Vec<Mover>,
    pub regulars: Vec<Regular>,
    /// Needs at least two days to compare.
    pub halves: Option<Halves>,
}

/// How many movers and regulars to list.
const TREND_TOP: usize = 3;

/// Groups rows (sorted by date, then value, as the database returns them)
/// into one tally list per day.
fn by_day(rows: &[DayTally]) -> Vec<(String, Vec<Tally>)> {
    let mut days: Vec<(String, Vec<Tally>)> = Vec::new();
    for r in rows {
        let tally = Tally {
            value: r.value,
            count: r.count,
        };
        match days.last_mut() {
            Some((date, tallies)) if *date == r.round_date => tallies.push(tally),
            _ => days.push((r.round_date.clone(), vec![tally])),
        }
    }
    days
}

/// Adds tally lists together, keeping them sorted by value.
fn sum(lists: &[&Vec<Tally>]) -> Vec<Tally> {
    let mut map = std::collections::BTreeMap::new();
    for list in lists {
        for t in list.iter() {
            *map.entry(t.value).or_insert(0) += t.count;
        }
    }
    map.into_iter()
        .map(|(value, count)| Tally { value, count })
        .collect()
}

fn avg(xs: impl Iterator<Item = i64>) -> Option<i64> {
    let (total, n) = xs.fold((0, 0), |(t, n), x| (t + x, n + 1));
    (n > 0).then(|| total * 100 / n)
}

pub fn trend(rows: &[DayTally]) -> Trend {
    let days = by_day(rows);
    let stats: Vec<DayStats> = days.iter().map(|(_, t)| analyse(t)).collect();

    let lines: Vec<DayLine> = days
        .iter()
        .zip(&stats)
        .map(|((date, _), s)| DayLine {
            date: date.clone(),
            players: s.total,
            winner: s.winner,
            lowest_free: s.lowest_unpicked,
        })
        .collect();

    let totals = sum(&days.iter().map(|(_, t)| t).collect::<Vec<_>>());
    let won_days: Vec<Tally> = stats
        .iter()
        .filter_map(|s| s.winner)
        .map(|value| Tally { value, count: 1 })
        .collect();
    let wins = sum(&[&won_days]);

    // Split into an older and a newer half; an odd middle day goes to the
    // newer one.
    let mid = days.len() / 2;
    let (old, new) = days.split_at(mid);
    let old_totals = sum(&old.iter().map(|(_, t)| t).collect::<Vec<_>>());
    let new_totals = sum(&new.iter().map(|(_, t)| t).collect::<Vec<_>>());
    let old_n: i64 = old_totals.iter().map(|t| t.count).sum();
    let new_n: i64 = new_totals.iter().map(|t| t.count).sum();
    // Maps, not scans: a year of rounds holds tens of thousands of distinct
    // values, and looking each one up in a list is quadratic.
    let old_map: HashMap<i64, i64> = old_totals.iter().map(|t| (t.value, t.count)).collect();
    let new_map: HashMap<i64, i64> = new_totals.iter().map(|t| (t.value, t.count)).collect();
    let share = |map: &HashMap<i64, i64>, n: i64, value: i64| {
        let c = map.get(&value).copied().unwrap_or(0);
        if n == 0 { 0 } else { c * 1000 / n }
    };

    let mut movers: Vec<Mover> = if old_n > 0 && new_n > 0 {
        totals
            .iter()
            // One-off picks are noise, not a trend.
            .filter(|t| t.count >= 2)
            .map(|t| Mover {
                value: t.value,
                before_permille: share(&old_map, old_n, t.value),
                after_permille: share(&new_map, new_n, t.value),
            })
            .collect()
    } else {
        Vec::new()
    };
    let delta = |m: &Mover| m.after_permille - m.before_permille;
    movers.sort_by(|a, b| delta(b).cmp(&delta(a)).then(a.value.cmp(&b.value)));
    let rising = movers
        .iter()
        .filter(|m| delta(m) > 0)
        .take(TREND_TOP)
        .map(|m| Mover { ..*m })
        .collect();
    let falling = movers
        .iter()
        .rev()
        .filter(|m| delta(m) < 0)
        .take(TREND_TOP)
        .map(|m| Mover { ..*m })
        .collect();

    // Each day lists a value at most once, so counting rows per value counts
    // the days it was picked on.
    let mut seen: HashMap<i64, usize> = HashMap::new();
    for (_, list) in &days {
        for t in list {
            *seen.entry(t.value).or_insert(0) += 1;
        }
    }
    let all_picks: i64 = totals.iter().map(|t| t.count).sum();
    let picks_of: HashMap<i64, i64> = totals.iter().map(|t| (t.value, t.count)).collect();
    let mut regulars: Vec<Regular> = seen
        .into_iter()
        .filter(|&(_, rounds)| rounds >= 2)
        .map(|(value, rounds)| {
            let picks = picks_of.get(&value).copied().unwrap_or(0);
            Regular {
                value,
                rounds,
                picks,
                share_permille: if all_picks == 0 { 0 } else { picks * 1000 / all_picks },
            }
        })
        .collect();
    regulars.sort_by(|a, b| b.rounds.cmp(&a.rounds).then(a.value.cmp(&b.value)));
    regulars.truncate(TREND_TOP);

    let halves = (days.len() >= 2).then(|| {
        let (a, b) = lines.split_at(mid);
        let span = |part: &[DayLine]| {
            (
                part.first().map(|d| d.date.clone()).unwrap_or_default(),
                part.last().map(|d| d.date.clone()).unwrap_or_default(),
            )
        };
        Halves {
            older: span(a),
            newer: span(b),
            players: (
                avg(a.iter().map(|d| d.players)).unwrap_or(0),
                avg(b.iter().map(|d| d.players)).unwrap_or(0),
            ),
            winner: (
                avg(a.iter().filter_map(|d| d.winner)),
                avg(b.iter().filter_map(|d| d.winner)),
            ),
            lowest_free: (
                avg(a.iter().map(|d| d.lowest_free)).unwrap_or(0),
                avg(b.iter().map(|d| d.lowest_free)).unwrap_or(0),
            ),
        }
    });

    Trend {
        days: lines,
        totals,
        wins,
        rising,
        falling,
        regulars,
        halves,
    }
}

/// `12.5` from hundredths, trimming a pointless `.0`.
pub fn hundredths(n: i64) -> String {
    let whole = n / 100;
    let tenth = (n % 100) / 10;
    if tenth == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{tenth}")
    }
}

/// `+47.6` / `-0.8` from a difference in hundredths.
pub fn signed_hundredths(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "+" };
    format!("{sign}{}", hundredths(n.abs()))
}

/// `+28%` for the relative change from `before` to `after`; empty when
/// `before` is zero, where a percentage means nothing.
pub fn percent_change(before: i64, after: i64) -> String {
    if before == 0 {
        return String::new();
    }
    let pct = (after - before) * 100 / before;
    if pct >= 0 {
        format!("+{pct}%")
    } else {
        format!("{pct}%")
    }
}

/// `+1.7 pts` / `-0.9 pts`: a difference between two shares, in percentage
/// points, from tenths of a percent.
pub fn permille_points(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "+" };
    let n = n.abs();
    if n % 10 == 0 {
        format!("{sign}{} pts", n / 10)
    } else {
        format!("{sign}{}.{} pts", n / 10, n % 10)
    }
}

/// `12.5%` from tenths of a percent.
pub fn permille(n: i64) -> String {
    let (whole, tenth) = (n / 10, n % 10);
    if tenth == 0 {
        format!("{whole}%")
    } else {
        format!("{whole}.{tenth}%")
    }
}

// ---------------------------------------------------------- the round bar

/// `[#########-----------]`: the share of the round already gone, as a text
/// bar. Rounds are taken as a flat 24 hours; on the two DST days a year it is
/// off by an hour's worth, which is decoration, not game logic. app.js draws
/// the same bar every second.
pub fn progress_bar(seconds_left: i64) -> (i64, String) {
    const DAY: i64 = 24 * 60 * 60;
    const CELLS: i64 = 30;
    let done = (DAY - seconds_left).clamp(0, DAY);
    let filled = done * CELLS / DAY;
    let bar = format!(
        "[{}{}]",
        "#".repeat(filled as usize),
        "-".repeat((CELLS - filled) as usize)
    );
    (done * 100 / DAY, bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pairs: &[(i64, i64)]) -> Vec<Tally> {
        pairs
            .iter()
            .map(|&(value, count)| Tally { value, count })
            .collect()
    }

    fn day(date: &str, pairs: &[(i64, i64)]) -> Vec<DayTally> {
        pairs
            .iter()
            .map(|&(value, count)| DayTally {
                round_date: date.to_string(),
                value,
                count,
            })
            .collect()
    }

    #[test]
    fn finds_the_winner_the_first_hole_and_the_extremes() {
        // 1 and 2 collided, 3 is unique, nobody took 4, 7 is unique too.
        let tallies = t(&[(1, 4), (2, 2), (3, 1), (5, 3), (7, 1)]);
        let s = analyse(&tallies);
        assert_eq!(s.total, 11);
        assert_eq!(s.distinct, 5);
        assert_eq!(s.winner, Some(3));
        assert_eq!(s.lowest_unpicked, 4);
        assert_eq!(s.most, t(&[(1, 4), (5, 3), (2, 2), (3, 1), (7, 1)]));
        assert_eq!(s.least, t(&[(3, 1), (7, 1), (2, 2), (5, 3), (1, 4)]));
    }

    #[test]
    fn a_round_where_everything_collided_has_no_winner() {
        let s = analyse(&t(&[(1, 2), (2, 3)]));
        assert_eq!(s.winner, None);
        assert_eq!(s.lowest_unpicked, 3);
    }

    #[test]
    fn if_nobody_took_1_then_1_is_the_hole() {
        let s = analyse(&t(&[(2, 1), (3, 1)]));
        assert_eq!(s.lowest_unpicked, 1);
        assert_eq!(s.winner, Some(2));
    }

    #[test]
    fn the_lists_stop_at_five() {
        let tallies: Vec<Tally> = (1..=9).map(|v| Tally { value: v, count: v }).collect();
        let s = analyse(&tallies);
        assert_eq!(s.most.len(), 5);
        assert_eq!(s.most[0].value, 9);
        assert_eq!(s.least.len(), 5);
        assert_eq!(s.least[0].value, 1);
    }

    #[test]
    fn the_chart_covers_the_bulk_and_lists_every_higher_pick() {
        // 200 picks on 1..=40, then a few stray high ones.
        let mut pairs: Vec<(i64, i64)> = (1..=40).map(|v| (v, 5)).collect();
        pairs.extend([(136, 2), (420, 1), (90_000, 1)]);
        let tallies = t(&pairs);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.span, 39); // 95% of 204 picks are in by 39
        assert_eq!(c.columns.len(), 39);

        // Nothing is dropped: the rest is listed number by number.
        let tail: Vec<(&str, i64)> = c.tail.iter().map(|t| (t.label.as_str(), t.count)).collect();
        assert_eq!(tail, [("40", 5), ("136", 2), ("420", 1), ("90\u{202f}000", 1)]);
        assert_eq!(c.tail_picks, 9);
        let drawn: i64 = c.columns.iter().map(|c| c.count).sum();
        assert_eq!(drawn + c.tail_picks, 204);

        // 420 and 90 000 were each picked once; the lower one won.
        assert!(c.tail[2].won);
        assert!(!c.tail[3].won);

        // Labels on 1 and every fifth number, so they never collide.
        let labelled: Vec<&str> = c
            .columns
            .iter()
            .filter(|c| c.show_label)
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(labelled, ["1", "5", "10", "15", "20", "25", "30", "35"]);
    }

    #[test]
    fn a_small_day_keeps_at_least_ten_columns() {
        let tallies = t(&[(1, 1), (4, 2)]);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.columns.len(), 10);
        assert_eq!(c.columns[0].kind, "win");
        assert_eq!(c.columns[0].title, "1: 1 pick, the winner");
        assert_eq!(c.columns[1].kind, "none");
        assert_eq!(c.columns[3].kind, "many");
        assert_eq!(c.columns[3].pct, 100);
        assert_eq!(c.columns[0].pct, 50);
        assert!(c.tail.is_empty());
    }

    #[test]
    fn a_winner_gets_a_column_if_it_is_close_enough() {
        let mut pairs: Vec<(i64, i64)> = (1..=40).map(|v| (v, 2)).collect();
        pairs.push((77, 1));
        let tallies = t(&pairs);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.span, 77);
        assert_eq!(c.columns[76].kind, "win");
    }

    #[test]
    fn nice_steps_are_1_2_5_by_powers_of_ten() {
        assert_eq!(nice_step(10, 12), 1);
        assert_eq!(nice_step(40, 12), 5);
        assert_eq!(nice_step(100, 12), 10);
    }

    #[test]
    fn the_progress_bar_fills_as_the_day_goes() {
        assert_eq!(progress_bar(24 * 60 * 60), (0, format!("[{}]", "-".repeat(30))));
        assert_eq!(progress_bar(12 * 60 * 60).0, 50);
        assert_eq!(progress_bar(0), (100, format!("[{}]", "#".repeat(30))));
    }

    #[test]
    fn a_trend_adds_the_days_up_and_compares_the_halves() {
        let mut rows = day("2026-09-01", &[(1, 3), (2, 1)]); // 2 wins, 4 players
        rows.extend(day("2026-09-02", &[(1, 2), (3, 1)])); // 3 wins, 3 players
        rows.extend(day("2026-09-03", &[(1, 1), (2, 2), (5, 3)])); // 1 wins, 6 players
        rows.extend(day("2026-09-04", &[(2, 4), (3, 2)])); // nobody, 6 players
        let tr = trend(&rows);

        assert_eq!(tr.days.len(), 4);
        assert_eq!(tr.days[0].winner, Some(2));
        assert_eq!(tr.days[3].winner, None);
        assert_eq!(tr.days[3].lowest_free, 1);
        assert_eq!(tr.totals, t(&[(1, 6), (2, 7), (3, 3), (5, 3)]));
        assert_eq!(tr.wins, t(&[(1, 1), (2, 1), (3, 1)]));

        // Old half: 7 picks, 1 has 5 of them. New half: 12 picks, 1 has 1.
        assert_eq!(tr.falling[0].value, 1);
        assert_eq!(tr.falling[0].before_permille, 714);
        assert_eq!(tr.falling[0].after_permille, 83);
        assert_eq!(tr.rising[0].value, 2);

        // 1, 2 and 3 each turned up on several days; 5 only once.
        let regulars: Vec<(i64, usize)> = tr.regulars.iter().map(|r| (r.value, r.rounds)).collect();
        assert_eq!(regulars, [(1, 3), (2, 3), (3, 2)]);
        // 1 was picked 6 times out of 19.
        assert_eq!(tr.regulars[0].picks, 6);
        assert_eq!(tr.regulars[0].share_permille, 315);

        let h = tr.halves.unwrap();
        assert_eq!(h.older, ("2026-09-01".to_string(), "2026-09-02".to_string()));
        assert_eq!(h.newer, ("2026-09-03".to_string(), "2026-09-04".to_string()));
        assert_eq!(h.players, (350, 600));
        assert_eq!(h.winner, (Some(250), Some(100)));
    }

    #[test]
    fn a_single_day_has_nothing_to_compare() {
        let tr = trend(&day("2026-09-01", &[(1, 1), (2, 2)]));
        assert!(tr.halves.is_none());
        assert!(tr.rising.is_empty() && tr.falling.is_empty());
    }

    #[test]
    fn changes_are_printed_with_sign_and_unit() {
        assert_eq!(signed_hundredths(4760), "+47.6");
        assert_eq!(signed_hundredths(-80), "-0.8");
        assert_eq!(percent_change(1696, 2172), "+28%");
        assert_eq!(percent_change(1600, 1520), "-5%");
        assert_eq!(percent_change(0, 5), "");
        assert_eq!(permille_points(17), "+1.7 pts");
        assert_eq!(permille_points(-20), "-2 pts");
    }

    #[test]
    fn fractions_are_printed_plainly() {
        assert_eq!(hundredths(350), "3.5");
        assert_eq!(hundredths(600), "6");
        assert_eq!(permille(714), "71.4%");
        assert_eq!(permille(250), "25%");
    }
}
