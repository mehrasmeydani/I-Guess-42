//! Random history for test instances: months of closed rounds, each with a
//! crowd of invented `bot-###` players, so the results, day and trends pages
//! can be tried (and timed) against realistic amounts of data.
//!
//! Only reachable from /admin, which does not exist unless ADMIN_LOGINS is
//! set. It never touches the open round, and every row it writes belongs to a
//! bot, so removing the bots removes all of it.

use chrono::{Datelike, NaiveDate, Weekday};
use rand::seq::index::sample;
use rand::Rng;

/// Size of the bot pool. Each day draws its players from it.
pub const BOTS: usize = 300;

/// Numbers people pick for the joke rather than to win.
const MEMES: [i64; 4] = [42, 69, 420, 1337];
/// Nobody who understands the game goes past this; the demo never does.
pub const HIGHEST: i64 = 500;

/// One invented guess: which day, which bot (an index into the pool), what number.
pub struct DemoGuess {
    pub round_date: String,
    pub bot: usize,
    pub value: i64,
}

/// Guesses for the `days` closed rounds ending on `last_closed`, oldest first.
///
/// Turnout grows slowly over the months, dips at weekends, and the crowd
/// drifts: early on most people pile onto 1-5, later more of them spread out
/// higher, so the trends page has something to find.
pub fn generate(rng: &mut impl Rng, last_closed: NaiveDate, days: i64) -> Vec<DemoGuess> {
    let mut out = Vec::new();
    for back in (0..days).rev() {
        let date = last_closed - chrono::Duration::days(back);
        // 0.0 on the oldest day, 1.0 on the newest.
        let age = if days > 1 {
            (days - 1 - back) as f64 / (days - 1) as f64
        } else {
            1.0
        };
        let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
        let base = 90.0 + 140.0 * age;
        let players = (base * if weekend { 0.6 } else { 1.0 } * rng.gen_range(0.8..1.2)) as usize;
        let players = players.clamp(10, BOTS);

        let key = date.format("%Y-%m-%d").to_string();
        for bot in sample(rng, BOTS, players).into_iter() {
            out.push(DemoGuess {
                round_date: key.clone(),
                bot,
                value: pick(rng, age),
            });
        }
    }
    out
}

/// One bot's number. `age` (0..1) shifts the crowd upwards over time.
///
/// The game rewards going low, so almost everyone does. The shares below add
/// up to exactly 1 at any age:
///
/// | share         | old day | new day | picks                          |
/// |---------------|---------|---------|--------------------------------|
/// | the obvious   | 45%     | 25%     | 1 to 5                         |
/// | a bit clever  | 38%     | 53%     | mostly under 10, a tail to ~40 |
/// | overthinking  | 14%     | 17%     | 10 to 40                       |
/// | the joke      | 2%      | 2%      | 42, 69, 420, 1337              |
/// | the outlier   | 1%      | 3%      | 60 to 500                      |
fn pick(rng: &mut impl Rng, age: f64) -> i64 {
    let roll: f64 = rng.gen();
    let obvious = 0.45 - 0.20 * age;
    let clever = obvious + 0.38 + 0.15 * age;
    let overthinking = clever + 0.14 + 0.03 * age;
    let joke = overthinking + 0.02;
    if roll < obvious {
        rng.gen_range(1..=5)
    } else if roll < clever {
        // Exponential with a mean that grows from 3 to 10.
        let mean = 3.0 + 7.0 * age;
        let u: f64 = rng.gen_range(f64::EPSILON..1.0);
        (1 + (-u.ln() * mean) as i64).min(HIGHEST)
    } else if roll < overthinking {
        rng.gen_range(10..=40)
    } else if roll < joke {
        MEMES[rng.gen_range(0..MEMES.len())]
    } else {
        rng.gen_range(60..=HIGHEST)
    }
}

pub fn bot_login(index: usize) -> String {
    format!("bot-{:03}", index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::collections::HashSet;

    #[test]
    fn covers_every_day_and_never_repeats_a_player_within_one() {
        let mut rng = StdRng::seed_from_u64(42);
        let last = NaiveDate::from_ymd_opt(2026, 9, 22).unwrap();
        let guesses = generate(&mut rng, last, 90);

        let days: HashSet<&str> = guesses.iter().map(|g| g.round_date.as_str()).collect();
        assert_eq!(days.len(), 90);
        assert!(days.contains("2026-09-22"));
        assert!(days.contains("2026-06-25"));
        assert!(!days.contains("2026-09-23"), "must stop at the last closed round");

        let pairs: HashSet<(&str, usize)> =
            guesses.iter().map(|g| (g.round_date.as_str(), g.bot)).collect();
        assert_eq!(pairs.len(), guesses.len());

        assert!(guesses.iter().all(|g| g.value >= 1 && g.bot < BOTS));
        assert!(guesses.iter().all(|g| g.value <= 1337), "nobody goes near 10k");
        let low = guesses.iter().filter(|g| g.value <= 10).count();
        assert!(low * 10 > guesses.len() * 6, "most of the crowd stays at 10 or under");
        assert!(guesses.len() > 90 * 50, "hundreds a day, not a handful");
    }

    #[test]
    fn bot_logins_are_padded() {
        assert_eq!(bot_login(0), "bot-001");
        assert_eq!(bot_login(299), "bot-300");
    }
}
