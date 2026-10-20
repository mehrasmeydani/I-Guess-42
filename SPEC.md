# Spec: open to everyone — global pool, logins, points and betting

Status: **Phase 1 built** (on the `global` branch, not deployed); phases 2–4 not started. This is the plan the next releases follow.
Where it disagrees with the code, the code describes what runs today and this
file describes what is meant to run next.

---

## Contents

1. [Goal](#1-goal)
2. [Decisions already made](#2-decisions-already-made)
3. [Open decisions (with defaults)](#3-open-decisions-with-defaults)
4. [Architecture in one picture](#4-architecture-in-one-picture)
5. [Data model](#5-data-model)
6. [Accounts and sign-in](#6-accounts-and-sign-in)
7. [Pools, routes and pages](#7-pools-routes-and-pages)
8. [Points economy (global pool only)](#8-points-economy-global-pool-only)
9. [Betting and odds](#9-betting-and-odds)
10. [Settlement at 12:42](#10-settlement-at-1242)
11. [Abuse and fake accounts](#11-abuse-and-fake-accounts)
12. [Moderation](#12-moderation)
13. [Configuration](#13-configuration)
14. [Privacy and legal](#14-privacy-and-legal)
15. [Cost](#15-cost)
16. [Release plan](#16-release-plan)
17. [Testing](#17-testing)
18. [Risks and known weaknesses](#18-risks-and-known-weaknesses)

---

## 1. Goal

Open i guess low to anyone, without changing the game 42 Vienna students play
today.

- A second, separate **global pool** that anyone can join with **Google,
  GitHub or email + password**.
- 42 students keep their **42 Vienna pool** exactly as it is, and can switch
  to the global pool **in one click** with the same login.
- The global pool has **in-game points**: a stake on every guess, a pot that
  grows with the number of players, and **bets** ("winning number under 10")
  with bookmaker-style odds computed from past rounds.
- Points are **fake, for bragging only**: never bought, never cashed out,
  never exchanged for anything.
- **Running cost stays at €0** until the game outgrows free tiers.

### Non-goals

- No change to how the 42 pool is played, won, shown or paid.
- No real money anywhere, in either direction.
- No mobile app, no push notifications, no daily reminder emails (those would
  cost email quota; see [15](#15-cost)).
- No chat, comments, friends or profiles beyond a username.

---

## 2. Decisions already made

| # | Decision |
|---|---|
| D1 | The 42 pool and its coalition points **stay untouched**. Same rules, same tables (`users`, `guesses`, `payouts`), same payout job, same campus restriction. |
| D2 | **42 accounts get no in-game points in the 42 pool.** They get coalition points from 42, as today. |
| D3 | **In the global pool, 42 students earn in-game points like everyone else.** |
| D4 | Sign-in options for the global pool: **42, Google, GitHub, email + password.** |
| D5 | Email + password is a first-class login with **verification, password reset and rate limits**. |
| D6 | **Everyone plays and earns from their first day.** No probation period, no "play now, prizes later". |
| D7 | Betting includes side bets like **"winning number under 10"**, with **odds computed from previous rounds**, shown like a betting site. |
| D8 | Points are play money: **no purchase, no cash-out, no prizes of value.** |
| D9 | **€0 running cost** to start; paid tiers only when usage demands it. |

---

## 3. Open decisions (with defaults)

Each has a default so building is never blocked; change any of them before
the phase that needs it.

| # | Question | Default | Needed by |
|---|---|---|---|
| O1 | What does `/` show to visitors who are not 42 students? | `/` stays the 42 pool (untouched). Global lives under `/g`. The header switcher and the login page point non-42 visitors to `/g`. | Phase 2 |
| O2 | Global pool deadline | Same **12:42 Europe/Vienna** as the 42 pool; the page also shows it in the visitor's local time. | Phase 2 |
| O3 | Can 42 students from **other campuses** use their 42 login for the global pool? | **Yes** for the global pool; the campus check keeps applying to the 42 pool only. | Phase 2 |
| O4 | Minimum GitHub account age | **30 days** (configurable). | Phase 2 |
| O5 | Daily grant / guess stake / signup bonus | **100 / 10 / 500** points. | Phase 3 |
| O6 | Bet lines | Under/over **5, 10, 20, 50**, plus **"nobody wins"**. | Phase 4 |
| O7 | House margin on odds | **8 %**. | Phase 4 |
| O8 | Who moderates the live global pool? | A new `MODERATORS` list (42 logins or usernames), separate from `ADMIN_LOGINS`. | Phase 2 |
| O9 | Mail service | **Resend** free tier, sending from `iguesslow.com`. | Phase 2 |

---

## 4. Architecture in one picture

Still one process, one SQLite file and no new services. What changes is that
there are now **two kinds of identity**:

```
                     ┌──────────────── accounts ────────────────┐
  sign-in methods    │ id (ours)  username  intra_id?  banned   │
  ─────────────────  └──────┬───────────────────┬───────────────┘
  42 OAuth      ──┐         │                   │ intra_id (only for 42 students)
  GitHub OAuth  ──┼─> identities                ▼
  Google OAuth  ──┤   (provider, subject)    users  ── 42 pool: guesses, payouts
  email+password──┘                          (unchanged, id = intra id)
                            │
                            ▼
                     global pool: global_guesses, ledger, pots, bets
```

- **`accounts`** is the new "who is this person" table, with our own ids.
  Every session points at an account.
- **`users`** stays as it is: one row per 42 student, keyed by intra id. A 42
  student's account carries `intra_id`, which is the link to their `users` row.
- The **42 pool** only ever reads `users` and `guesses`, so its queries do not
  change. The only 42-pool code that changes is the code that *finds out who
  is signed in*.
- The **global pool** has its own tables and never writes to the 42 ones.

Why not a `pool` column on `guesses`: that table's primary key is
`(round_date, user_id)` with `user_id` = intra id. Adding non-42 players would
mean rebuilding the table and rewriting every 42-pool query. Separate tables
keep D1 true by construction.

The stats code (`stats.rs`) works on `Tally` rows and does not care where they
come from. The query functions in `db.rs` that feed it get a `Pool` argument,
which picks the table and id column (`guesses`/`user_id` or
`global_guesses`/`account_id`). Table names come from a Rust `enum`, never from
input.

---

## 5. Data model

Migrations are append-only, numbered after the current `0004`.

### 0005_accounts.sql (Phase 1, built)

See `migrations/0005_accounts.sql`. Built as planned, with two refinements:

- `accounts.intra_id` is `ON DELETE CASCADE`, not `SET NULL`. The only `users`
  rows ever deleted are stand-ins (demo bots, test players), and their
  accounts and sessions should go with them. Deleting a *global* account
  (6.10) still clears `intra_id` by hand and leaves `users` alone.
- Generated `bot-###` players get no account unless someone was signed in as
  one. Every 42 user and hand-made stand-in gets one.

Sessions are copied with their tokens, then `sessions` is dropped. The
`oauth_states` columns `provider`, `link_account_id` and `pkce_verifier` are
added now and used from Phase 2.

### 0006_passwords.sql (Phase 2)

```sql
-- Email + password logins. One per account at most.
CREATE TABLE passwords (
    account_id  INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    email       TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    hash        TEXT    NOT NULL,       -- argon2id PHC string
    verified_at TEXT,                   -- NULL until the link in the email is opened
    created_at  TEXT    NOT NULL,
    changed_at  TEXT    NOT NULL
);

-- Single-use links sent by email. Only a SHA-256 of the token is stored, so a
-- copy of the database cannot be used to verify or reset anything.
CREATE TABLE email_tokens (
    token_hash  TEXT    PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    purpose     TEXT    NOT NULL CHECK (purpose IN ('verify', 'reset')),
    created_at  TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL,
    used_at     TEXT
);

-- Mails sent today, so the app never goes past the mail service's free
-- daily limit (see MAIL_DAILY_LIMIT).
CREATE TABLE mail_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    purpose   TEXT NOT NULL,
    sent_at   TEXT NOT NULL
);
```

### 0007_global_pool.sql (Phase 2)

```sql
-- The global pool's guesses. Same shape and rules as `guesses`, keyed by
-- account instead of intra id. `participates` works as in the 42 pool
-- (admin ghost guesses, and guesses voided by a ban).
CREATE TABLE global_guesses (
    round_date   TEXT    NOT NULL,
    account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    value        INTEGER NOT NULL CHECK (value >= 1),
    submitted_at TEXT    NOT NULL,
    participates INTEGER NOT NULL DEFAULT 1,
    ip_hash      TEXT,                  -- see section 11; never the raw address
    PRIMARY KEY (round_date, account_id)
);
CREATE INDEX idx_global_guesses_round ON global_guesses(round_date, participates, value);

-- Signup signals for spotting clusters of fake accounts (section 11).
ALTER TABLE accounts ADD COLUMN signup_ip_hash TEXT;
```

### 0008_points.sql (Phase 3)

```sql
-- Every change to a balance, ever. A balance is SUM(amount). Rows are never
-- updated or deleted; a correction is a new row.
CREATE TABLE ledger (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL REFERENCES accounts(id),
    round_date  TEXT    NOT NULL,       -- the round this belongs to
    kind        TEXT    NOT NULL CHECK (kind IN
                  ('signup', 'grant', 'stake', 'pot', 'bet', 'bet_win', 'refund', 'adjust')),
    amount      INTEGER NOT NULL,       -- positive credits, negative debits
    ref         TEXT    NOT NULL,       -- what makes this row unique, e.g. a bet id
    note        TEXT,                   -- moderator adjustments say why
    created_at  TEXT    NOT NULL,
    -- The same thing can never be credited or charged twice.
    UNIQUE (account_id, kind, ref)
);
CREATE INDEX idx_ledger_account ON ledger(account_id);

-- One row per settled global round: how big the pot was and where it went.
-- Its existence is what marks a round as settled (section 10).
CREATE TABLE pots (
    round_date   TEXT    PRIMARY KEY,
    stakes       INTEGER NOT NULL,      -- sum of this round's guess stakes
    carried_in   INTEGER NOT NULL,      -- rolled over from rounds nobody won
    winner_id    INTEGER REFERENCES accounts(id),   -- NULL: nobody won, pot rolls on
    paid         INTEGER NOT NULL,      -- what the winner got (0 if nobody won)
    players      INTEGER NOT NULL,
    settled_at   TEXT    NOT NULL
);
```

### 0009_bets.sql (Phase 4)

```sql
CREATE TABLE bets (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   INTEGER NOT NULL REFERENCES accounts(id),
    round_date   TEXT    NOT NULL,
    market       TEXT    NOT NULL,      -- e.g. 'under:10', 'over:20', 'none'
    stake        INTEGER NOT NULL CHECK (stake > 0),
    odds_x100    INTEGER NOT NULL,      -- locked at placement: 2.35 is stored as 235
    placed_at    TEXT    NOT NULL,
    result       TEXT    CHECK (result IN ('won', 'lost', 'void')),  -- NULL until settled
    payout       INTEGER,               -- stake * odds, rounded down; 0 if lost
    settled_at   TEXT,
    UNIQUE (account_id, round_date, market)   -- one bet per market per round
);
CREATE INDEX idx_bets_round ON bets(round_date);
```

All amounts are whole points (`i64`). Odds are stored as hundredths so there
are no floats in the database, matching how `stats.rs` already handles
averages.

---

## 6. Accounts and sign-in

### 6.1 Who is signed in

`current_user()` today returns a `db::User` (a 42 student). It becomes:

```rust
pub struct Viewer {
    pub account: Account,     // always present when signed in
    pub intra: Option<User>,  // Some for 42 students; the 42 pool needs this
}
```

- **42 pool handlers** take `viewer.intra`. If it is `None`, the visitor is
  treated as **not signed in for that pool**: no guess form, a "this pool is
  for 42 Vienna students, play the global pool" note instead.
- **Global pool handlers** use `viewer.account`.
- A banned or deleted account is treated as signed out.

### 6.2 Sign-in page `/login`

Today `/login` goes straight to 42. It becomes a page with up to four options.
Each OAuth button only shows when its keys are configured:

```
 sign in
 ───────────────────────────────
 [ 42 intra ]       42 students: both pools
 [ GitHub   ]
 [ Google   ]
 ── or ──
 email    [                ]
 password [                ]
 [ sign in ]   forgot password?   create account
```

**The 42 redirect URI does not change** (`/auth/callback`, registered on the
intra app), so the existing intra application keeps working without being
edited. The 42 button links to `/login/42`, which does what `/login` does now.

### 6.3 42 sign-in (changed only underneath)

Same flow as today, campus check included for the 42 pool (O3). After
`upsert_user`:

1. Find the account with `intra_id = me.id`, or create one (username = 42
   login; if that username is taken by a non-42 account, the new account gets
   `login-42`, and so on).
2. Ensure an `identities` row `('42', intra_id)`.
3. Create an `account_sessions` row.

With O3 = yes, a student from another campus gets an account with **no
`intra_id` link to the 42 pool**. The callback then skips `upsert_user`, so
they never appear in the 42 pool, but they can play the global pool.

### 6.4 GitHub sign-in

- Authorization code flow with PKCE and the existing `state` table.
  Scope: `read:user user:email`.
- `GET https://api.github.com/user` gives `id`, `login` and `created_at`.
  `GET /user/emails` gives the primary verified email.
- **Refused** if `created_at` is less than `GITHUB_MIN_ACCOUNT_AGE_DAYS` ago:
  "GitHub accounts newer than 30 days can't sign up. Use Google or email
  instead."
- Subject = GitHub numeric `id` (logins can be renamed; ids cannot).
- New account → username suggestion = GitHub login (see 6.8).

### 6.5 Google sign-in

- OpenID Connect authorization code flow with PKCE. Scopes: `openid email`.
- The ID token comes straight from Google's token endpoint over TLS, so its
  claims are read without verifying its signature. This is allowed by the
  OIDC spec for the code flow.
- Requires `email_verified = true`.
- Subject = `sub`.
- Google exposes no account age; Google's own signup checks are the barrier.

### 6.6 Email + password

**Sign up** (`GET/POST /signup`): email, password, username, plus a Turnstile
widget when configured.

1. Normalise the email (trim, lowercase). Reject it if its domain is on the
   disposable-domain list (bundled file, see [11](#11-abuse-and-fake-accounts)).
2. Check the password (rules below).
3. If the email already exists: **send that address a "you already have an
   account — sign in or reset your password" mail** and show the same
   "check your inbox" page as for a new signup. This way the form never reveals
   which emails are registered.
4. Otherwise create the account, the `identities('email', email)` row and the
   `passwords` row with `verified_at = NULL`, and send a verify link.
5. Sign the person in immediately. Until verified they can look around but
   **cannot guess, bet or receive the signup bonus**; a banner says "verify your
   email to play" with a resend button (max 3 per hour).

**Password rules**

- 10 to 128 characters (an upper bound, so hashing can't be used to exhaust
  memory).
- Not on a bundled list of the 10,000 most common leaked passwords, and not
  equal to the email or username.
- No composition rules (no "must contain a symbol"), following NIST 800-63B.

**Hashing**: `argon2` crate, argon2id, m = 19 MiB, t = 2, p = 1 (the OWASP
baseline). Hashing runs in `spawn_blocking` behind a semaphore of 4 permits,
so a burst of logins cannot use more than ~80 MiB.

**Sign in** (`POST /login/email`)

- One generic error for every failure: "wrong email or password."
- Unknown emails still run a dummy argon2 verify, so response time does not
  reveal whether an account exists.
- Rate limits (in memory, see [11.4](#114-rate-limits)): 5 failures per email
  per 15 minutes and 20 per IP per 15 minutes. Past either limit: "too many
  attempts, try again in N minutes."

**Verify** (`GET /verify?token=…`): marks `verified_at`, credits the signup
bonus (Phase 3), and redirects to `/g?msg=verified`. Tokens are valid for 24
hours and can be used once.

**Forgot / reset** (`GET/POST /forgot`, `GET/POST /reset?token=…`)

- `/forgot` always says "if that email has an account, a link is on its way."
- Reset tokens are valid for 1 hour and can be used once.
- Setting a new password **ends every session of that account** and signs the
  person in fresh.

**Token format**: 32 random bytes, base64url, sent in the link. Only the
SHA-256 hash is stored.

### 6.7 Linking several logins to one account

`/account` lists the account's logins and lets you add or remove them.

- **Link** GitHub/Google: starts the OAuth flow with `link_account_id` set. If
  that GitHub/Google identity already belongs to *another* account: "that
  GitHub account is already used by another player."
- **Link 42**: same flow; on success it also sets `accounts.intra_id` if the
  campus check passes. This is how someone who signed up with Google first
  gets the 42 pool later.
- **Add a password**: email + password, then verification as in 6.6.
- **No automatic linking by email.** If a Google sign-in brings an email that
  belongs to an existing password account, it does *not* merge them. It says
  "an account with this email exists — sign in to it and link Google from your
  account page." Matching emails across providers is a known account-takeover
  route.
- **Unlink** is allowed while at least one other login remains. **42 cannot be
  unlinked** while `intra_id` is set, because the 42 pool depends on it.

### 6.8 Usernames

- 3 to 20 characters, `[a-z0-9_-]`, case-insensitive unique.
- **Reserved**: every login in `users` (42 students), plus `admin`, `mod`,
  `system`, `deleted`, `bot-*`, `test*`. This stops a stranger registering as
  a 42 student's login in the global pool.
- In the global pool, accounts linked to 42 show a small `42` tag next to
  their name.
- Can be changed on `/account` once every 30 days.

### 6.9 Sessions and cookies

- The same cookie (`ig42_session`), the same 30-day opaque token and the same
  `HttpOnly`, `SameSite=Lax` and `Secure` settings. Only the table changes.
- Every form already posts same-site, and `SameSite=Lax` blocks cross-site
  POSTs, so no separate CSRF tokens are needed. **Keep it that way:** no
  state-changing GET routes, except `/verify` and `/reset`, which are
  token-protected.
- The hourly sweep in `main.rs` also deletes expired `email_tokens` and
  `mail_log` rows older than 2 days.

### 6.10 Account deletion

`POST /account/delete` (asks for confirmation):

- Sets `deleted_at` and renames the username to `deleted-<id>`.
- Deletes all identities, passwords, sessions and tokens.
- Guesses, ledger rows and bets stay, so past results and pots still add up;
  they now show under `deleted-<id>`.
- For 42 students it only deletes the *global* side. The 42 pool's `users`
  row and history are untouched (`intra_id` is set to NULL).

---

## 7. Pools, routes and pages

### 7.1 Header

```
 i guess low      today   results   trends      [ 42 vienna | global ]   pot 340 · you 1,250 pts   megardes
```

- The pool switcher shows for 42 students (and on the login page for
  everyone).
- Non-42 accounts see only the global links.
- The points read-out shows only in the global pool.

### 7.2 Routes

Existing routes keep their URLs and behaviour.

| Route | Method | What |
|---|---|---|
| `/` `/results` `/day/{date}` `/trends` `/guess` `/guess/confirm` | | **42 pool, unchanged.** |
| `/login` | GET | Sign-in chooser (6.2). |
| `/login/42` | GET | Starts the 42 flow (what `/login` does today). |
| `/auth/callback` | GET | 42 callback (unchanged URL). |
| `/login/github`, `/auth/github/callback` | GET | GitHub flow. |
| `/login/google`, `/auth/google/callback` | GET | Google flow. |
| `/login/email` | POST | Password sign-in. |
| `/signup` | GET, POST | Create a password account. |
| `/verify` | GET | Open an email verification link. |
| `/forgot`, `/reset` | GET, POST | Password reset. |
| `/account` | GET | Username, logins, password, delete. |
| `/account/username`, `/account/password`, `/account/unlink`, `/account/delete`, `/account/resend` | POST | Account actions. |
| `/link/{provider}` | GET | Start linking a login to the signed-in account. |
| `/g` | GET | Global pool front page: countdown, guess form, pot, your balance. |
| `/g/guess`, `/g/guess/confirm` | POST | Two-step guess, as in the 42 pool, showing the stake. |
| `/g/results`, `/g/day/{date}`, `/g/trends` | GET | Same pages as the 42 pool, fed from `global_guesses`. |
| `/g/bets` | GET | Today's markets with live odds, your open bets, your past slips. |
| `/g/bet` | POST | Place a bet. |
| `/g/leaderboard` | GET | Balances: all-time and this week's gain. |
| `/g/points` | GET | Your ledger: every credit and debit. |
| `/mod`, `/mod/ban`, `/mod/unban`, `/mod/adjust`, `/mod/void` | GET, POST | Moderation (section 12). |
| `/privacy`, `/imprint` | GET | Legal pages (section 14). |

The pages are shared, not copied: `index.html`, `results.html`, `day.html` and
`trends.html` get a `pool` field that sets link prefixes (`""` or `"/g"`) and
shows or hides pool-specific blocks (coalition-points note vs. pot and
balance).

### 7.3 Who sees names

Today `names_visible` = "is signed in". With public signups that would show 42
students' logins to anyone who makes an account. New rule:

- **42 pool**: names visible only to viewers with `intra` set (42 students).
  Everyone else sees results without names, like logged-out visitors today.
- **Global pool**: names visible to any signed-in account.

### 7.4 Global front page

```
 GLOBAL POOL                                        closes in 3h 12m
 ────────────────────────────────────────────────────────────────────
 pot   340 pts   34 players so far   (rolled over: 120 from yesterday)
 you   1,250 pts

 your number  [        ]  [ lock it in for 10 pts → ]

 bets  under 10 ×1.45   over 10 ×2.60   nobody wins ×9.80     all bets →
```

After guessing, the sealed box looks like the 42 pool's, with "stake 10 pts"
under it.

---

## 8. Points economy (global pool only)

Every number here is a constant in a new `src/economy.rs`, named and
documented like `PARTICIPANT_POINTS` in `points.rs`.

| Constant | Default | Meaning |
|---|---|---|
| `SIGNUP_BONUS` | 500 | Credited once, when the account is verified (OAuth accounts: on creation). |
| `DAILY_GRANT` | 100 | Credited once per round to every account that visits `/g` while signed in and verified. |
| `GUESS_STAKE` | 10 | Charged when a guess is confirmed; goes into the pot. |
| `MAX_BET` | 100 | Largest stake on one bet. |
| `MAX_BETS_STAKE_PER_ROUND` | 200 | Largest total staked on bets per account per round. |

### 8.1 Rules

- **Grant.** On the first signed-in `/g` page view of a round, insert
  `ledger(kind='grant', ref=round_date, +100)`. The `UNIQUE` constraint makes
  a second credit impossible, even from two tabs at once.
  - It is "visit to collect", not automatic for everyone, so abandoned
    accounts don't pile up points and inactive fakes stay poor.
- **Stake.** Confirming a guess inserts the guess and a
  `ledger(kind='stake', ref=round_date, −10)` row **in one transaction**. If the
  balance is below 10 the guess is refused ("not enough points — come back
  tomorrow for your daily 100"). The grant always covers it, so in practice
  this only happens to someone who bet everything.
- **Pot.** Pot for round R = all stakes in R + `carried_in` from the last
  unwon rounds. **The winner takes the whole pot.** More players → more
  stakes → bigger pot, which is how "more players, more points" works. No
  extra multiplier is needed.
- **Nobody wins** (every number taken at least twice): the pot rolls into the
  next round's `carried_in`.
- **No negative balances.** Stakes and bets are checked against the balance in
  the same transaction that writes them.
- **Ledger only.** No balance column anywhere; balance = `SUM(amount)` over an
  indexed column. That's fast enough for years at this scale.

### 8.2 Worked example

40 players, 14 of them 42 students. Yesterday nobody won and 120 rolled over.

- Stakes: 40 × 10 = 400. Pot = 400 + 120 = **520**.
- The winner (lowest unique number) gets +520. Everyone else is −10 for the
  day, +100 if they visited.

### 8.3 Inflation

Grants add ~100 points per active player per day. The only drain is the bet
margin (section 9). Balances will drift upward over months, which is fine for
bragging points. Two things keep the leaderboard meaningful:

- `/g/leaderboard` has a **"this week"** tab: sum of ledger rows in the last 7
  days, excluding grants and the signup bonus. That is *gains from play*, which
  doesn't reward simply having joined early.
- If inflation ever matters, a seasonal reset (monthly or quarterly) can be
  added later as a new ledger `kind`. That's not built now.

---

## 9. Betting and odds

### 9.1 Markets

For every global round, open from when the round opens until its 12:42
deadline. W = that round's winning number (the lowest unique number), or
none.

| Market id | Wins when |
|---|---|
| `under:N` for N in 5, 10, 20, 50 | W exists and W < N |
| `over:N` for N in 5, 10, 20, 50 | W exists and W ≥ N |
| `none` | nobody wins |

Under/over bets **lose** on a no-winner day. That's standard bookmaker
behaviour and keeps `none` meaningful.

Rules:

- One bet per market per round (DB `UNIQUE`); betting both sides of a line is
  allowed.
- Stake 1 to `MAX_BET`, and total bets per round at most
  `MAX_BETS_STAKE_PER_ROUND`.
- **You don't need to guess to bet**, and betting doesn't affect your guess.
- Placing a bet inserts the bet and `ledger(kind='bet', ref=bet_id, −stake)` in
  one transaction.

### 9.2 Estimating probabilities

For each market, the probability p that it wins is estimated from past global
rounds, with recent days counting more. Then the 42 pool's history pulls it
towards a sensible value while the global pool is young.

Let the last up to 90 closed global rounds be days d = 1 … n (d = 1 is
yesterday). For a market with outcome h_d ∈ {0, 1} on day d:

```
weight      w_d = 0.5 ^ ((d − 1) / 14)            # a 14-day half-life
evidence    S   = Σ w_d · h_d,   T = Σ w_d
prior       p0  = the same estimate computed over the 42 pool's history
                  (0.5 if that has no history either)
smoothed    p   = (S + α · p0) / (T + α),  α = 4
```

- **α = 4** means the prior counts like 4 days of data. On day one,
  p = p0. After a month of global rounds the global data dominates.
- p is clamped to [0.02, 0.98], so no market is ever "certain".
- Rounds with zero real players (`participates = 1`) are skipped, and so are
  rounds that were voided by moderation.

### 9.3 From probability to odds

Decimal odds, as on European betting sites:

```
fair odds      = 1 / p
offered odds   = (1 − margin) / p,  margin = 0.08
then           rounded *down* to the nearest 0.05, clamped to [1.05, 50.00]
stored as      odds_x100 (integer)
payout         = floor(stake × odds_x100 / 100)     # includes the stake back
```

- **Locked at placement.** The odds shown on the slip are the odds you get,
  even if they change later that day.
- The odds are recomputed when the round opens (after settlement, see 10) and
  cached in memory for the day. They do **not** move with bets placed or
  guesses made today. Today's guesses are secret until 12:42, and moving odds
  would leak information about them.

### 9.4 Worked example

Market `under:10`. The global pool has 20 days of history, and the winner was
under 10 on 12 of them, mostly recently (S = 8.1, T = 12.4 after
weighting). The 42 pool's history gives p0 = 0.70.

```
p        = (8.1 + 4 × 0.70) / (12.4 + 4) = 10.9 / 16.4 = 0.665
offered  = 0.92 / 0.665 = 1.383  → 1.35
```

A 50-point bet on `under:10` pays **67** if it wins (net +17) and 0 if it
loses.

### 9.5 Bets page `/g/bets`

```
 TODAY'S MARKETS                     close 12:42 · odds locked when you bet
 ──────────────────────────────────────────────────────────────────────────
                    chance   odds
 under 5             38 %    ×2.40     [ stake ][ bet ]
 over 5              55 %    ×1.65     [ stake ][ bet ]
 under 10            67 %    ×1.35     ...
 ...
 nobody wins          7 %    ×13.10    ...

 YOUR BETS TODAY           stake   odds    pays
 under 10                     50   ×1.35     67

 PAST SLIPS  (last 14 days, won/lost/void, net)
```

The "chance" column shows p (before margin) as a percentage, with a hover
note "based on the last 90 days, recent days count more". This explains what
the algorithm does without requiring trust.

### 9.6 Code layout

`src/odds.rs` holds pure functions, so they are easy to unit test:

```rust
pub enum Market { Under(i64), Over(i64), Nobody }
pub fn outcome(market: Market, winner: Option<i64>) -> bool;
pub fn probability(history: &[Option<i64>], prior: f64) -> f64;   // newest first
pub fn offered_odds_x100(p: f64) -> i64;
pub fn payout(stake: i64, odds_x100: i64) -> i64;
```

The only floats in the codebase live in `probability()`. Everything that's
stored or shown as a number of points is an integer.

---

## 10. Settlement at 12:42

A new background task `economy::run`, modelled on `points::run`. It runs **on
every instance, test instances included**, since the points are fake. It
follows the admin clock offset on test instances so the clock can be shifted
past a deadline.

**When**: at startup, and 30 s after every 12:42 deadline.

**What**: for every closed global round with no `pots` row, **oldest first**
(rollover depends on order), **in a single transaction per round**:

1. Compute W from `global_guesses` (`participates = 1`), with the same
   lowest-unique rule and SQL as `WINNER_CTE`, pointed at the other table.
2. `stakes` = −SUM(ledger `stake` rows for that round).
   `carried_in` = the previous `pots` row's carry (its `stakes + carried_in`
   if it had no winner, else 0).
3. If there is a winner: `ledger(winner, kind='pot', ref=round_date, +pot)`.
4. Every unsettled bet of that round gets `result` and `payout`. Winners get
   `ledger(kind='bet_win', ref=bet_id, +payout)`.
5. Insert the `pots` row. **Its presence is the "settled" flag.** The
   `UNIQUE` constraints on `ledger` mean that even a crash mid-way followed
   by a retry cannot pay anything twice.

After settling, recompute today's odds (9.3).

Settlement does not depend on any external service, so there are no retries
or backoff like the coalition payout. A failure is a bug. It is logged at
`error` and retried at the next startup or deadline.

---

## 11. Abuse and fake accounts

### 11.1 The threat

- **Blocking.** One person controls several accounts and has them guess 1, 2,
  3 … so their main account wins with a slightly higher number and takes the
  pot, which also contains the fakes' stakes.
- **Bet manipulation.** Bet `over:10`, then use fakes to block 1–9.
- **Farming.** Fakes collect daily grants. Balances can't be transferred, so
  this only matters through the pot (above).

**The 42 pool is immune.** Every account is one real 42 student, and nothing
here changes that.

### 11.2 What stops it (layered, all free)

| Layer | Effect |
|---|---|
| GitHub min account age (30 days) | Fresh GitHub accounts can't sign up. |
| Google verified email | Each fake costs a Google account (phone verification often required). |
| Disposable email blocklist | Throwaway inboxes (mailinator etc.) are refused. Bundled list from the open-source `disposable-email-domains` project, refreshed when dependencies update. |
| Email verification before playing | Every fake needs a real, working inbox. |
| Cloudflare Turnstile on signup | Stops scripted bulk signups. Free, no cookie banner needed. |
| Guess stake | Each blocking account spends 10 points per day, so it must be visited daily to keep collecting its grant: a recurring cost in effort. |
| Bet caps (100 per bet, 200 per round) | The most a manipulated bet can pay is bounded: at ×2.60 odds, 100 points stake → 160 profit. |
| Signup and guess IP clustering (11.3) | Moderators see suspicious groups and can ban them. |

### 11.3 Clustering signals (for humans, not automatic bans)

The raw IP is **never stored**. Instead:
`ip_hash = HMAC-SHA256(IP_HASH_KEY, ip)`, truncated to 16 hex characters and
rotated each month by including the month in the key. That's enough to see
"these 6 accounts share a network this month" and nothing more.

`/mod` shows, for the open round and the last 7 days:

- groups of ≥ 3 accounts with the same signup or guess `ip_hash`;
- groups of accounts created within 10 minutes of each other;
- **consecutive-run patterns**: accounts whose guesses formed the run 1…k under
  the winner on the same day, more than once in 14 days.

The client IP comes from `X-Forwarded-For`, trusted only from Caddy (the app
listens only on the compose network). The left-most address that Caddy
appended is used.

### 11.4 Rate limits

A small in-memory limiter (`src/limits.rs`, a mutex-guarded
`HashMap<key, VecDeque<Instant>>`, no new crate). Limits reset on restart,
which is acceptable.

| Action | Limit |
|---|---|
| Password sign-in failures | 5 per email / 15 min, 20 per IP / 15 min |
| Signups | 3 per IP / hour |
| Verification/reset mails | 3 per account or email / hour |
| Bets | 30 per account / minute (stops scripts) |

### 11.5 What this does *not* stop

A patient person with several real Google accounts can still block numbers.
This is accepted, and documented in [18](#18-risks-and-known-weaknesses):
points are bragging-only, the payoff is capped, and moderators can ban.

---

## 12. Moderation

The live site has no admin today: `/admin` only exists on test instances, and
that stays so. The global pool needs moderation on the live site, so there is
a separate, narrower role.

- `MODERATORS` = comma-separated 42 logins or usernames. Empty = `/mod`
  answers 404, like `/admin` today.
- **Ban** an account (with a reason):
  - it is signed out everywhere;
  - its guess in the **open** round is set to `participates = 0` and its stake
    refunded;
  - its open bets are voided and refunded.
  
  Settled rounds are not re-settled. If a ban should undo a past win, a
  moderator uses **adjust**.
- **Adjust**: a manual ledger row (`kind='adjust'`) with a mandatory note.
  Shown on the player's `/g/points` page.
- **Void a round**: before settlement only. Every stake and bet is refunded,
  no pot is paid, and the round is excluded from odds history. Meant for a
  round that was clearly attacked.
- Every moderator action is logged with `tracing::info!` (who, what, why).
  There is no separate audit table in the first version.

`/admin` on test instances gets global-pool versions of its tools: ghost
guesses, demo history (bot accounts for the global pool), impersonation of
global accounts and the clock shift. Settlement and odds can then be tried
end to end.

---

## 13. Configuration

New environment variables. Every login provider and mail is **off when its
keys are unset**: the button disappears and the feature degrades gracefully.

| Variable | Required | Default | What |
|---|---|---|---|
| `PUBLIC_URL` | for OAuth | — | e.g. `https://iguesslow.com`; OAuth redirect URIs are built from it. |
| `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` | no | — | GitHub OAuth app. Callback: `$PUBLIC_URL/auth/github/callback`. |
| `GITHUB_MIN_ACCOUNT_AGE_DAYS` | no | `30` | O4. |
| `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` | no | — | Google OAuth client. Callback: `$PUBLIC_URL/auth/google/callback`. |
| `RESEND_API_KEY` | for email login | — | Unset: email+password signup is disabled on the live site; on a test instance, mails are **printed to the log** instead of sent. |
| `MAIL_FROM` | with Resend | — | e.g. `i guess low <noreply@iguesslow.com>`. |
| `MAIL_DAILY_LIMIT` | no | `90` | Stays under Resend's free 100/day. Past it, signups show "try again tomorrow" and password resets still go through (they're more important). |
| `TURNSTILE_SITE_KEY` / `TURNSTILE_SECRET` | no | — | Unset: no CAPTCHA. |
| `IP_HASH_KEY` | for clustering | — | Random 32+ bytes. Unset: no IP signals are stored. |
| `MODERATORS` | no | empty | Section 12. |

New crates: `argon2` (password hashing), `sha2` and `hmac` (token and IP
hashes). OAuth, Resend and Turnstile are plain HTTPS calls through the
existing `reqwest` client, so no SDKs are needed.

Turnstile loads a script from `challenges.cloudflare.com`. If a
Content-Security-Policy is added later, it must allow that host on `/signup`.

---

## 14. Privacy and legal

Opening to the public means storing personal data of people who aren't 42
students, so:

- **`/privacy`**: what's stored (username, email, provider ids, hashed IPs,
  guesses, points) and why, how long, the processors used (Resend for mail,
  Cloudflare for Turnstile, Google/GitHub/42 for sign-in), and how to delete
  your account. Linked in the footer and on `/signup`.
- **`/imprint`**: Austrian law (ECG/MedienG) requires a site imprint once a
  site goes beyond purely private use. Opening it to the public is a good
  moment to add one.
- **No cookie banner needed**: the only cookie is the session cookie, which is
  strictly necessary. Turnstile is covered as a security measure. Adding
  analytics or ads later would change this.
- **Gambling law**: as long as points can't be bought, sold, transferred or
  exchanged for anything of value, this is not gambling under the
  Glücksspielgesetz. **These must stay true:**
  - no way to buy points or anything that affects points, odds or stakes;
  - no point transfers between accounts;
  - no prizes for leaderboard places.
  
  Cosmetic purchases (if ever) must be completely separate from points.
- **42**: monetisation (if ever) stays out of the 42 pool unless 42 Vienna
  agrees, since the 42 login and coalition API are school services.

---

## 15. Cost

| Item | Cost | Notes |
|---|---|---|
| GitHub OAuth, Google OAuth | €0 | Google: an "External" app in testing mode allows only 100 listed test users; switch it to "In production" before launch. `openid email` are non-sensitive scopes, so no paid or lengthy verification. |
| Resend | €0 | 3,000 mails/month, 100/day. Only verification, reset and "already registered" mails, so roughly 1–3 per new player. |
| Cloudflare Turnstile | €0 | |
| Server, SQLite, domain | already paid | No new services or containers. |
| **Total** | **€0/month** | |

**When it would start costing money**

| Trigger | Response |
|---|---|
| > ~90 signups on one day (Resend daily cap) | `MAIL_DAILY_LIMIT` makes signups wait until tomorrow; switch to Amazon SES (~$0.10 per 1,000 mails) or Resend paid (~$20/month) if it keeps happening. |
| SQLite write contention | Not expected below thousands of players. The guess rush before 12:42 is a few writes per second at most. |
| Server CPU/RAM | Argon2 is the only heavy part and is capped by the semaphore (6.6). |

---

## 16. Release plan

**Nothing here is deployed until all four phases are done.** They are built
in order, each finished and tested before the next starts, but they ship
together as **one release (v2.0.0)**.

The work lives on a long-lived `global` branch cut from `dev`, so `dev` and
`main` stay releasable for small fixes to the live site in the meantime.
`dev` is merged into `global` regularly so the two don't drift. When phase 4
is done, `global` merges into `dev`, and `dev` goes to `main` through the
usual release flow in the README.

### Phase 1 — accounts underneath (no visible change) — **done**

- Migration `0005_accounts.sql` (accounts, identities, account_sessions,
  oauth_states columns), plus the data copy of existing users and sessions.
- `Viewer` replaces `User` in `current_user`. 42-pool handlers read
  `viewer.intra`.
- 42 sign-in writes the account and identity (6.3).
- `names_visible` changes to the rule in 7.3 (no visible effect yet, since
  only 42 students can sign in).
- ~~`db.rs` query functions get a `Pool` argument~~ moved to Phase 2: with only
  one pool it would be an argument nothing varies, so it arrives together
  with the global tables it exists to choose between.
- **Done when**: every existing test passes, the site behaves identically, and
  a copy of the live database migrates with everyone still signed in.
  (Not deployed on its own; see above.)

### Phase 2 — global pool and logins

- Migrations `0006` and `0007`.
- `/login` chooser, GitHub, Google, email + password with verify/forgot/reset,
  Resend, Turnstile, rate limits, `/account` (linking, username, delete).
- A `Pool` argument for the `db.rs` query functions (`Vienna` or `Global`),
  choosing the table and id column.
- `/g` pages for the global pool, **without points yet**: guesses are free and
  the winner is shown like in the 42 pool.
- Pool switcher, `/privacy`, `/imprint`, `/mod` with ban/unban, IP clustering
  view.
- Test-instance admin tools for the global pool.
- **Done when**: someone with no 42 account can sign up three ways, guess,
  and see results; a 42 student can switch pools with one click; the 42 pool
  is unchanged.

### Phase 3 — points

- Migration `0008`, `economy.rs`, settlement task (pot only).
- Signup bonus, daily grant, guess stake, pot and rollover.
- Balance in the header, `/g/points`, `/g/leaderboard`, `/mod/adjust`,
  `/mod/void`.

### Phase 4 — bets and odds

- Migration `0009`, `odds.rs`, the bets page, settlement extended to bets,
  odds cache.
- Bet caps and the bet rate limit.

Docs (README, CODE.md) are updated as each phase lands on `global`, so they
are complete when the release goes out.

---

## 17. Testing

Following the existing style: unit tests next to the code, an in-memory
SQLite test database (as `db.rs` tests do now), and rendered-template tests
for UI rules.

**Phase 1**
- Migration test: a database with users, sessions, guesses and payouts at
  schema 0004 migrates; every session still resolves to the same 42 user; 42
  pool queries return identical results before and after.
- A non-42 `Viewer` sees the 42 pool without names and without a guess form
  (rendered test).

**Phase 2**
- Password: hash/verify round trip; length bounds; common-password and
  same-as-email rejection.
- Tokens: expire, single use, only the hash stored.
- Signup with an existing email sends the "already registered" mail and
  renders the same page as a new signup.
- Rate limiter: the 6th failure inside 15 minutes is refused, and allowed
  again after the window.
- Linking: an identity can't be linked to two accounts; no automatic merge on
  matching email; 42 can't be unlinked while `intra_id` is set.
- Reserved usernames: a 42 login can't be registered.
- GitHub account age check (parsed from a fixture response).
- Global winner query matches the 42 one on the same tallies.

**Phase 3**
- The grant can't be credited twice for one round (two concurrent inserts).
- A guess with balance < stake is refused and writes nothing.
- Settlement: winner gets stakes + carry; no-winner rolls over; three unwon
  days in a row then a win pays everything; re-running settlement pays
  nothing more.
- A ban in the open round refunds the stake and removes the guess from the
  winner calculation.

**Phase 4**
- `outcome()` for every market on: a winner below, at and above each line,
  and no winner.
- `probability()`: no history → equals prior; long constant history → close to
  the observed rate; recent days weigh more than old ones.
- `offered_odds_x100()`: margin applied, rounding down to 0.05, clamps at 1.05
  and 50.00.
- Odds locked at placement survive a recompute.
- Bet caps: over `MAX_BET`, over the per-round total and over balance are all
  refused.

---

## 18. Risks and known weaknesses

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Multi-account blocking in the global pool | Medium once it's popular | Some rounds won unfairly | Section 11 layers; moderators ban and void; payoff capped; 42 pool unaffected. |
| Phase 1 migration breaks live sessions or data | Low | High | Migration test on a copy of the live DB before release; `igldeploy` already takes a backup before every deploy. |
| Exposing 42 students' names to the public | — | High | 7.3 is built in Phase 1, before any code that lets people sign up without 42. |
| Resend free cap hit on a viral day | Low | Signups delayed a day | `MAIL_DAILY_LIMIT`, switch to SES. |
| Email deliverability (mails in spam) | Medium | Can't verify | SPF/DKIM/DMARC DNS records for `iguesslow.com` as part of the Resend setup. |
| Points inflation makes the leaderboard meaningless | Medium, slow | Low | "This week" leaderboard; seasonal reset possible later. |
| Odds are poor early on | High at launch | Low | 42-pool prior, α = 4, clamps; it's fake points. |
| Feature creep into anything that touches money | — | Legal | Section 14 rules. |

---

*Implementation starts with Phase 1 once the open decisions in
[section 3](#3-open-decisions-with-defaults) are confirmed or left at their
defaults.*
