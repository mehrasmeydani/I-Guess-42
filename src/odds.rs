//! Bets on the global round and their odds (SPEC.md section 9): pure
//! functions, so they are easy to test. The only floating point in the
//! codebase lives in `probability`; everything stored or shown as points is
//! an integer.
//!
//! A market is a statement about the round's winning number W - the lowest
//! number exactly one player picked - or about there being none. Its chance
//! is estimated from past global rounds, recent ones counting more, and
//! pulled towards the 42 round's history while the global round is young.
//! The odds are fair odds less a house margin, so points drain a little and
//! the leaderboard keeps meaning something.

/// The lines under/over bets are offered on.
pub const LINES: [i64; 4] = [5, 10, 20, 50];
/// The house keeps this much of fair odds (8%).
const MARGIN_PCT: i64 = 8;
/// Recent rounds count more: a round 14 days back counts half as much.
const HALF_LIFE_DAYS: f64 = 14.0;
/// How many past rounds the estimate looks at.
pub const HISTORY: usize = 90;
/// How many days of evidence the prior is worth.
const PRIOR_WEIGHT: f64 = 4.0;
/// No market is ever certain.
const P_MIN: f64 = 0.02;
const P_MAX: f64 = 0.98;
/// Odds are offered between these, in hundredths.
const ODDS_MIN: i64 = 105;
const ODDS_MAX: i64 = 5000;

/// The most one bet may stake, and the most an account may stake on bets
/// in one round: what a manipulated round can pay out is bounded.
pub const MAX_BET: i64 = 100;
pub const MAX_BETS_PER_ROUND: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Market {
    /// W exists and W < N.
    Under(i64),
    /// W exists and W >= N.
    Over(i64),
    /// Nobody wins: every number was picked more than once.
    Nobody,
}

impl Market {
    /// Every market offered, in the order a page lists them.
    pub fn all() -> Vec<Market> {
        let mut v = Vec::new();
        for n in LINES {
            v.push(Market::Under(n));
            v.push(Market::Over(n));
        }
        v.push(Market::Nobody);
        v
    }

    /// As stored: "under:10", "over:10", "none".
    pub fn key(self) -> String {
        match self {
            Market::Under(n) => format!("under:{n}"),
            Market::Over(n) => format!("over:{n}"),
            Market::Nobody => "none".to_string(),
        }
    }

    /// Only the markets in `all()` parse, so nothing else can be bet on.
    pub fn parse(key: &str) -> Option<Market> {
        Market::all().into_iter().find(|m| m.key() == key)
    }

    /// As a page shows it.
    pub fn label(self) -> String {
        match self {
            Market::Under(n) => format!("winner under {n}"),
            Market::Over(n) => format!("winner {n} or more"),
            Market::Nobody => "nobody wins".to_string(),
        }
    }
}

/// Whether `market` won, given the round's winning number. Under and over
/// lose when nobody wins, as at any bookmaker; that is what gives "nobody
/// wins" its meaning.
pub fn outcome(market: Market, winner: Option<i64>) -> bool {
    match (market, winner) {
        (Market::Under(n), Some(w)) => w < n,
        (Market::Over(n), Some(w)) => w >= n,
        (Market::Nobody, None) => true,
        _ => false,
    }
}

/// The weighted share of past rounds where `market` won, and the total
/// weight behind it. `history` is newest first.
fn evidence(market: Market, history: &[Option<i64>]) -> (f64, f64) {
    history.iter().take(HISTORY).enumerate().fold((0.0, 0.0), |(s, t), (d, w)| {
        let weight = 0.5f64.powf(d as f64 / HALF_LIFE_DAYS);
        let hit = if outcome(market, *w) { weight } else { 0.0 };
        (s + hit, t + weight)
    })
}

/// The estimate a market's odds are made from, before smoothing by the
/// global round's own history: the 42 round's rate, or a coin flip.
pub fn prior(market: Market, vienna_history: &[Option<i64>]) -> f64 {
    let (s, t) = evidence(market, vienna_history);
    if t > 0.0 { s / t } else { 0.5 }
}

/// The chance `market` wins: the global round's weighted rate, smoothed
/// towards `prior` as if the prior were `PRIOR_WEIGHT` days of evidence, and
/// kept off 0 and 1. `history` is newest first.
pub fn probability(market: Market, history: &[Option<i64>], prior: f64) -> f64 {
    let (s, t) = evidence(market, history);
    ((s + PRIOR_WEIGHT * prior) / (t + PRIOR_WEIGHT)).clamp(P_MIN, P_MAX)
}

/// Decimal odds offered for chance `p`, in hundredths: fair odds less the
/// margin, rounded down to the nearest 0.05, between 1.05 and 50.00.
pub fn offered_odds_x100(p: f64) -> i64 {
    let fair = (100 - MARGIN_PCT) as f64 / p; // in hundredths
    let rounded = (fair / 5.0).floor() as i64 * 5;
    rounded.clamp(ODDS_MIN, ODDS_MAX)
}

/// What a winning bet pays back, stake included, rounded down.
pub fn payout(stake: i64, odds_x100: i64) -> i64 {
    (i128::from(stake) * i128::from(odds_x100) / 100) as i64
}

/// "×2.35".
pub fn odds_label(odds_x100: i64) -> String {
    format!("×{}.{:02}", odds_x100 / 100, odds_x100 % 100)
}

/// One market as the bets page offers it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub market: Market,
    /// The estimated chance, in whole percent, shown beside the odds.
    pub chance_pct: i64,
    pub odds_x100: i64,
}

/// Today's offers, from the winners of past global rounds and of past 42
/// rounds, each newest first.
pub fn offers(global_history: &[Option<i64>], vienna_history: &[Option<i64>]) -> Vec<Offer> {
    Market::all()
        .into_iter()
        .map(|market| {
            let p = probability(market, global_history, prior(market, vienna_history));
            Offer {
                market,
                chance_pct: (p * 100.0).round() as i64,
                odds_x100: offered_odds_x100(p),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_market_wins_and_loses_where_it_should() {
        for (market, winner, wins) in [
            (Market::Under(10), Some(9), true),
            (Market::Under(10), Some(10), false),
            (Market::Under(10), Some(11), false),
            (Market::Under(10), None, false),
            (Market::Over(10), Some(9), false),
            (Market::Over(10), Some(10), true),
            (Market::Over(10), Some(11), true),
            (Market::Over(10), None, false),
            (Market::Nobody, None, true),
            (Market::Nobody, Some(1), false),
        ] {
            assert_eq!(outcome(market, winner), wins, "{market:?} {winner:?}");
        }
    }

    #[test]
    fn markets_round_trip_and_nothing_else_parses() {
        for m in Market::all() {
            assert_eq!(Market::parse(&m.key()), Some(m));
        }
        assert_eq!(Market::all().len(), 9);
        for bad in ["under:7", "over:", "none ", "UNDER:10", "", "under:10;drop"] {
            assert_eq!(Market::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn with_no_history_the_chance_is_the_prior() {
        assert!((probability(Market::Under(10), &[], 0.7) - 0.7).abs() < 1e-9);
        assert!((prior(Market::Under(10), &[]) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_long_steady_history_outweighs_the_prior() {
        // Under 10 won three days in four, for 90 days.
        let h: Vec<Option<i64>> = (0..90).map(|i| Some(if i % 4 == 0 { 15 } else { 3 })).collect();
        // 90 days at a 14-day half-life weigh about 20 days of evidence,
        // against the prior's 4: (0.75 * 20 + 0.1 * 4) / 24 is about 0.64.
        let p = probability(Market::Under(10), &h, 0.1);
        assert!((0.6..0.7).contains(&p), "{p}");
        assert!(p - 0.1 > 0.75 - p, "far nearer the history than the prior");
    }

    #[test]
    fn recent_days_count_more_than_old_ones() {
        // Under 10 won the last 10 days, lost the 30 before: newest first.
        let mut h: Vec<Option<i64>> = vec![Some(3); 10];
        h.extend(vec![Some(30); 30]);
        let plain_rate = 10.0 / 40.0;
        let p = probability(Market::Under(10), &h, 0.5);
        assert!(p > plain_rate + 0.15, "{p}");
    }

    #[test]
    fn nothing_is_ever_certain() {
        let always: Vec<Option<i64>> = vec![Some(3); 90];
        assert!(probability(Market::Under(10), &always, 1.0) <= 0.98);
        assert!(probability(Market::Over(10), &always, 0.0) >= 0.02);
    }

    #[test]
    fn odds_carry_the_margin_round_down_and_stay_in_bounds() {
        // The spec's worked example: p = 0.665 -> 0.92 / 0.665 = 1.383 -> 1.35.
        assert_eq!(offered_odds_x100(0.665), 135);
        assert_eq!(offered_odds_x100(0.5), 180); // 1.84 -> 1.80
        assert_eq!(offered_odds_x100(0.98), 105, "never below 1.05");
        assert_eq!(offered_odds_x100(0.001), 5000, "never above 50");
        for p in [0.02, 0.1, 0.33, 0.5, 0.9] {
            assert_eq!(offered_odds_x100(p) % 5, 0);
            // Always worse than fair: the house keeps its margin.
            assert!((offered_odds_x100(p) as f64) < 100.0 / p);
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
    fn todays_offers_cover_every_market() {
        let global: Vec<Option<i64>> = vec![Some(7), Some(12), None, Some(3)];
        let vienna: Vec<Option<i64>> = vec![Some(9); 20];
        let o = offers(&global, &vienna);
        assert_eq!(o.len(), 9);
        let under10 = o.iter().find(|x| x.market == Market::Under(10)).unwrap();
        assert!(under10.chance_pct > 50, "the 42 round's 9s push under 10 up");
        assert!(o.iter().all(|x| (2..=98).contains(&x.chance_pct)));
    }
}
