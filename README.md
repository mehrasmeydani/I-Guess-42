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

See [deploy/README.md](deploy/README.md) for running a test instance alongside
the live one.

## Understanding the code

**[CODE.md](CODE.md)** is a full walkthrough of the codebase: every file, every
function, and every line that is not self-evident — what it does, why it is
written that way, and what breaks if it changes.

## Learning this from scratch

If you want to rebuild this yourself rather than read the finished code,
**[LEARNING.md](LEARNING.md)** is a 0–100 roadmap: what to learn in what order,
which sources are authoritative, and the design questions you should decide for
yourself. It deliberately withholds the answers.

## Going live on Proxmox

The live site is **https://iguesslow.com**, on a Proxmox guest. Everything
needed is in **[deploy/PROXMOX.md](deploy/PROXMOX.md)**: creating the guest,
installing Docker, port forwarding and DNS, the 42 redirect URIs, then

```sh
./deploy/proxmox.sh root@<guest-ip>
```

Test locally first with `ENV_FILE=.env.test cargo run` (your login in
`ADMIN_LOGINS`, its own database). `.env` holds the live settings and must
keep `ADMIN_LOGINS` empty; the deploy script refuses a test instance unless
you insist. The AWS route below still works too.

## Going live on AWS — what's still to do

The app is built, tested and containerised, but **it has never been deployed**.
Two things are outstanding, and both need your account rather than more code.

- [ ] **AWS access keys** — the account exists, the keys were never created
- [ ] **A domain** — required before HTTPS can work
- [ ] Register `https://<domain>/auth/callback` on the intra application
- [ ] Run `./deploy/provision.sh`, then `./deploy/deploy.sh`

### 1. AWS access keys

`aws sts get-caller-identity` currently answers `NoCredentials`. The CLI itself
is installed at `/sgoinfre/megardes/aws/bin/aws` and is already on your `PATH`
via `~/.zshrc` and `~/.bashrc`.

In the AWS console, signed in as the root user or an admin:

1. **IAM → Users → Create user**. Name it `i-guess-42-deploy`. Leave console
   access **off** — this identity is only ever used by the CLI.
2. **Next → Attach policies directly → Create policy → JSON**. Paste
   [`deploy/iam-policy.json`](deploy/iam-policy.json) and save it as
   `i-guess-42-deploy`. (`AmazonEC2FullAccess` is one click and also works, but
   it grants far more than this project needs.)
3. Finish creating the user, open it, then **Security credentials → Create
   access key → Command Line Interface**.
4. Back on this machine:

   ```sh
   aws configure
   # AWS Access Key ID:     AKIA...
   # AWS Secret Access Key: ...
   # Default region name:   eu-central-1     # Frankfurt, closest to Vienna
   # Default output format: json
   ```

The secret key is displayed exactly once. If you lose it, delete that key and
create another — it cannot be retrieved.

### 2. A domain

Caddy obtains a Let's Encrypt certificate on first boot, but Let's Encrypt has
to reach the server *by name*, so a bare IP will not do.

> **Route 53 does not include a free first year.** A `.com` is billed at
> roughly **$14 immediately**, plus **$0.50/month** for the hosted zone. The AWS
> free tier covers compute and bandwidth, never domain registration.

The genuinely free option for a 42 student is the
[GitHub Student Developer Pack](https://education.github.com/pack), which
includes a free `.me` for a year through Namecheap. Any registrar is fine —
all that matters is being able to set an **A record** pointing at the IP that
`provision.sh` prints.

### 3. Then deploy

```sh
./deploy/provision.sh          # ~2 min; creates the instance, prints its IP
# point the domain's A record at that IP and wait for DNS to propagate
cp .env.example .env           # SITE_DOMAIN + FT_CLIENT_ID + FT_CLIENT_SECRET
./deploy/deploy.sh
```

Don't forget to add `https://<your-domain>/auth/callback` as a redirect URI on
the intra application — sign-in fails with a redirect-uri mismatch otherwise.

`./deploy/teardown.sh` deletes everything again and stops the billing.

> **Caveat:** the scripts in `deploy/` have never been run against a real AWS
> account, because there were no credentials available to test with. The shell
> is syntax-checked and the logic is straightforward, but expect to fix a rough
> edge or two on the first `provision.sh` run.

## Setup

### 1. Rust

Already installed on this machine, under `/sgoinfre` (see `~/.zshrc`). Home has
only a few GB free and a debug build of this project is ~1.8 GB, so keep the
build artifacts off `/home`:

```sh
export CARGO_TARGET_DIR=/goinfre/$USER/i_guess_42_target
```

That directory already holds a warm build.

### 2. Register the intra application

Go to <https://profile.intra.42.fr/oauth/applications/new> and create an app
with redirect URI `http://localhost:3000/auth/callback` (add your production
URL too, once you have one). Copy the **UID** and **SECRET**.

### 3. Configure and run

```sh
cp .env.example .env
$EDITOR .env          # paste FT_CLIENT_ID and FT_CLIENT_SECRET
cargo run
```

Then open <http://localhost:3000>. The SQLite file and its migrations are
created on first start.

```sh
cargo test            # 48 tests: round boundaries, guess parsing, and the
                      # winner query against a throwaway SQLite file
cargo clippy --all-targets
```

### With Docker

The app plus Caddy for automatic HTTPS, which is also how it runs in
production:

```sh
cp .env.example .env   # SITE_DOMAIN + the 42 credentials
docker compose up -d --build
```

### Deploying

See **[deploy/README.md](deploy/README.md)** for the full path: AWS credentials,
`./deploy/provision.sh` to create an EC2 instance with Docker and a static IP,
`./deploy/deploy.sh` to ship the image, and `./deploy/teardown.sh` to delete it
all again. Nothing past `provision.sh` is AWS-specific — the same compose stack
runs on any host with Docker, a domain, and ports 80/443 open.

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

## Layout

```
src/
  main.rs        router, startup, hourly session sweep
  config.rs      environment
  round.rs       12:42 Europe/Vienna round boundaries (+ DST handling)
  stats.rs       day and multi-day statistics, charts, sparklines
  demo.rs        random bot history for test instances
  db.rs          schema access; the winner query lives here
  auth.rs        42 OAuth2 flow and session cookies
  handlers.rs    routes
  templates.rs   view structs
  app.rs         shared state and the error page
templates/       askama HTML
static/          stylesheet and countdown
migrations/      applied automatically at startup
deploy/          AWS provisioning, deployment, teardown
Dockerfile       multi-stage build -> 141 MB image, runs as non-root
compose.yml      app + Caddy (automatic Let's Encrypt)
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
