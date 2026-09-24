# i guess low

A daily lowest-unique-number game for 42 students. Sign in with your intra
account, put one whole number in, and at **12:42 Europe/Vienna** the lowest
number that *exactly one person* picked wins the day.

Guessing 1 is only clever if nobody else does.

## Rules

- **One guess per player per round, and it is final.** Submitting shows an
  "are you sure?" page first, because it cannot be changed or withdrawn.
- Any whole number from `1` up to `9223372036854775807` (i64 max). There is no
  practical ceiling — the whole game is about going low.
- A round runs from one 12:42 Europe/Vienna deadline to the next, and is
  labelled by the date its deadline falls on. The round labelled `2026-09-06`
  is open from 2026-09-05 12:42 until 2026-09-06 12:42.
- Guesses are hidden from everyone until the round closes. Only the running
  headcount is public.
- If every number in a round was picked by two or more people, nobody wins.

## Stack

| | |
|---|---|
| Web | `axum` 0.8, server-rendered `askama` templates |
| Storage | SQLite via `sqlx` (runtime queries — no `DATABASE_URL` needed at build time) |
| Auth | 42 intra OAuth2 authorization code flow, opaque server-side sessions |

No JavaScript beyond a 20-line countdown; the game itself works without it.

## Test mode

Setting `ADMIN_LOGINS` to a comma-separated list of 42 logins turns an instance
into a **test instance**: those users get `/admin`, every page grows a warning
banner, and the routes 404 for everyone else. Leave it unset on the live
instance and the admin routes do not exist at all.

`/admin` gives you five things:

- **End the round now** — shift the game clock past the 12:42 deadline instead
  of waiting for it, then reset it afterwards. The offset is in memory only and
  shifts nothing but which round is open.
- **Demo accounts** — sign in as a stand-in player and walk the real flow,
  guess form and confirmation included. Your own session is parked and a banner
  brings you back. Only stand-ins can be used this way; a real 42 account is
  refused, which matters because classmates can sign in to a test instance too.
- **Ghost numbers** — enter a number that shows up in the admin view but is
  left out of the headcount and cannot win or burn anything. Useful for parking
  numbers in a round to see how it looks without changing the outcome.
- **Reveal and clear** — see the open round's guesses, which players cannot,
  and wipe a round to start over.
- **Demo history** — fill the last 30, 90, 180 or 365 closed rounds with
  random guesses from 300 invented `bot-###` players (hundreds a day), to
  try the results and trends pages against months of data and see how fast
  they are. One button removes every bot and every guess they made.

See [Getting started](#getting-started) for running a test instance on your own
machine.

## Understanding the code

**[CODE.md](CODE.md)** is a full walkthrough of the codebase: every file, every
function, and every line that is not self-evident — what it does, why it is
written that way, and what breaks if it changes.

## Learning this from scratch

If you want to rebuild this yourself rather than read the finished code,
**[LEARNING.md](LEARNING.md)** is a 0–100 roadmap: what to learn in what order,
which sources are authoritative, and the design questions you should decide for
yourself. It deliberately withholds the answers.

## Getting started

Everything runs on your own machine: a Rust toolchain, a SQLite file, and a
42 intra application of your own for sign-in. No Docker needed.

### 1. A Linux shell

- **Windows:** use WSL. In PowerShell, `wsl --install -d Ubuntu`, restart, and
  open the Ubuntu terminal. Clone and build **inside WSL** (under `~`, not under
  `/mnt/c`): builds on the Windows side of the filesystem are many times slower.
  VS Code works with it through the WSL extension (`code .` from the Ubuntu
  terminal).
- **macOS / Linux:** your normal terminal is fine.

### 2. Rust and a C compiler

```sh
sudo apt update && sudo apt install -y build-essential pkg-config git   # Ubuntu / WSL
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

SQLite is compiled from source as part of the build, which is what needs the
C compiler. On macOS, `xcode-select --install` covers it.

### 3. Your own 42 application

Create one at <https://profile.intra.42.fr/oauth/applications/new> with the
redirect URI `http://localhost:3000/auth/callback`, and copy its **UID** and
**SECRET**. Everyone uses their own: the live site's credentials are not
shared.

### 4. Configure and run

```sh
git clone git@github.com:mehrasmeydani/I-Guess-42.git
cd I-Guess-42
cp .env.example .env
$EDITOR .env          # paste FT_CLIENT_ID and FT_CLIENT_SECRET, and put your
                      # own login in ADMIN_LOGINS to get /admin
cargo run
```

Then open <http://localhost:3000>. The database (`data/game.db`) and its
tables are created on first start. The first build takes a few minutes;
after that it is quick.

With your login in `ADMIN_LOGINS` the instance is a **test instance**: open
`/admin` to move the clock past 12:42, sign in as demo players, and fill in
months of random history (**Demo history**) so the results and trends pages
have something to show.

### 5. Before you open a pull request

```sh
cargo test                   # 49 tests
cargo clippy --all-targets   # should print no warnings
```

Templates are compiled into the binary, so a typo in a template is a build
error, not a broken page. The stylesheet and script in `static/` are read from
disk and only need a reload. Keep to the look described at the top of
`static/style.css`.

## Branches

| Branch | What it is |
|---|---|
| `main` | Exactly what runs on https://iguesslow.com. Only updated when a new version is deployed. |
| `dev` | Where work happens. Open pull requests against `dev`. |

Each deployed version is tagged (`v1.0.0`, ...). To deploy, merge `dev` into
`main`, tag the merge, and roll it out.

## Running in production

The live site, **https://iguesslow.com**, is run by the maintainer from the
`Dockerfile`, `compose.yml` and `Caddyfile` in this repository: the app plus
Caddy, which fetches the HTTPS certificate. The same stack runs anywhere with
Docker:

```sh
cp .env.example .env   # SITE_DOMAIN, the 42 credentials, ADMIN_LOGINS empty
docker compose up -d --build
```

## Environment

| Variable | Required | Default |
|---|---|---|
| `FT_CLIENT_ID` | yes | — |
| `FT_CLIENT_SECRET` | yes | — |
| `FT_REDIRECT_URI` | no | `http://localhost:3000/auth/callback` |
| `DATABASE_URL` | no | `sqlite://data/game.db` |
| `BIND_ADDR` | no | `127.0.0.1:3000` |
| `SECURE_COOKIES` | no | `false` |
| `ADMIN_LOGINS` | no | empty (test mode off) |
| `ALLOWED_CAMPUS_IDS` | no | `53` (42 Vienna); empty lets every campus in |
| `RUST_LOG` | no | `i_guess_42=info,tower_http=warn` |
| `ENV_FILE` | no | `.env`; point it at another file to keep two setups side by side |

## Layout

```
src/
  main.rs        router, startup, hourly session sweep
  config.rs      environment
  round.rs       12:42 Europe/Vienna round boundaries (+ DST handling)
  stats.rs       day and multi-day statistics and charts
  demo.rs        random bot history for test instances
  db.rs          schema access; the winner query lives here
  auth.rs        42 OAuth2 flow and session cookies
  handlers.rs    routes
  templates.rs   view structs
  app.rs         shared state and the error page
templates/       askama HTML
static/          stylesheet, countdown script, fonts
migrations/      applied automatically at startup
Dockerfile       multi-stage build -> 141 MB image, runs as non-root
compose.yml      app + Caddy (automatic Let's Encrypt)
Caddyfile        HTTPS and the reverse proxy
```

## How the winner is computed

There is no scheduled job. Winners are derived on read, from `guesses` alone:

```sql
-- values exactly one person picked, per round
uniq    = SELECT round_date, value FROM guesses
          GROUP BY round_date, value HAVING COUNT(*) = 1
-- the lowest of those is the winner
winners = SELECT round_date, MIN(value) FROM uniq GROUP BY round_date
```

A round with no row in `winners` had no unique number, so no winner.

## Results

`/results` shows the top three of the leaderboard (everyone else is one click
away) and the last 7 closed rounds, with switches for 30 or all of them. A
search box finds rounds by the winner's login, the winning number, or a date
or month (`2026-08`), and a date picker jumps straight to that day's page.

## Day pages and trends

Every closed round has a page at `/day/YYYY-MM-DD`, linked from the results
table and the home page. It shows the winning number, the lowest number nobody
picked, the headcount, a column chart, and the most and least picked
numbers. The chart has one column per number over the range where 95% of the
picks fall; every higher pick is listed right under it with its count. The
full list of numbers is one click away underneath.

`/trends` takes the last 7 days, 30 days, or all of them together: the picks
added up, a day-by-day table with bars for players and winning numbers, and
a comparison of the older
half of the range against the newer half. That covers players per day, the
winning number, the lowest free number, and which numbers got more or less
popular. It also lists the numbers that kept coming back, and prints how long
the analysis took.

Both answer only for closed rounds, which is what keeps the open round's
numbers secret until 12:42.

## Security notes

- Sessions are 32 random alphanumeric characters, stored server-side; the
  cookie is `HttpOnly`, `SameSite=Lax`, `Secure` when configured.
- The OAuth `state` is single-use, stored server-side, and expires in 15
  minutes.
- `SameSite=Lax` is what stops cross-site guess submissions; there is no
  separate CSRF token.
- Only students whose **primary** 42 campus is in `ALLOWED_CAMPUS_IDS` can
  sign in. Anyone else is turned away at the OAuth callback, before a user
  row or session is created. Visitors from other campuses keep their home
  campus as primary, so they are refused too.
- The client secret is only ever sent to `api.intra.42.fr` in a POST body, and
  is never logged.
