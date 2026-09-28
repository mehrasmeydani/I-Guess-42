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

/// How many numbers the "most picked", "least picked" and "never picked"
/// lists show, on a day page and over a whole range alike.
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
/// A run of this many numbers with nothing on them ends the chart. Past such a
/// gap the columns are a flat line of zeros, and whatever sits out there is an
/// outlier the tail can list by name. Only once most of the picks are already
/// on the axis, so a day where the crowd went high is still drawn rather than
/// pushed wholesale into the tail.
const GAP: i64 = 10;

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
        // Coverage alone cannot tell an outlier from the crowd on a quiet day:
        // 95% of eighteen picks is all eighteen, so one player off at 100 used
        // to drag the axis out there with ninety empty columns behind it. A
        // wide stretch of nothing is the other end of the bulk.
        if covered * 2 >= total && t.value - reach > GAP {
            break;
        }
        covered += t.count;
        reach = t.value;
        if covered >= need {
            break;
        }
    }
    let keep = if keep <= MAX_COLS { keep } else { 0 };
    let reach = reach.max(keep).clamp(MIN_COLS, MAX_COLS);
    // A handful of stray high picks can drag `reach` up, or run it straight
    // into MAX_COLS, long after the last number anyone actually went near.
    // Those picks are in the tail either way and all they leave behind is
    // empty columns, so the axis stops at the last number with something on
    // it -- the winner's column included, which is why `keep` is in here too.
    let span = tallies
        .iter()
        .map(|t| t.value)
        .filter(|&v| v <= reach)
        .max()
        .unwrap_or(0)
        .max(keep)
        .max(MIN_COLS);
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
/// are marked, and the hover says *when* - a green bar is otherwise a fact
/// with no date on it. `trend.days` already carries every day's winner, so
/// the dates cost no extra query.
///
/// Over a long range a number can win more than once, and the hover cannot
/// grow with it: one win names its day, several give the count and the most
/// recent. `trend.days` is oldest first, so that is the last one.
pub fn range_chart(trend: &Trend) -> Chart {
    let mut won_on: HashMap<i64, Vec<&str>> = HashMap::new();
    for day in &trend.days {
        if let Some(value) = day.winner {
            won_on.entry(value).or_default().push(&day.date);
        }
    }
    let keep = trend.wins.iter().map(|w| w.value).max().unwrap_or(0);
    build(&trend.totals, keep, |value, count| {
        if count == 0 {
            return ("none", String::new());
        }
        match won_on.get(&value).map(Vec::as_slice) {
            None => ("picked", String::new()),
            Some([date]) => ("win", format!(", won {date}")),
            Some(dates) => (
                "win",
                format!(", won {} days, last {}", dates.len(), dates[dates.len() - 1]),
            ),
        }
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
    /// The crowd's favourites over the whole range, most picked first; ties go
    /// to the lower number.
    pub most: Vec<Tally>,
    /// The lowest numbers nobody picked on any day of the range. Every one of
    /// them would have won each of those rounds, which is the useful part.
    pub never: Vec<i64>,
    /// Needs at least two days to compare.
    pub halves: Option<Halves>,
}

/// How many rising and falling numbers to list.
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

    let mut most = totals.clone();
    most.sort_by(|a, b| b.count.cmp(&a.count).then(a.value.cmp(&b.value)));
    most.truncate(TOP);

    // Walk up from 1 and collect the holes. `totals` is sorted by value, so
    // the numbers that were picked are stepped through alongside the count,
    // rather than searched for once per candidate.
    let mut never = Vec::with_capacity(TOP);
    let mut picked = totals.iter().peekable();
    let mut value = 1;
    while never.len() < TOP {
        match picked.peek() {
            Some(t) if t.value == value => {
                picked.next();
            }
            _ => never.push(value),
        }
        value += 1;
    }

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
        most,
        never,
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

/// How many cells the bar is drawn with server-side. The bar should run the
/// full width of the page, but how many characters that is depends on the
/// screen, and the server cannot see the screen. So this is the short bar
/// that fits the narrowest phone without spilling; app.js measures the row
/// and redraws it wider. A visitor without JavaScript keeps this one.
const BAR_CELLS: i64 = 30;

/// `[#########-----------]`: the share of the round already gone, as a text
/// bar. Rounds are taken as a flat 24 hours; on the two DST days a year it is
/// off by an hour's worth, which is decoration, not game logic. app.js draws
/// the same bar every second, at [`BAR_CELLS`] or wider.
pub fn progress_bar(seconds_left: i64) -> (i64, String) {
    const DAY: i64 = 24 * 60 * 60;
    const CELLS: i64 = BAR_CELLS;
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
    fn a_lone_high_pick_does_not_stretch_a_quiet_day_across_the_page() {
        // Eighteen players, seventeen of them under 18 and one off at 100.
        // 95% of eighteen picks is all eighteen, so coverage alone kept 100 on
        // the axis with eighty-odd empty columns in front of it.
        let mut pairs: Vec<(i64, i64)> = (1..=17).map(|v| (v, 1)).collect();
        pairs.push((100, 1));
        let tallies = t(&pairs);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.span, 17);
        assert_eq!(c.tail.len(), 1, "the stray pick is listed, not drawn");
        assert_eq!(c.tail[0].label, "100");
        assert_eq!(c.tail_picks, 1);
    }

    #[test]
    fn a_crowd_that_went_high_is_still_drawn_rather_than_all_tail() {
        // The gap rule only fires once most of the picks are on the axis:
        // here they are all up in the sixties, and cutting at the first gap
        // would leave an empty chart and a tail holding the whole round.
        let tallies = t(&[(1, 1), (61, 3), (62, 2), (63, 4)]);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.span, 63);
        assert!(c.tail.is_empty());
    }

    #[test]
    fn the_axis_never_ends_in_a_run_of_empty_columns() {
        // One pick low and eleven far out of reach: covering them runs past
        // MAX_COLS, and a hundred columns for a bar on 5 is ninety-five of
        // them empty.
        let mut pairs = vec![(5, 1)];
        pairs.extend((150..=160).map(|v| (v, 1)));
        let tallies = t(&pairs);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.span, MIN_COLS);
        assert_eq!(c.tail.len(), 11);
    }

    #[test]
    fn a_stray_high_pick_does_not_leave_the_axis_running_into_nothing() {
        // Eleven players, ten of them on 1..=10 and one off at 200. Covering
        // 95% of so few picks needs every one of them, so the reach runs past
        // MAX_COLS -- but 200 is listed in the tail, and the axis has no
        // business carrying on out there after it.
        let mut pairs: Vec<(i64, i64)> = (1..=10).map(|v| (v, 1)).collect();
        pairs.push((200, 1));
        let tallies = t(&pairs);
        let c = day_chart(&tallies, &analyse(&tallies));
        assert_eq!(c.span, 10);
        assert_eq!(c.columns.len(), 10);
        assert_eq!(c.tail.len(), 1, "the stray pick is still listed");
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
        // Bracketed, and as many cells as the server draws -- app.js counts on
        // both, so neither is free to drift.
        let cells = BAR_CELLS as usize;
        assert_eq!(progress_bar(24 * 60 * 60), (0, format!("[{}]", "-".repeat(cells))));
        assert_eq!(progress_bar(12 * 60 * 60).0, 50);
        assert_eq!(progress_bar(0), (100, format!("[{}]", "#".repeat(cells))));
        let (_, half) = progress_bar(12 * 60 * 60);
        assert_eq!(half.len(), cells + 2);
        assert_eq!(half.matches('#').count(), cells / 2);
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

        // 2 took 7 of the 19 picks, 1 took 6; ties below them go to the lower
        // number. Nobody ever took 4, and 6 upwards is untouched too.
        assert_eq!(tr.most, t(&[(2, 7), (1, 6), (3, 3), (5, 3)]));
        assert_eq!(tr.never, [4, 6, 7, 8, 9]);

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

    #[test]
    fn a_green_bar_says_which_day_that_number_won() {
        // 7 is alone on the first and third day and wins both; on the second
        // day 3 is also alone and lower, so it takes that one. 9 and 5 are
        // shared, so they never win.
        let mut rows = day("2026-01-01", &[(7, 1), (9, 2)]);
        rows.extend(day("2026-01-02", &[(3, 1), (7, 1)]));
        rows.extend(day("2026-01-03", &[(5, 2), (7, 1)]));
        let c = range_chart(&trend(&rows));

        let title = |label: &str| {
            c.columns
                .iter()
                .find(|col| col.label == label)
                .map(|col| col.title.as_str())
                .unwrap_or("<no such column>")
        };

        // More than one win: the count, plus the most recent day - not every
        // date, which would grow with the range.
        assert_eq!(title("7"), "7: 3 picks, won 2 days, last 2026-01-03");
        // Exactly one win names its day.
        assert_eq!(title("3"), "3: 1 pick, won 2026-01-02");
        // Never won, so no date and no green.
        assert_eq!(title("9"), "9: 2 picks");
        let kind = |label: &str| c.columns.iter().find(|col| col.label == label).unwrap().kind;
        assert_eq!(kind("7"), "win");
        assert_eq!(kind("9"), "picked");
    }

}
