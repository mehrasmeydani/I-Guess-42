# i guess 42

A daily lowest-unique-number game for 42 students. Sign in with your intra
account, put one whole number in, and at **12:42 Europe/Vienna** the lowest
number that *exactly one person* picked wins the day.

Guessing 1 is only clever if nobody else does.

## Rules

- One guess per player per round. You can change it as often as you like until
  the deadline.
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

## Learning this from scratch

If you want to rebuild this yourself rather than read the finished code,
**[LEARNING.md](LEARNING.md)** is a 0–100 roadmap: what to learn in what order,
which sources are authoritative, and the design questions you should decide for
yourself. It deliberately withholds the answers.

## Going live — what's still to do

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
cargo test            # 19 tests: round boundaries, guess parsing, and the
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
| `RUST_LOG` | no | `i_guess_42=info,tower_http=warn` |

## Layout

```
src/
  main.rs        router, startup, hourly session sweep
  config.rs      environment
  round.rs       12:42 Europe/Vienna round boundaries (+ DST handling)
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

## Security notes

- Sessions are 32 random alphanumeric characters, stored server-side; the
  cookie is `HttpOnly`, `SameSite=Lax`, `Secure` when configured.
- The OAuth `state` is single-use, stored server-side, and expires in 15
  minutes.
- `SameSite=Lax` is what stops cross-site guess submissions; there is no
  separate CSRF token.
- The client secret is only ever sent to `api.intra.42.fr` in a POST body, and
  is never logged.
