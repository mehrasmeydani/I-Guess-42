//! Bets on the global round and their odds (SPEC.md section 9): pure
//! functions, so they are easy to test. Everything stored or shown as points
//! is an integer; floating point stays in here.
//!
//! A bet is on the round's winning number W - the lowest number exactly one
//! player picked - landing in a range the player chooses, or on there being
//! no winner at all. Odds come from two things:
//!
//! - Last week's picks, replayed: `SIMS` made-up rounds, each with last
//!   week's usual number of players drawing from what people actually picked.
//!   How often W lands in a range is its starting chance.
//! - The book: points already bet count as evidence, like money moving a
//!   betting exchange. The more is staked on numbers in a range, the likelier
//!   it looks and the less it pays; ranges nobody backs drift longer.
//!
//! The house keeps `MARGIN_PCT` of fair odds - the tax - so every bet loses a
//! little on average and the near-certain ones ("1 or more") pay less than
//! they cost and are not offered at all.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// The house keeps this much of fair odds (8%).
const MARGIN_PCT: i64 = 8;
/// How many days of picks the simulation replays.
pub const WEEK_DAYS: i64 = 7;
/// Made-up rounds per day's odds.
const SIMS: u32 = 4000;
/// Below this many global picks last week, the 42 round's picks fill in.
const MIN_PICKS: usize = 30;
/// Players per simulated round, at most (keeps the work bounded).
const MAX_PLAYERS: usize = 1000;
/// Without a week to go on, rounds are imagined this size.
const DEFAULT_PLAYERS: usize = 10;
/// Share of every chance taken from a smooth curve instead of the
/// simulation, so a range nobody picked last week is unlikely, not impossible.
const SMOOTH: f64 = 0.05;
/// That curve's chance of nobody winning.
const NOBODY_PRIOR: f64 = 0.05;
/// How many points the starting chances are worth against bets: 1000 points
/// staked on a range move it as far as the whole simulation pulls.
const LIQUIDITY: f64 = 1000.0;
/// No bet is ever certain.
const P_MIN: f64 = 0.01;
const P_MAX: f64 = 0.99;
/// Odds are offered between these, in hundredths. Anything that would pay
/// less than ×1.05 is too safe to bet on.
const ODDS_MIN: i64 = 105;
const ODDS_MAX: i64 = 5000;
/// The largest number a range may name.
pub const MAX_LINE: i64 = 1_000_000_000;

/// The most one bet may stake, and the most an account may stake on bets
/// in one round: what a manipulated round can pay out is bounded.
pub const MAX_BET: i64 = 100;
pub const MAX_BETS_PER_ROUND: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Market {
    /// W exists and lo <= W, and W <= hi if there is a hi.
    Range { lo: i64, hi: Option<i64> },
    /// Nobody wins: every number was picked more than once.
    Nobody,
}

impl Market {
    /// A range, if it is one: 1 <= lo <= hi <= `MAX_LINE`.
    pub fn range(lo: i64, hi: Option<i64>) -> Option<Market> {
        let ok = (1..=MAX_LINE).contains(&lo) && hi.is_none_or(|h| (lo..=MAX_LINE).contains(&h));
        ok.then_some(Market::Range { lo, hi })
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

    /// As stored: "range:5:12", "range:10:" (no top), "none".
    pub fn key(self) -> String {
        match self {
            Market::Range { lo, hi } => format!("range:{lo}:{}", hi.map(|h| h.to_string()).unwrap_or_default()),
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
        if let Some(rest) = key.strip_prefix("range:") {
            let (lo, hi) = rest.split_once(':')?;
            let hi = if hi.is_empty() { None } else { Some(num(hi)?) };
            return Market::range(num(lo)?, hi);
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
        }
    }

    /// What the bets page's two boxes hold for it.
    pub fn inputs(self) -> (String, String) {
        match self {
            Market::Range { lo, hi } => (lo.to_string(), hi.map(|h| h.to_string()).unwrap_or_default()),
            Market::Nobody => (String::new(), String::new()),
        }
    }

    /// The outcomes both markets win on, if any.
    fn overlap(self, other: Market) -> Option<Market> {
        match (self, other) {
            (Market::Nobody, Market::Nobody) => Some(Market::Nobody),
            (Market::Range { lo: a, hi: x }, Market::Range { lo: b, hi: y }) => {
                let lo = a.max(b);
                let hi = match (x, y) {
                    (Some(x), Some(y)) => Some(x.min(y)),
                    (h, None) | (None, h) => h,
                };
                hi.is_none_or(|h| lo <= h).then_some(Market::Range { lo, hi })
            }
            _ => None,
        }
    }
}

/// The ranges the bets page lists ready-made. Together with "nobody wins"
/// they cover every outcome once, so their chances add up to 100%.
pub fn board() -> Vec<Market> {
    let mut v: Vec<Market> = [(1, Some(1)), (2, Some(2)), (3, Some(3)), (4, Some(5)), (6, Some(9)), (10, Some(19)), (20, Some(49)), (50, None)]
        .into_iter()
        .map(|(lo, hi)| Market::Range { lo, hi })
        .collect();
    v.push(Market::Nobody);
    v
}

/// Whether `market` won, given the round's winning number. Ranges lose when
/// nobody wins, as at any bookmaker; that is what gives "nobody wins" its
/// meaning.
pub fn outcome(market: Market, winner: Option<i64>) -> bool {
    match (market, winner) {
        (Market::Range { lo, hi }, Some(w)) => lo <= w && hi.is_none_or(|h| w <= h),
        (Market::Nobody, None) => true,
        _ => false,
    }
}

/// A run of past rounds' picks: every guess, and how many rounds they came from.
#[derive(Debug, Clone, Default)]
pub struct Week {
    pub picks: Vec<i64>,
    pub rounds: usize,
}

impl Week {
    /// From per-day tallies, as `db::range_tallies` gives them.
    pub fn from_tallies(rows: &[crate::stats::DayTally]) -> Week {
        let mut picks = Vec::new();
        let mut days: Vec<&str> = Vec::new();
        for r in rows {
            picks.extend(std::iter::repeat_n(r.value, r.count.max(0) as usize));
            if days.last() != Some(&r.round_date.as_str()) {
                days.push(&r.round_date);
            }
        }
        days.dedup();
        Week { picks, rounds: days.len() }
    }

    fn players(&self) -> Option<usize> {
        (self.rounds > 0).then(|| self.picks.len().div_ceil(self.rounds))
    }
}

/// Where a day's chances start: how often each number won in the simulated
/// rounds, before any bets.
#[derive(Debug, Clone, PartialEq)]
pub struct Base {
    pub sims: u32,
    /// (winning number, how many simulated rounds it won), lowest first.
    winners: Vec<(i64, u32)>,
    nobody: u32,
    /// Picks the simulation drew from, and players per simulated round.
    pub picks: usize,
    pub players: usize,
}

impl Base {
    /// The starting chance of `market`: mostly the simulation, a little the
    /// smooth curve; all of the curve if there was nothing to simulate.
    fn chance(&self, market: Market) -> f64 {
        let (smooth, share) = if self.sims == 0 {
            (1.0, 0.0)
        } else {
            let hits = match market {
                Market::Nobody => self.nobody,
                m => self.winners.iter().filter(|(w, _)| outcome(m, Some(*w))).map(|(_, n)| n).sum(),
            };
            (SMOOTH, f64::from(hits) / f64::from(self.sims))
        };
        let curve = match market {
            Market::Nobody => NOBODY_PRIOR,
            Market::Range { lo, hi } => (1.0 - NOBODY_PRIOR) * curve(lo, hi),
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

/// Replays `picks` as `SIMS` rounds of `players` each: every player draws
/// one of the picks at random, and the lowest number drawn once wins.
fn simulate(picks: &[i64], players: usize, seed: u64) -> Base {
    let mut values: Vec<i64> = picks.to_vec();
    values.sort_unstable();
    values.dedup();
    let index: Vec<usize> = picks.iter().map(|p| values.binary_search(p).unwrap_or(0)).collect();
    let mut rng = StdRng::seed_from_u64(seed);
    let mut counts = vec![0u32; values.len()];
    let mut wins = vec![0u32; values.len()];
    let mut nobody = 0;
    for _ in 0..SIMS {
        counts.iter_mut().for_each(|c| *c = 0);
        for _ in 0..players {
            counts[index[rng.gen_range(0..index.len())]] += 1;
        }
        match counts.iter().position(|&c| c == 1) {
            Some(i) => wins[i] += 1,
            None => nobody += 1,
        }
    }
    Base {
        sims: SIMS,
        winners: values.into_iter().zip(wins).filter(|(_, n)| *n > 0).collect(),
        nobody,
        picks: picks.len(),
        players,
    }
}

/// The day's starting chances, from last week's global picks; if there were
/// few, the 42 round's picks fill in. With no picks at all, the smooth
/// curve alone. `seed` makes a day's simulation the same every time it is
/// worked out.
pub fn base(global: &Week, vienna: &Week, seed: u64) -> Base {
    let mut picks = global.picks.clone();
    if picks.len() < MIN_PICKS {
        picks.extend(&vienna.picks);
    }
    if picks.is_empty() {
        return Base { sims: 0, winners: Vec::new(), nobody: 0, picks: 0, players: 0 };
    }
    let players = global.players().or(vienna.players()).unwrap_or(DEFAULT_PLAYERS).clamp(2, MAX_PLAYERS);
    simulate(&picks, players, seed)
}

/// A seed from a round's key, the same on every run.
pub fn seed(round: &str) -> u64 {
    round.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// The chance of `market` now: the starting chance, moved by the points
/// already on the book. Each bet backs its own range; where that overlaps
/// `market`, it counts for `market` in proportion to the overlap's share of
/// the bet's own starting chance.
pub fn chance(base: &Base, book: &[(Market, i64)], market: Market) -> f64 {
    let total: f64 = book.iter().map(|(_, s)| *s as f64).sum();
    let backed: f64 = book
        .iter()
        .filter_map(|(bet, stake)| {
            let both = market.overlap(*bet)?;
            let own = base.chance(*bet);
            (own > 0.0).then(|| *stake as f64 * base.chance(both) / own)
        })
        .sum();
    ((LIQUIDITY * base.chance(market) + backed) / (LIQUIDITY + total)).clamp(P_MIN, P_MAX)
}

/// Decimal odds offered for chance `p`, in hundredths: fair odds less the
/// tax, rounded down to the nearest 0.05, at most 50.00. None when that is
/// under 1.05: too safe to bet on.
pub fn offered_odds_x100(p: f64) -> Option<i64> {
    let fair = (100 - MARGIN_PCT) as f64 / p; // in hundredths
    let rounded = ((fair / 5.0).floor() as i64 * 5).min(ODDS_MAX);
    (rounded >= ODDS_MIN).then_some(rounded)
}

/// What a winning bet pays back, stake included, rounded down.
pub fn payout(stake: i64, odds_x100: i64) -> i64 {
    (i128::from(stake) * i128::from(odds_x100) / 100) as i64
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
    /// The chance, in whole percent, at least 1.
    pub chance_pct: i64,
    /// None: too safe to offer.
    pub odds_x100: Option<i64>,
}

pub fn quote(base: &Base, book: &[(Market, i64)], market: Market) -> Quote {
    let p = chance(base, book, market);
    Quote { market, chance_pct: ((p * 100.0).round() as i64).max(1), odds_x100: offered_odds_x100(p) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(lo: i64, hi: Option<i64>) -> Market {
        Market::range(lo, hi).unwrap()
    }

    fn week(picks: &[i64], rounds: usize) -> Week {
        Week { picks: picks.to_vec(), rounds }
    }

    /// Last week as a plausible crowd: lots of small numbers, a long tail.
    fn crowd() -> Base {
        let mut picks = Vec::new();
        for (v, n) in [(1, 30), (2, 25), (3, 22), (4, 18), (5, 15), (7, 14), (8, 9), (11, 8), (13, 9), (17, 7), (23, 5), (42, 10), (69, 4), (100, 3)] {
            picks.extend(std::iter::repeat_n(v, n));
        }
        base(&week(&picks, 7), &Week::default(), 1)
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
            assert_eq!(outcome(market, winner), wins, "{market:?} {winner:?}");
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
            assert_eq!(b.iter().filter(|m| outcome(**m, w)).count(), 1, "{w:?}");
        }
        for base in [crowd(), base(&Week::default(), &Week::default(), 1)] {
            let sum: f64 = b.iter().map(|m| base.chance(*m)).sum();
            assert!((sum - 1.0).abs() < 1e-9, "{sum}");
        }
    }

    #[test]
    fn the_simulation_finds_the_lowest_unique_number() {
        // Two players, each picking 1 or 2: they match half the time.
        let b = simulate(&[1, 2], 2, 7);
        let p1 = b.chance(r(1, Some(1)));
        let none = b.chance(Market::Nobody);
        assert!((p1 - 0.5).abs() < 0.05, "{p1}");
        assert!((none - 0.5).abs() < 0.05, "{none}");
        assert!(b.chance(r(2, Some(2))) < 0.02, "2 only wins under 1, which never happens alone");
        // Everyone on one number: nobody ever wins.
        let b = simulate(&[4, 4, 4], 5, 7);
        assert!(b.chance(Market::Nobody) > 0.9);
    }

    #[test]
    fn a_day_simulates_the_same_every_time() {
        assert_eq!(crowd(), crowd());
        assert_ne!(seed("2026-10-21"), seed("2026-10-22"));
    }

    #[test]
    fn few_global_picks_borrow_the_42_round_and_none_fall_back_to_the_curve() {
        let global = week(&[3, 3, 5], 1);
        let vienna = week(&vec![2; 40], 4);
        let b = base(&global, &vienna, 1);
        assert_eq!((b.picks, b.players), (43, 3), "picks pooled, players from the global week");
        let b = base(&Week::default(), &vienna, 1);
        assert_eq!(b.players, 10, "players from the 42 week");
        let b = base(&Week::default(), &Week::default(), 1);
        assert_eq!(b.sims, 0);
        assert!(b.chance(r(1, Some(1))) > b.chance(r(2, Some(2))), "low numbers likelier");
    }

    #[test]
    fn near_certain_ranges_are_not_offered() {
        let b = crowd();
        let any = quote(&b, &[], r(1, None));
        assert!(any.chance_pct >= 90, "{any:?}");
        assert_eq!(any.odds_x100, None, "the tax makes it pay less than it costs");
        let narrow = quote(&b, &[], r(6, Some(9)));
        assert!(narrow.odds_x100.is_some());
    }

    #[test]
    fn bets_shorten_their_range_and_lengthen_the_rest() {
        let b = crowd();
        let target = r(5, Some(12));
        let before = chance(&b, &[], target);
        let backed = chance(&b, &[(r(5, Some(12)), 100)], target);
        let partly = chance(&b, &[(r(1, Some(8)), 100)], target);
        let against = chance(&b, &[(r(50, None), 100)], target);
        assert!(backed > partly && partly > before && before > against, "{backed} {partly} {before} {against}");
        // And the odds follow.
        let o = |p: f64| offered_odds_x100(p).unwrap();
        assert!(o(backed) < o(before) && o(before) < o(against));
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
        assert_eq!(offered_odds_x100(0.001), Some(5000), "never above 50");
        for p in [0.02, 0.1, 0.33, 0.5, 0.8] {
            let o = offered_odds_x100(p).unwrap();
            assert_eq!(o % 5, 0);
            assert!((o as f64) < 100.0 / p, "always worse than fair");
        }
    }

    #[test]
    fn a_win_pays_stake_times_odds_rounded_down() {
        assert_eq!(payout(50, 135), 67);
        assert_eq!(payout(100, 5000), 5000);
        assert_eq!(payout(1, 105), 1);
        assert_eq!(odds_label(235), "×2.35");
        assert_eq!(odds_label(1305), "×13.05");
    }

    #[test]
    fn a_week_counts_its_rounds() {
        use crate::stats::DayTally;
        let t = |d: &str, v, c| DayTally { round_date: d.into(), value: v, count: c };
        let w = Week::from_tallies(&[t("2026-10-14", 1, 2), t("2026-10-14", 5, 1), t("2026-10-15", 3, 4)]);
        assert_eq!((w.picks.len(), w.rounds), (7, 2));
        assert_eq!(w.players(), Some(4));
    }
}
