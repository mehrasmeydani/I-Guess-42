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

## Where it is going

Today there is one pool: everyone allowed in (42 Vienna by default,
`ALLOWED_CAMPUS_IDS`) plays against everyone else. The plan is to open it to
other campuses, each with its **own pool**, and maybe to merge pools later;
a worldwide pool is a far-off idea. The groundwork that needs: a campus on
each guess, and the winner query grouped per campus as well as per day.

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
cargo test                   # 67 tests
cargo clippy --all-targets   # should print no warnings
```

GitHub Actions runs both of these on every pull request against `dev` or
`main`, and also builds the production image so a broken `Dockerfile` shows up
before a deploy rather than during one. Warnings are errors there. Nothing in
CI deploys anything; rolling out stays the manual step described under
[Running in production](#running-in-production).

Templates are compiled into the binary, so a typo in a template is a build
error, not a broken page. The stylesheet and script in `static/` are read from
disk and only need a reload. Keep to the look described at the top of
`static/style.css`.

## Branches

| Branch | What it is |
|---|---|
| `main` | Exactly what runs on https://iguesslow.com. Only updated when a new version is deployed. |
| `dev` | Where work happens. Open pull requests against `dev`. |

Each deployed version is tagged (`v1.0.0`, ...). To deploy, bump
`version.txt`, merge `dev` into `main`, tag the merge with the same number, and
[roll it out](#deploying). The footer of every page prints that file, so you can
tell what is running on a site without looking at the server.

It is `version.txt` and not `version` in `Cargo.toml` for a build reason: the
Dockerfile compiles the dependencies in a layer keyed on `Cargo.toml`, so
editing that file for a release throws away 281 compiled crates and adds
minutes to the deploy. `version.txt` is copied after that layer, and a release
rebuilds only this crate.

## Running in production

The live site, **https://iguesslow.com**, is run by the maintainer from the
`Dockerfile`, `compose.yml` and `Caddyfile` in this repository: the app plus
Caddy, which fetches the HTTPS certificate. The same stack runs anywhere with
Docker:

```sh
cp .env.example .env   # SITE_DOMAIN, the 42 credentials, ADMIN_LOGINS empty
docker compose up -d --build
```

On a host where something else already terminates TLS, Caddy is in the way and
the app needs to be reachable from outside the compose network.
`compose.override.yml.example` is that setup: copy it to
`compose.override.yml`, which Compose merges on top and git ignores, so each
server keeps its own. It also writes down the two settings that bite quietly
once Caddy is gone.

## Deploying

Updating a site that is already serving, as opposed to the first install
above. Roughly ten minutes, most of it the build. Steps are in order because
step 2 is the one you cannot do afterwards.

### 1. Cut the release

On your own machine, not the server.

```sh
printf '1.2.0\n' > version.txt
git commit -am "v1.2.0" && git push origin dev

git checkout main
git merge --no-ff dev          # --no-ff: main carries merge commits, so it cannot fast-forward
git push origin main

git tag -a v1.2.0 -m "v1.2.0" $(git rev-parse main)
git push origin v1.2.0
```

Two things that have each gone wrong before:

- **Tag an explicit commit, never an implicit `HEAD`.** During a conflicted
  merge `HEAD` is still the *pre-merge* commit, so a bare `git tag` quietly
  captures the old `version.txt` and the deploy serves the previous version
  number.
- **Do not chain the merge and the tag on one `&&` line.** A failed merge
  short-circuits the push, but the next command still runs, against a
  half-merged tree.

One check before going near the server:

```sh
git show v1.2.0:version.txt    # must print the version you just tagged
```

### 2. Back up the database

The whole game is a single SQLite file in the `game_data` volume — every
round, every guess, every session. There is no second copy anywhere, and three
routine-looking commands delete it: `docker compose down -v`,
`docker volume prune`, and `docker system prune -a --volumes` (which also takes
`caddy_data`, so the site comes back without its certificates). Plain
`docker compose down` is safe; so is `up -d --build`, which is all a deploy
needs.

```sh
docker compose stop app        # SQLite in WAL mode: copy it cold, not under a writer
docker run --rm \
  -v "$(docker volume ls -q --filter name=game_data)":/data \
  -v "$PWD":/out \
  alpine tar czf "/out/game_data-$(date +%F-%H%M).tgz" -C /data .
docker compose start app
```

The tarball is written by a container, so it lands owned by `root` — moving or
deleting it later needs `sudo`, or another throwaway container.

Copy that file off the box. To restore it, the reverse, with the app stopped:

```sh
docker run --rm -v "$(docker volume ls -q --filter name=game_data)":/data -v "$PWD":/in \
  alpine sh -c 'rm -rf /data/* && tar xzf /in/game_data-XXXX.tgz -C /data'
```

### 3. Roll it out

On the server:

```sh
git fetch --tags --force origin
git checkout v1.2.0
cat version.txt                # sanity: the number you expect
docker compose up -d --build
```

`--force` on the fetch is not optional: **git will not move a tag it already
has**, so without it the box silently rebuilds the old commit while reporting
the new tag name.

How long the build takes:

- **~30 seconds** for a normal release — one that touches only `version.txt`,
  `src/`, `templates/` and `static/`. The dependency layer is reused.
- **two to three minutes** when `Cargo.toml` or `Cargo.lock` changed, which
  invalidates that layer and recompiles all 281 crates. Dependency updates and
  MSRV bumps land in this bucket.

### 4. Verify

```sh
curl -fsS https://iguesslow.com/healthz    # "ok"; it runs a query, so a wedged pool fails here
docker compose ps                          # app healthy, not restarting
```

Then load a page and read the footer: it prints `version.txt`, so it tells you
which commit is actually serving. That is the check that the right thing
shipped — not the tag name, which is what lies when step 1 went wrong.

### 5. If it does not come back

```sh
docker compose logs --tail=50 app
```

Migrations run at startup (`src/db.rs`), so a bad one is not a broken page, it
is a container that will not boot — and `restart: unless-stopped` turns that
into a crash loop rather than a stopped container. `running migrations` in the
log is the signature. Recovery is the backup from step 2 plus a rollback;
migrations are forward-only and there is nothing to un-apply.

### Rolling back

```sh
git checkout v1.1.0
docker compose up -d --build
```

This puts back the old **code**, not the old **schema**: `sqlx::migrate!` has
no down scripts, so whatever the new version applied is still there. That is
fine when the migration only added something the old code ignores, which has
been true so far. When it is not, restore the backup instead — that is the
only path that actually reverses a migration.

### When not to deploy

Avoid the minutes around **12:42 Europe/Vienna**. Nothing breaks: winners are
derived on read (`src/db.rs`), so there is no scheduled job to miss, and
sessions live in SQLite rather than in memory, so a restart does not log anyone
out. But the close is the one moment the site is interesting and people are
watching it, and a container restart drops whatever requests are in flight.

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
| `IGLCP_API_KEY` | no | empty (no coalition points); pays everyone who played a round and more to its winner, ignored in test mode |
| `IGLCP_API_URL` | no | `https://iglcp-api.42vienna.com` |
| `RUST_LOG` | no | `i_guess_42=info,tower_http=warn` |
| `ENV_FILE` | no | `.env`; point it at another file to keep two setups side by side |

## Layout

```
src/
  main.rs        router, startup, hourly session sweep
  config.rs      environment
  points.rs      coalition points for players and winners (42 Vienna API)
  round.rs       12:42 Europe/Vienna round boundaries (+ DST handling)
  stats.rs       day and multi-day statistics and charts
  demo.rs        random bot history for test instances
  db.rs          schema access; the winner query lives here
  auth.rs        42 OAuth2 flow and session cookies
  handlers.rs    routes
  templates.rs   view structs
  app.rs         shared state and the error page
templates/       askama HTML
static/          stylesheet and countdown script
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
