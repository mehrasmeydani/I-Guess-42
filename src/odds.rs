//! Bets on the global round and their odds (SPEC.md section 9): pure
//! functions, so they are easy to test. Everything stored or shown as points
//! is an integer; floating point stays in here.
//!
//! A bet is on the round's winning number W - the lowest number exactly one
//! player picked - landing in a range the player chooses, or on there being
//! no winner at all; or on the most picked number M landing in a range.
//! Odds come from two things:
//!
//! - Last month's picks, worked out exactly: with the usual number of
//!   players drawing from what people actually picked, how likely each number
//!   is to be the lowest picked once, or the most picked (see `analyse`).
//!   That is a range's starting chance.
//! - The book: points already bet count as evidence, like money moving a
//!   bookmaker's prices. The more is staked on numbers in a range, the
//!   likelier it looks and the less it pays. Odds only ever shorten: a range
//!   never pays more than its starting odds, so a big bet on one side cannot
//!   be used to lengthen the other and then bet that at a false price.
//!
//! The house keeps `MARGIN_PCT` of fair odds - the tax - so every bet loses a
//! little on average and the near-certain ones ("1 or more") pay less than
//! they cost and are not offered at all.

/// The house keeps this much of fair odds (8%).
const MARGIN_PCT: i64 = 8;
/// How many days of picks the chances are worked out from.
pub const HISTORY_DAYS: i64 = 30;
/// The last this many days of it count twice: the month for breadth, the
/// week so the odds follow how people pick now.
pub const RECENT_DAYS: i64 = 7;
/// The most a single bet can win, over its stake, per player a day: a bet in
/// a round of 30 players a day wins 3000 at most, however long the odds or
/// big the stake. The game's size, not a lucky few, sets what is at stake.
pub const PROFIT_PER_PLAYER: i64 = 100;
/// Below this many global picks last week, the 42 round's picks fill in.
const MIN_PICKS: usize = 30;
/// Players a round, at most.
const MAX_PLAYERS: usize = 1000;
/// Without a week to go on, rounds are imagined this size.
const DEFAULT_PLAYERS: usize = 10;
/// Share of every chance taken from a smooth curve instead of the
/// picks, so a range nobody picked last month is unlikely, not impossible.
const SMOOTH: f64 = 0.05;
/// That curve's chance of nobody winning.
const NOBODY_PRIOR: f64 = 0.05;
/// How many points the starting chances are worth against bets: 1000 points
/// staked on a range move it as far as all of last month's picks pull.
const LIQUIDITY: f64 = 1000.0;
/// No bet is ever certain, nor ever impossible: a long shot is 0.01% at
/// least, which pays about ×9200.
const P_MIN: f64 = 0.0001;
const P_MAX: f64 = 0.99;
/// Odds are offered between these, in hundredths. Anything that would pay
/// less than ×1.05 is too safe to bet on; the top is out of reach of
/// `P_MIN`, there only so no number can run away.
const ODDS_MIN: i64 = 105;
const ODDS_MAX: i64 = 1_000_000;
/// The largest number a range may name.
pub const MAX_LINE: i64 = 1_000_000_000;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Market {
    /// W exists and lo <= W, and W <= hi if there is a hi.
    Range { lo: i64, hi: Option<i64> },
    /// Nobody wins: every number was picked more than once.
    Nobody,
    /// M, the most picked number (ties: the lowest of them), is in the
    /// range: lo <= M, and M <= hi if there is a hi.
    Most { lo: i64, hi: Option<i64> },
}

/// What a round came to, as bets are judged: its winning number, if any,
/// and its most picked number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    pub winner: Option<i64>,
    pub most: Option<i64>,
}

impl Market {
    /// A range, if it is one: 1 <= lo <= hi <= `MAX_LINE`.
    pub fn range(lo: i64, hi: Option<i64>) -> Option<Market> {
        let ok = (1..=MAX_LINE).contains(&lo) && hi.is_none_or(|h| (lo..=MAX_LINE).contains(&h));
        ok.then_some(Market::Range { lo, hi })
    }

    /// The same range, on the most picked number instead.
    pub fn most(self) -> Market {
        match self {
            Market::Range { lo, hi } => Market::Most { lo, hi },
            m => m,
        }
    }

    pub fn is_most(self) -> bool {
        matches!(self, Market::Most { .. })
    }

    /// From the bets page's two boxes: an empty top means "or more".
    pub fn from_inputs(lo: &str, hi: &str) -> Option<Market> {
        let lo = lo.trim().parse().ok()?;
        let hi = match hi.trim() {
            "" => None,
            h => Some(h.parse().ok()?),
        };
        Market::range(lo, hi)
    }

    /// As stored: "range:5:12", "range:10:" (no top), "none", "most:1:3".
    pub fn key(self) -> String {
        let top = |hi: Option<i64>| hi.map(|h| h.to_string()).unwrap_or_default();
        match self {
            Market::Range { lo, hi } => format!("range:{lo}:{}", top(hi)),
            Market::Most { lo, hi } => format!("most:{lo}:{}", top(hi)),
            Market::Nobody => "none".to_string(),
        }
    }

    /// Only well-formed markets parse. The first bets were stored as
    /// "under:N" and "over:N"; those still read as the ranges they were.
    pub fn parse(key: &str) -> Option<Market> {
        if key == "none" {
            return Some(Market::Nobody);
        }
        let num = |s: &str| s.parse::<i64>().ok().filter(|_| s.bytes().all(|b| b.is_ascii_digit()));
        if let Some((kind, rest)) = key.split_once(':').filter(|(k, _)| *k == "range" || *k == "most") {
            let (lo, hi) = rest.split_once(':')?;
            let hi = if hi.is_empty() { None } else { Some(num(hi)?) };
            let range = Market::range(num(lo)?, hi)?;
            return Some(if kind == "most" { range.most() } else { range });
        }
        if let Some(n) = key.strip_prefix("under:") {
            return Market::range(1, Some(num(n)? - 1));
        }
        if let Some(n) = key.strip_prefix("over:") {
            return Market::range(num(n)?, None);
        }
        None
    }

    /// As a page shows it.
    pub fn label(self) -> String {
        match self {
            Market::Range { lo: 1, hi: None } => "any winner".to_string(),
            Market::Range { lo, hi: None } => format!("winner {lo} or more"),
            Market::Range { lo, hi: Some(h) } if lo == h => format!("winner exactly {lo}"),
            Market::Range { lo, hi: Some(h) } => format!("winner {lo} to {h}"),
            Market::Nobody => "nobody wins".to_string(),
            Market::Most { lo: 1, hi: None } => "any most picked".to_string(),
            Market::Most { lo, hi: None } => format!("most picked {lo} or more"),
            Market::Most { lo, hi: Some(h) } if lo == h => format!("most picked exactly {lo}"),
            Market::Most { lo, hi: Some(h) } => format!("most picked {lo} to {h}"),
        }
    }

    /// What the bets page's two boxes hold for it.
    pub fn inputs(self) -> (String, String) {
        match self {
            Market::Range { lo, hi } | Market::Most { lo, hi } => (lo.to_string(), hi.map(|h| h.to_string()).unwrap_or_default()),
            Market::Nobody => (String::new(), String::new()),
        }
    }

    /// The outcomes both markets win on, if any. Bets on the winner and on
    /// the most picked number never overlap: they are separate books.
    fn overlap(self, other: Market) -> Option<Market> {
        let meet = |a: i64, x: Option<i64>, b: i64, y: Option<i64>| {
            let lo = a.max(b);
            let hi = match (x, y) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (h, None) | (None, h) => h,
            };
            hi.is_none_or(|h| lo <= h).then_some((lo, hi))
        };
        match (self, other) {
            (Market::Nobody, Market::Nobody) => Some(Market::Nobody),
            (Market::Range { lo: a, hi: x }, Market::Range { lo: b, hi: y }) => meet(a, x, b, y).map(|(lo, hi)| Market::Range { lo, hi }),
            (Market::Most { lo: a, hi: x }, Market::Most { lo: b, hi: y }) => meet(a, x, b, y).map(|(lo, hi)| Market::Most { lo, hi }),
            _ => None,
        }
    }
}

const BOARD: [(i64, Option<i64>); 8] = [(1, Some(1)), (2, Some(2)), (3, Some(3)), (4, Some(5)), (6, Some(9)), (10, Some(19)), (20, Some(49)), (50, None)];

/// The ranges the bets page lists ready-made for the winner. Together with
/// "nobody wins" they cover every outcome once, so their chances add up to 100%.
pub fn board() -> Vec<Market> {
    let mut v: Vec<Market> = BOARD.into_iter().map(|(lo, hi)| Market::Range { lo, hi }).collect();
    v.push(Market::Nobody);
    v
}

/// The same ranges for the most picked number, which every played round has.
pub fn board_most() -> Vec<Market> {
    BOARD.into_iter().map(|(lo, hi)| Market::Most { lo, hi }).collect()
}

fn within(lo: i64, hi: Option<i64>, n: i64) -> bool {
    lo <= n && hi.is_none_or(|h| n <= h)
}

/// Whether `market` won. Winner ranges lose when nobody wins, as at any
/// bookmaker; that is what gives "nobody wins" its meaning.
pub fn outcome(market: Market, o: Outcome) -> bool {
    match market {
        Market::Range { lo, hi } => o.winner.is_some_and(|w| within(lo, hi, w)),
        Market::Nobody => o.winner.is_none(),
        Market::Most { lo, hi } => o.most.is_some_and(|m| within(lo, hi, m)),
    }
}

/// A run of past rounds: the picks to draw from (recent days twice), how
/// many rounds they came from, and how many guesses there really were.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub picks: Vec<i64>,
    pub rounds: usize,
    pub plays: usize,
}

impl History {
    /// From per-day tallies, as `db::range_tallies` gives them. Days from
    /// `recent_from` on are drawn from twice.
    pub fn from_tallies(rows: &[crate::stats::DayTally], recent_from: &str) -> History {
        let mut picks = Vec::new();
        let mut days: Vec<&str> = Vec::new();
        let mut plays = 0;
        for r in rows {
            let n = r.count.max(0) as usize;
            let times = if r.round_date.as_str() >= recent_from { 2 } else { 1 };
            picks.extend(std::iter::repeat_n(r.value, n * times));
            plays += n;
            if days.last() != Some(&r.round_date.as_str()) {
                days.push(&r.round_date);
            }
        }
        days.dedup();
        History { picks, rounds: days.len(), plays }
    }

    /// Players a day, on average.
    fn players(&self) -> Option<usize> {
        (self.rounds > 0).then(|| self.plays.div_ceil(self.rounds))
    }
}

/// Where a day's chances start: each number's chance to win, or to be the
/// most picked, before any bets.
#[derive(Debug, Clone, PartialEq)]
pub struct Base {
    /// False when there were no picks to go on: the smooth curve alone.
    pub has_data: bool,
    /// (number, chance it wins), lowest first.
    winners: Vec<(i64, f64)>,
    nobody: f64,
    /// (number, chance it is the most picked), lowest first.
    modes: Vec<(i64, f64)>,
    /// Picks the chances were worked out from, and players a round.
    pub picks: usize,
    pub players: usize,
}

impl Base {
    /// The most one bet may win today over its stake: players a day times
    /// `PROFIT_PER_PLAYER`.
    pub fn max_profit(&self) -> i64 {
        self.players.max(1) as i64 * PROFIT_PER_PLAYER
    }

    /// The starting chance of `market`: mostly the picks, a little the
    /// smooth curve; all of the curve if there were no picks.
    fn chance(&self, market: Market) -> f64 {
        let (smooth, share) = if !self.has_data {
            (1.0, 0.0)
        } else {
            let sum = |v: &[(i64, f64)], lo, hi| v.iter().filter(|(n, _)| within(lo, hi, *n)).map(|(_, p)| p).sum();
            let share = match market {
                Market::Nobody => self.nobody,
                Market::Range { lo, hi } => sum(&self.winners, lo, hi),
                Market::Most { lo, hi } => sum(&self.modes, lo, hi),
            };
            (SMOOTH, share)
        };
        let curve = match market {
            Market::Nobody => NOBODY_PRIOR,
            Market::Range { lo, hi } => (1.0 - NOBODY_PRIOR) * curve(lo, hi),
            Market::Most { lo, hi } => curve(lo, hi),
        };
        (1.0 - smooth) * share + smooth * curve
    }
}

/// The smooth curve's share of winners from `lo` to `hi`: low numbers win
/// far more than high ones, falling off as 1/n² (an integral, so any range
/// is one sum). From 1 with no top, it is all of them.
fn curve(lo: i64, hi: Option<i64>) -> f64 {
    let top = hi.map_or(0.0, |h| 1.0 / (h as f64 + 0.5));
    (1.0 / (lo as f64 - 0.5) - top) / 2.0
}

/// The chance a pick count with mean `l` is exactly one.
fn once(l: f64) -> f64 {
    l * (-l).exp()
}

/// The chances, worked out exactly rather than played out. `players` people
/// each pick one of `picks` at random. Each number's count is then near
/// enough independent, a Poisson count with mean λ = players × its share of
/// the picks (exact when the day's number of players is itself uncertain,
/// as it is). So:
///
/// - v wins when exactly one picks it and no lower number has exactly one:
///   P(v) = λᵥ·e^(−λᵥ) × Π over u < v of (1 − λᵤ·e^(−λᵤ)).
/// - Nobody wins: the same product over every number.
/// - v is the most picked (ties to the lowest) when, for the count k it
///   has, every lower number has fewer than k and every higher one at most
///   k: summed over k. Then scaled to add up to 1, leaving out the day
///   nobody plays.
fn analyse(picks: &[i64], players: usize) -> Base {
    let mut counts: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
    for p in picks {
        *counts.entry(*p).or_default() += 1;
    }
    let n = players as f64;
    let total = picks.len() as f64;
    let means: Vec<(i64, f64)> = counts.into_iter().map(|(v, c)| (v, n * c as f64 / total)).collect();

    let mut nobody_yet = 1.0;
    let mut winners = Vec::with_capacity(means.len());
    for &(v, l) in &means {
        let p = once(l);
        winners.push((v, nobody_yet * p));
        nobody_yet *= 1.0 - p;
    }
    Base { has_data: true, winners, nobody: nobody_yet, modes: modes(&means), picks: picks.len(), players }
}

/// Each number's chance of being the most picked, ties to the lowest, for
/// independent Poisson counts with these means (see `analyse`).
fn modes(means: &[(i64, f64)]) -> Vec<(i64, f64)> {
    let top = means.iter().map(|m| m.1).fold(0.0, f64::max);
    // Far enough out that every count's chance beyond is negligible.
    let kmax = (top + 12.0 * top.sqrt() + 20.0).ceil() as usize;
    // pmf[u][k] and cdf[u][k] = P(count of u <= k), from logs so a large
    // mean does not underflow e^(-λ).
    let mut pmf = vec![vec![0.0; kmax + 1]; means.len()];
    let mut cdf = vec![vec![0.0; kmax + 1]; means.len()];
    for (u, &(_, l)) in means.iter().enumerate() {
        let mut log_p = -l;
        let mut acc = 0.0;
        for k in 0..=kmax {
            if k > 0 {
                log_p += l.ln() - (k as f64).ln();
            }
            pmf[u][k] = log_p.exp();
            acc += pmf[u][k];
            cdf[u][k] = acc.min(1.0);
        }
    }
    let m = means.len();
    let mut out = vec![0.0; m];
    let mut right = vec![1.0; m + 1];
    for k in 1..=kmax {
        // right[v] = product over u > v of P(count of u <= k).
        right[m] = 1.0;
        for u in (0..m).rev() {
            right[u] = right[u + 1] * cdf[u][k];
        }
        // Lower numbers must have fewer than k: walked up from the left.
        let mut left = 1.0;
        for v in 0..m {
            out[v] += pmf[v][k] * left * right[v + 1];
            left *= cdf[v][k - 1];
        }
    }
    let sum: f64 = out.iter().sum();
    means.iter().zip(out).map(|(&(v, _), p)| (v, if sum > 0.0 { p / sum } else { 0.0 })).collect()
}

/// What the old way did, for checking the formula in tests: play `rounds`
/// rounds of `players` drawing from `picks`, count who won and what was
/// most picked.
#[cfg(test)]
fn played_out(picks: &[i64], players: usize, rounds: u32) -> (std::collections::HashMap<Option<i64>, f64>, std::collections::HashMap<i64, f64>) {
    use rand::{Rng, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let mut values: Vec<i64> = picks.to_vec();
    values.sort_unstable();
    values.dedup();
    let (mut wins, mut most) = (std::collections::HashMap::new(), std::collections::HashMap::new());
    for _ in 0..rounds {
        let mut counts = vec![0u32; values.len()];
        for _ in 0..players {
            let p = picks[rng.gen_range(0..picks.len())];
            counts[values.binary_search(&p).unwrap()] += 1;
        }
        let w = counts.iter().position(|&c| c == 1).map(|i| values[i]);
        *wins.entry(w).or_insert(0.0) += 1.0 / f64::from(rounds);
        let top = *counts.iter().max().unwrap();
        let i = counts.iter().position(|&c| c == top).unwrap();
        *most.entry(values[i]).or_insert(0.0) += 1.0 / f64::from(rounds);
    }
    (wins, most)
}

/// The day's starting chances, from last month's global picks; if there were
/// few, the 42 round's picks fill in. With no picks at all, the smooth
/// curve alone.
pub fn base(global: &History, vienna: &History) -> Base {
    let mut picks = global.picks.clone();
    if picks.len() < MIN_PICKS {
        picks.extend(&vienna.picks);
    }
    if picks.is_empty() {
        return Base { has_data: false, winners: Vec::new(), nobody: 0.0, modes: Vec::new(), picks: 0, players: DEFAULT_PLAYERS };
    }
    let players = global.players().or(vienna.players()).unwrap_or(DEFAULT_PLAYERS).clamp(2, MAX_PLAYERS);
    analyse(&picks, players)
}

/// The chance of `market` now: the starting chance, moved by the points
/// already on its book (winner bets, or most-picked bets). Each bet backs its
/// own range; where that overlaps `market`, it counts for `market` in
/// proportion to the overlap's share of the bet's own starting chance. Never
/// below the starting chance: bets shorten what they back and lengthen
/// nothing.
pub fn chance(base: &Base, book: &[(Market, i64)], market: Market) -> f64 {
    let total: f64 = book.iter().filter(|(m, _)| m.is_most() == market.is_most()).map(|(_, s)| *s as f64).sum();
    let backed: f64 = book
        .iter()
        .filter_map(|(bet, stake)| {
            let both = market.overlap(*bet)?;
            let own = base.chance(*bet);
            (own > 0.0).then(|| *stake as f64 * base.chance(both) / own)
        })
        .sum();
    let start = base.chance(market);
    ((LIQUIDITY * start + backed) / (LIQUIDITY + total)).max(start).clamp(P_MIN, P_MAX)
}

/// Decimal odds offered for chance `p`, in hundredths: fair odds less the
/// tax, rounded down to the nearest 0.05, at most 10000.00. None when that is
/// under 1.05: too safe to bet on.
pub fn offered_odds_x100(p: f64) -> Option<i64> {
    let fair = (100 - MARGIN_PCT) as f64 / p; // in hundredths
    let rounded = ((fair / 5.0).floor() as i64 * 5).min(ODDS_MAX);
    (rounded >= ODDS_MIN).then_some(rounded)
}

/// What a winning bet pays back, stake included, rounded down; its profit
/// at most `max_profit` if it has a cap (bets from before caps have none).
pub fn payout(stake: i64, odds_x100: i64, max_profit: Option<i64>) -> i64 {
    let full = (i128::from(stake) * i128::from(odds_x100) / 100) as i64;
    match max_profit {
        Some(cap) => full.min(stake + cap),
        None => full,
    }
}

/// The biggest stake worth placing at `odds_x100` under a profit cap: past
/// it, a win would be capped and only the loss would grow. At least 1.
pub fn max_stake(odds_x100: i64, max_profit: i64) -> i64 {
    if odds_x100 <= 100 {
        return 1;
    }
    (i128::from(max_profit) * 100 / i128::from(odds_x100 - 100)).clamp(1, i128::from(i64::MAX)) as i64
}

/// "×2.35".
pub fn odds_label(odds_x100: i64) -> String {
    format!("×{}.{:02}", odds_x100 / 100, odds_x100 % 100)
}

/// The house's cut as a page says it: "8%".
pub fn tax_label() -> String {
    format!("{MARGIN_PCT}%")
}

/// One market as offered right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub market: Market,
    /// The chance in hundredths of a percent, at least 1 (0.01%).
    pub chance_bp: i64,
    /// None: too safe to offer.
    pub odds_x100: Option<i64>,
}

pub fn quote(base: &Base, book: &[(Market, i64)], market: Market) -> Quote {
    let p = chance(base, book, market);
    Quote { market, chance_bp: ((p * 10_000.0).round() as i64).max(1), odds_x100: offered_odds_x100(p) }
}

/// A chance, in hundredths of a percent, as the page shows it: two decimals
/// under 1% ("0.04%"), one under 10% ("7.5%"), whole percent above.
pub fn chance_label(bp: i64) -> String {
    if bp < 100 {
        format!("0.{bp:02}%")
    } else if bp < 1000 {
        let tenths = (bp + 5) / 10;
        format!("{}.{}%", tenths / 10, tenths % 10)
    } else {
        format!("{}%", (bp + 50) / 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(lo: i64, hi: Option<i64>) -> Market {
        Market::range(lo, hi).unwrap()
    }

    fn week(picks: &[i64], rounds: usize) -> History {
        History { picks: picks.to_vec(), rounds, plays: picks.len() }
    }

    /// Last week as a plausible crowd: lots of small numbers, a long tail.
    fn crowd() -> Base {
        let mut picks = Vec::new();
        for (v, n) in [(1, 30), (2, 25), (3, 22), (4, 18), (5, 15), (7, 14), (8, 9), (11, 8), (13, 9), (17, 7), (23, 5), (42, 10), (69, 4), (100, 3)] {
            picks.extend(std::iter::repeat_n(v, n));
        }
        base(&week(&picks, 7), &History::default())
    }

    #[test]
    fn every_market_wins_and_loses_where_it_should() {
        for (market, winner, wins) in [
            (r(1, Some(9)), Some(9), true),
            (r(1, Some(9)), Some(10), false),
            (r(5, Some(12)), Some(4), false),
            (r(5, Some(12)), Some(5), true),
            (r(5, Some(12)), Some(12), true),
            (r(5, Some(12)), Some(13), false),
            (r(7, Some(7)), Some(7), true),
            (r(7, Some(7)), Some(8), false),
            (r(10, None), Some(10), true),
            (r(10, None), Some(1_000_000), true),
            (r(10, None), Some(9), false),
            (r(1, None), None, false),
            (Market::Nobody, None, true),
            (Market::Nobody, Some(1), false),
        ] {
            assert_eq!(outcome(market, Outcome { winner, most: Some(3) }), wins, "{market:?} {winner:?}");
        }
    }

    #[test]
    fn markets_round_trip_and_nothing_malformed_parses() {
        for m in [r(1, Some(1)), r(5, Some(12)), r(10, None), r(1, None), Market::Nobody] {
            assert_eq!(Market::parse(&m.key()), Some(m), "{}", m.key());
        }
        assert_eq!(r(5, Some(12)).key(), "range:5:12");
        assert_eq!(r(10, None).key(), "range:10:");
        for bad in ["range:0:5", "range:6:5", "range:5", "range:-1:3", "range:+5:6", "range:5:x", "range::", "range:1:2000000000", "none ", "", "under:1", "x"] {
            assert_eq!(Market::parse(bad), None, "{bad}");
        }
        // The first bets' keys still read as what they meant.
        assert_eq!(Market::parse("under:10"), Some(r(1, Some(9))));
        assert_eq!(Market::parse("over:10"), Some(r(10, None)));
    }

    #[test]
    fn the_two_boxes_make_a_range() {
        assert_eq!(Market::from_inputs("5", "12"), Some(r(5, Some(12))));
        assert_eq!(Market::from_inputs(" 10 ", ""), Some(r(10, None)));
        assert_eq!(Market::from_inputs("7", "7"), Some(r(7, Some(7))));
        for (lo, hi) in [("12", "5"), ("0", "3"), ("", "3"), ("a", ""), ("3", "b"), ("-2", "4")] {
            assert_eq!(Market::from_inputs(lo, hi), None, "{lo}..{hi}");
        }
        assert_eq!(r(5, None).inputs(), ("5".to_string(), String::new()));
    }

    #[test]
    fn labels_read_naturally() {
        assert_eq!(r(5, Some(12)).label(), "winner 5 to 12");
        assert_eq!(r(7, Some(7)).label(), "winner exactly 7");
        assert_eq!(r(10, None).label(), "winner 10 or more");
        assert_eq!(r(1, None).label(), "any winner");
        assert_eq!(Market::Nobody.label(), "nobody wins");
    }

    #[test]
    fn overlaps_are_what_both_win_on() {
        assert_eq!(r(1, Some(9)).overlap(r(5, None)), Some(r(5, Some(9))));
        assert_eq!(r(1, Some(4)).overlap(r(5, None)), None);
        assert_eq!(r(3, None).overlap(r(8, None)), Some(r(8, None)));
        assert_eq!(Market::Nobody.overlap(r(1, None)), None);
        assert_eq!(Market::Nobody.overlap(Market::Nobody), Some(Market::Nobody));
    }

    #[test]
    fn the_board_covers_every_outcome_once() {
        let b = board();
        for w in [Some(1), Some(2), Some(5), Some(9), Some(10), Some(49), Some(50), Some(9999), None] {
            assert_eq!(b.iter().filter(|m| outcome(**m, Outcome { winner: w, most: None })).count(), 1, "{w:?}");
        }
        let m = board_most();
        for most in [1, 2, 5, 9, 10, 49, 50, 9999] {
            assert_eq!(m.iter().filter(|x| outcome(**x, Outcome { winner: None, most: Some(most) })).count(), 1, "{most}");
        }
        for base in [crowd(), base(&History::default(), &History::default())] {
            let sum: f64 = b.iter().map(|m| base.chance(*m)).sum();
            assert!((sum - 1.0).abs() < 1e-9, "{sum}");
            let sum: f64 = m.iter().map(|m| base.chance(*m)).sum();
            assert!((sum - 1.0).abs() < 1e-9, "most picked: {sum}");
        }
    }

    #[test]
    fn the_most_picked_number_is_its_own_market() {
        let most = |lo, hi| r(lo, hi).most();
        assert_eq!(most(1, Some(3)).key(), "most:1:3");
        assert_eq!(Market::parse("most:7:"), Some(most(7, None)));
        assert_eq!(Market::parse("most:9:3"), None);
        assert_eq!(most(42, Some(42)).label(), "most picked exactly 42");
        assert_eq!(most(4, Some(9)).label(), "most picked 4 to 9");
        assert_eq!(most(10, None).label(), "most picked 10 or more");
        let o = Outcome { winner: Some(7), most: Some(2) };
        assert!(outcome(most(1, Some(3)), o) && !outcome(most(3, None), o));
        assert!(outcome(r(5, Some(9)), o) && !outcome(r(1, Some(3)), o), "winner bets go by the winner");
        // Its own book: winner bets do not move it.
        let b = crowd();
        let start = chance(&b, &[], most(1, Some(1)));
        assert_eq!(chance(&b, &[(r(1, Some(1)), 500)], most(1, Some(1))), start);
        assert!(chance(&b, &[(most(1, Some(1)), 500)], most(1, Some(1))) > start);
    }

    #[test]
    fn the_formula_by_hand() {
        // Two players picking 1 or 2: each number's count has mean 1, and
        // exactly one is e^-1 = 0.368. 1 wins whenever it has exactly one;
        // 2 when it has exactly one and 1 does not.
        let b = analyse(&[1, 2], 2);
        let one = (-1.0f64).exp();
        assert!((b.winners[0].1 - one).abs() < 1e-12);
        assert!((b.winners[1].1 - (1.0 - one) * one).abs() < 1e-12);
        assert!((b.nobody - (1.0 - one) * (1.0 - one)).abs() < 1e-12);
        let total: f64 = b.winners.iter().map(|w| w.1).sum::<f64>() + b.nobody;
        assert!((total - 1.0).abs() < 1e-12, "every outcome once");
        // Everyone on one number: that number is the most picked, always,
        // and it almost never wins.
        let b = analyse(&[4], 5);
        assert!((b.modes[0].1 - 1.0).abs() < 1e-9);
        assert!(b.nobody > 0.95);
    }

    #[test]
    fn the_formula_agrees_with_playing_it_out() {
        // A realistic crowd of 50 a round, checked against 20 000 rounds
        // actually played: the formula matches within a couple of points.
        let mut picks = Vec::new();
        for (v, n) in [(1, 30), (2, 25), (3, 22), (4, 18), (5, 15), (7, 14), (8, 9), (11, 8), (13, 9), (17, 7), (23, 5), (42, 10), (69, 4), (100, 3)] {
            picks.extend(std::iter::repeat_n(v, n));
        }
        let b = analyse(&picks, 50);
        let (wins, most) = played_out(&picks, 50, 20_000);
        for (lo, hi) in [(1, 3), (4, 9), (10, 19), (20, 1000)] {
            let formula: f64 = b.winners.iter().filter(|w| (lo..=hi).contains(&w.0)).map(|w| w.1).sum();
            let played: f64 = wins.iter().filter(|(w, _)| w.is_some_and(|w| (lo..=hi).contains(&w))).map(|(_, p)| p).sum();
            assert!((formula - played).abs() < 0.03, "winner {lo}-{hi}: formula {formula:.3}, played {played:.3}");
        }
        let played_nobody = wins.get(&None).copied().unwrap_or(0.0);
        assert!((b.nobody - played_nobody).abs() < 0.03, "nobody: {} vs {played_nobody}", b.nobody);
        for v in [1, 2, 3] {
            let formula = b.modes.iter().find(|m| m.0 == v).map_or(0.0, |m| m.1);
            let played = most.get(&v).copied().unwrap_or(0.0);
            assert!((formula - played).abs() < 0.05, "most picked {v}: formula {formula:.3}, played {played:.3}");
        }
        let all: f64 = b.modes.iter().map(|m| m.1).sum();
        assert!((all - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_same_picks_give_the_same_chances_every_time() {
        assert_eq!(crowd(), crowd());
    }

    #[test]
    fn near_certain_ranges_are_not_offered() {
        let b = crowd();
        let any = quote(&b, &[], r(1, None));
        assert!(any.chance_bp >= 9000, "{any:?}");
        assert_eq!(any.odds_x100, None, "the tax makes it pay less than it costs");
        let narrow = quote(&b, &[], r(6, Some(9)));
        assert!(narrow.odds_x100.is_some());
    }

    #[test]
    fn bets_shorten_what_they_back_and_lengthen_nothing() {
        let b = crowd();
        let target = r(5, Some(12));
        let before = chance(&b, &[], target);
        let backed = chance(&b, &[(r(5, Some(12)), 100)], target);
        let partly = chance(&b, &[(r(1, Some(8)), 100)], target);
        let against = chance(&b, &[(r(50, None), 100)], target);
        assert!(backed > partly && partly > before, "{backed} {partly} {before}");
        assert_eq!(against, before, "a bet elsewhere never makes this pay more");
        // A whale on one side cannot open up a false price on the other.
        let whale = chance(&b, &[(r(1, Some(4)), 1_000_000)], target);
        assert_eq!(whale, before);
        // And the odds follow.
        let o = |p: f64| offered_odds_x100(p).unwrap();
        assert!(o(backed) < o(before) && o(before) == o(against));
        // A lot of money moves it a lot, never past certain.
        let flood = vec![(target, 100); 500];
        assert!(chance(&b, &flood, target) > 0.9);
        assert!(chance(&b, &flood, target) <= P_MAX);
    }

    #[test]
    fn odds_carry_the_tax_round_down_and_stay_in_bounds() {
        assert_eq!(offered_odds_x100(0.665), Some(135)); // 0.92 / 0.665 = 1.383
        assert_eq!(offered_odds_x100(0.5), Some(180));
        assert_eq!(offered_odds_x100(0.87), Some(105));
        assert_eq!(offered_odds_x100(0.9), None, "1.02 is too safe");
        assert_eq!(offered_odds_x100(0.001), Some(92_000), "a 0.1% shot pays ×920");
        assert_eq!(offered_odds_x100(0.0001), Some(920_000), "the 0.01% floor pays ×9200");
        assert_eq!(offered_odds_x100(0.005), Some(18_400), "0.5%: ×184");
        assert_eq!(offered_odds_x100(0.000_01), Some(1_000_000), "never above 10000");
        assert_eq!(chance_label(4), "0.04%");
        assert_eq!(chance_label(37), "0.37%");
        assert_eq!(chance_label(100), "1.0%");
        assert_eq!(chance_label(752), "7.5%");
        assert_eq!(chance_label(1000), "10%");
        assert_eq!(chance_label(2740), "27%");
        for p in [0.02, 0.1, 0.33, 0.5, 0.8] {
            let o = offered_odds_x100(p).unwrap();
            assert_eq!(o % 5, 0);
            assert!((o as f64) < 100.0 / p, "always worse than fair");
        }
    }

    #[test]
    fn a_win_pays_stake_times_odds_rounded_down() {
        assert_eq!(payout(50, 135, None), 67);
        assert_eq!(payout(100, 5000, None), 5000);
        assert_eq!(payout(1, 105, None), 1);
        // Capped: the profit, not the stake, is what the cap limits.
        assert_eq!(payout(500, 40_740, Some(3000)), 3500);
        assert_eq!(payout(50, 135, Some(3000)), 67, "under the cap: as before");
        // The biggest stake that still wins more: right up to the cap.
        assert_eq!(max_stake(40_740, 2500), 6, "×407.40: 6 wins 2438, 7 would pass 2500");
        assert_eq!(max_stake(200, 2500), 2500, "×2: profit equals stake");
        assert_eq!(max_stake(105, 2500), 50_000);
        for (o, cap) in [(40_740, 2500), (135, 3000), (918, 700)] {
            let s = max_stake(o, cap);
            assert!(payout(s, o, None) - s <= cap && payout(s + 1, o, None) - (s + 1) > cap - 1, "{o} {cap} {s}");
        }
        assert_eq!(odds_label(235), "×2.35");
        assert_eq!(odds_label(1305), "×13.05");
    }

    #[test]
    fn a_month_counts_its_rounds_and_its_last_week_twice() {
        use crate::stats::DayTally;
        let t = |d: &str, v, c| DayTally { round_date: d.into(), value: v, count: c };
        let rows = [t("2026-09-30", 1, 2), t("2026-09-30", 5, 1), t("2026-10-15", 3, 4)];
        let h = History::from_tallies(&rows, "2026-10-14");
        assert_eq!((h.picks.len(), h.rounds, h.plays), (3 + 8, 2, 7), "the recent day drawn from twice");
        assert_eq!(h.players(), Some(4), "players a day from real guesses, not the doubled draw");
    }

    #[test]
    fn the_biggest_win_grows_with_the_game() {
        let small = base(&week(&[1, 2, 3, 4, 5, 6], 1), &History::default());
        let big = base(&week(&vec![2; 300], 3), &History::default());
        assert_eq!(small.max_profit(), 6 * PROFIT_PER_PLAYER);
        assert_eq!(big.max_profit(), 100 * PROFIT_PER_PLAYER);
        assert_eq!(base(&History::default(), &History::default()).max_profit(), 10 * PROFIT_PER_PLAYER, "no history: a round of ten");
    }
}
