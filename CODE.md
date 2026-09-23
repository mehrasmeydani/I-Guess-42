# The code, explained

A guided walkthrough of every file in this project: what each piece does, why
it is written that way, and what would break if it were written differently.

**On "every line":** annotating all ~1,300 lines individually would duplicate
the source into a second file that goes stale the moment anything changes, and
lines like `use std::sync::Arc;` do not repay a paragraph. So this document
explains **every function, every type, and every line that is not obvious from
reading it** — which is the part that actually teaches you something. Where a
line looks strange, it is explained. Where five lines are one idea, they are
explained as one idea.

Snippets here are sometimes condensed — multi-line formatting collapsed, or
irrelevant middles elided with `...` — so the point stays visible. **The source
is authoritative**; if the two ever disagree, the source is right and this file
needs fixing.

Read it next to the source with two windows open. If you want to build the
thing yourself instead, read [LEARNING.md](LEARNING.md) first — this file gives
away all the answers.

---

## Contents

- [The 60-second picture](#the-60-second-picture)
- [What happens during one request](#what-happens-during-one-request)
- [Ideas used everywhere](#ideas-used-everywhere)
- [`src/main.rs` — startup and routing](#srcmainrs--startup-and-routing)
- [`src/config.rs` — the environment](#srcconfigrs--the-environment)
- [`src/app.rs` — shared state and errors](#srcapprs--shared-state-and-errors)
- [`src/round.rs` — what "today" means](#srcroundrs--what-today-means)
- [`src/db.rs` — storage and the winner query](#srcdbrs--storage-and-the-winner-query)
- [`src/auth.rs` — signing in with 42](#srcauthrs--signing-in-with-42)
- [`src/handlers.rs` — the routes](#srchandlersrs--the-routes)
- [`src/templates.rs` — view models](#srctemplatesrs--view-models)
- [`templates/` — the HTML](#templates--the-html)
- [`static/` — CSS and the countdown](#static--css-and-the-countdown)
- [`migrations/` — the schema](#migrations--the-schema)
- [The deployment files](#the-deployment-files)
- [If you want to change X](#if-you-want-to-change-x)

---

## The 60-second picture

One process. It serves HTML, talks to a SQLite file on local disk, and calls
out to the 42 API when somebody signs in. There is no frontend framework, no
background job scheduler, no cache, and no second service.

```
browser ──HTTP──> axum router ──> handler ──> db.rs ──> SQLite file
                                     │
                                     └──> auth.rs ──HTTPS──> api.intra.42.fr
                                     │
                                     └──> templates.rs ──> HTML string
```

The eight source files split by responsibility:

| File | Owns |
|---|---|
| `main.rs` | Process startup, the routing table, the background sweeper |
| `config.rs` | Reading the environment, once, at boot |
| `app.rs` | The state every handler shares, and the error type |
| `round.rs` | Pure time arithmetic: which round is open, when it closes |
| `db.rs` | Every SQL statement in the project |
| `auth.rs` | The OAuth2 dance and cookie construction |
| `handlers.rs` | One function per route; the glue |
| `templates.rs` | The structs the HTML templates are rendered from |

The rule that keeps this navigable: **SQL only appears in `db.rs`, and time
arithmetic only appears in `round.rs`.** When you want to know how winners are
decided, there is exactly one place to look.

---

## What happens during one request

Trace `POST /guess/confirm`, the most involved path in the app.

1. **Tokio** accepts the TCP connection and hands the parsed HTTP request to
   axum.
2. **`TraceLayer`** (a middleware wrapped around everything) starts a span for
   logging.
3. **The router** matches the method and path to `handlers::confirm_guess`.
4. **Extractors run.** axum inspects the handler's parameters and builds each
   one: `State<AppState>` clones the shared state, `CookieJar` parses the
   `Cookie` header, `Form<GuessForm>` reads the body and deserialises the
   URL-encoded fields into a struct. If any fails, the handler is never called.
5. **The handler body runs.** It looks up the session, re-derives the current
   round from the clock, validates the number, and asks `db.rs` to insert.
6. **The database call** goes through the connection pool. `.await` here means
   the OS thread goes off and serves other requests while SQLite works.
7. **The handler returns** `Redirect::to("/?msg=saved")`, which axum turns into
   a `303 See Other` response.
8. **The layers unwind**, `TraceLayer` logs the status and duration, and Tokio
   writes the bytes back.

Every route follows that shape. Only steps 5 and 7 differ.

---

## Ideas used everywhere

Understanding these six things removes most of the mystery from the rest.

### `async` / `.await`

An `async fn` does not run when you call it — it returns a *future*, a value
describing work not yet done. `.await` hands control back to the Tokio runtime
until that work is ready. One OS thread therefore serves many simultaneous
requests, because a request waiting on the database is not occupying a thread.

`#[tokio::main]` on `main` is a macro that starts the runtime and blocks on the
future your `main` body produces.

### Extractors

axum decides what a handler needs by looking at its argument types:

```rust
pub async fn confirm_guess(
    State(state): State<AppState>,   // shared state
    jar: CookieJar,                  // parsed from the Cookie header
    Form(form): Form<GuessForm>,     // parsed from the request body
) -> Result<Response, AppError>
```

The `State(state)` syntax is a pattern match destructuring the wrapper in the
argument position, equivalent to taking `s: State<AppState>` and writing
`let state = s.0;`.

**Ordering matters.** Everything except the last argument must be buildable
from just the headers; only the final argument may consume the body. Putting
`Form<...>` anywhere but last produces a trait-bound error that never mentions
argument order — recognise it by sight, because you will hit it.

### `Arc` and cheap cloning

`AppState` is cloned for every request, so every field must be cheap to clone.
`SqlitePool` and `reqwest::Client` are internally reference-counted already.
`Config` is not, so it is wrapped: `cfg: Arc<Config>`. `Arc` is an atomically
reference-counted pointer — cloning bumps a counter rather than copying the
data, and the value is dropped when the last clone goes.

### `Result` and the `?` operator

`?` unwraps a `Result`, or returns early converting the error into the
function's error type via `From`. Every handler returns
`Result<Response, AppError>`, and `AppError` has a blanket `From` for anything
convertible to `anyhow::Error` — which is why a database error, an HTTP error
and a URL parse error can all be `?`-ed in the same function.

### `Option`

Rust has no null. "Might not exist" is `Option<T>`, and the compiler forces you
to handle the missing case. This project uses it for the signed-in user
(`Option<User>`), a round's winner (`Option<WinnerView>`), a user's avatar
(`Option<String>`), and a player's guess (`Option<i64>`).

The `let ... else` form appears repeatedly:

```rust
let Some(user) = current_user(&state, &jar).await? else {
    return Ok(Redirect::to("/?msg=login_required").into_response());
};
```

Read it as: bind `user` if there is one, otherwise run the block, which must
diverge (return, break, panic). After it, `user` is a plain `User`.

### Ownership, briefly

Values have one owner. Passing by value moves ownership; `&` borrows. This is
why you see `&state.db` (borrow the pool, do not move it out of the state) and
`user.login.clone()` in the rare places a copy is genuinely needed. If the
borrow checker rejects something, it is usually asking "who owns this?" and the
honest answer is usually "it should be cloned" or "it should be borrowed for
less time".

---

## `src/main.rs` — startup and routing

90 lines: declare the modules, boot the process, describe the routes, serve.

```rust
mod app;
mod auth;
mod config;
mod db;
mod handlers;
mod round;
mod templates;
```

Each `mod x;` tells the compiler to include `src/x.rs` as a module. Without
these lines the files are not part of the build at all. Inside any of them,
`crate::round::Round` reaches across.

### Boot sequence

```rust
dotenvy::dotenv().ok();
```

Loads `.env` into the process environment *if the file exists*. `.ok()` throws
away the `Result` because a missing `.env` is fine in production, where the
values come from the real environment.

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("i_guess_42=info,tower_http=warn")),
    )
    .init();
```

Installs the logger. `tracing` is a facade — libraries emit events, and the
subscriber decides what to do with them. The filter comes from `RUST_LOG` if
set; otherwise the default shows this crate's `info` and above while keeping
`tower_http`'s per-request chatter down to warnings. `.init()` registers it
globally, which can only happen once per process.

```rust
let cfg = Config::from_env()?;
let bind_addr = cfg.bind_addr.clone();
```

Configuration is read once, at boot, and a missing secret aborts startup rather
than failing on the first sign-in attempt. `bind_addr` is cloned out now
because `cfg` is about to be moved into an `Arc`.

```rust
let db = db::connect(&cfg.database_url).await?;
db::purge_expired(&db).await?;
tokio::spawn(sweep_expired(db.clone()));
```

Open the pool and run migrations (both inside `connect`), clear out dead
sessions once at startup, then spawn a background task to keep doing it.
`tokio::spawn` returns immediately; the task runs concurrently forever. It gets
its own clone of the pool because it outlives this scope.

```rust
let http = reqwest::Client::builder()
    .user_agent(concat!("i_guess_42/", env!("CARGO_PKG_VERSION")))
    .timeout(Duration::from_secs(15))
    .build()
```

One HTTP client for the whole process, built once and shared. This matters:
each `reqwest::Client` owns a connection pool, so constructing one per request
would open a fresh TLS connection to 42 every time. `env!` reads the version
from `Cargo.toml` **at compile time**, and `concat!` glues the literals
together, so the user agent costs nothing at runtime. The 15-second timeout
means a hanging 42 API cannot pin a request open indefinitely.

```rust
let cfg = Arc::new(cfg);
if cfg.test_mode() {
    tracing::warn!(admins = ?cfg.admin_logins, "TEST MODE: /admin is reachable");
}
let state = AppState::new(db, cfg, http);
```

The warning is deliberate: test mode exposes routes that reveal an open round's
guesses, so an instance running with it on should say so loudly at boot. `?` in
`admins = ?cfg.admin_logins` is `tracing` syntax meaning "record this using its
`Debug` formatting".

### The router

```rust
let router = Router::new()
    .route("/", get(handlers::index))
    .route("/guess", post(handlers::submit_guess))
    .route("/guess/confirm", post(handlers::confirm_guess))
    ...
    .nest_service("/static", ServeDir::new("static"))
    .fallback(handlers::not_found)
    .layer(TraceLayer::new_for_http())
    .with_state(state);
```

Each `.route` binds a path *and a method*. `get(...)` and `post(...)` build a
`MethodRouter`; a `POST` to a `get`-only path yields `405 Method Not Allowed`
automatically.

The guess flow is deliberately two routes. `/guess` shows a confirmation page,
`/guess/confirm` commits — see [the handlers](#the-two-step-guess).

`nest_service` mounts a whole service under a prefix. `ServeDir` reads from the
`static` directory **relative to the working directory at runtime**, which is
why the Dockerfile copies `static/` next to the binary and sets `WORKDIR`.

`.fallback` catches unmatched paths. `.layer` wraps everything in middleware —
layers apply to routes added *before* them, which is why it comes last.
`.with_state` supplies the value every `State<AppState>` extractor will clone,
and changes the router's type from `Router<AppState>` to `Router`, which is
what `axum::serve` requires. Forgetting it produces a type error at `serve`,
far from the actual mistake.

### Serving

```rust
let listener = tokio::net::TcpListener::bind(&bind_addr).await
    .with_context(|| format!("binding {bind_addr}"))?;
axum::serve(listener, router).await.context("running the server")?;
```

`with_context` attaches a message to the error if it fails, so "address already
in use" arrives as `binding 127.0.0.1:3000: Address already in use` rather than
a bare errno. It takes a closure so the string is only built on failure.

### The sweeper

```rust
async fn sweep_expired(db: db::Db) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        ticker.tick().await;
        if let Err(err) = db::purge_expired(&db).await {
            tracing::warn!(%err, "sweeping expired rows failed");
        }
    }
}
```

An infinite loop that wakes hourly. The first `tick()` completes immediately,
which is harmless because startup already purged once. Errors are logged and
swallowed deliberately: a failed cleanup must not kill the task, or the process
would silently stop cleaning up forever. `%err` means "record using `Display`".

Note this is the *only* background task in the project. Closing a round needs
no job at all, because winners are computed on read.

---

## `src/config.rs` — the environment

56 lines, and mostly a struct.

```rust
fn req(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn opt(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
```

Two helpers: required (fail loudly, naming the variable) and optional (fall
back). `unwrap_or_else` takes a closure so the default `String` is only
allocated when actually needed.

```rust
secure_cookies: opt("SECURE_COOKIES", "false") == "true",
```

Anything other than exactly `true` is false. Blunt, but it fails to the safe
side for a boolean whose only job is to be on in production.

```rust
admin_logins: opt("ADMIN_LOGINS", "")
    .split(',')
    .map(|l| l.trim().to_lowercase())
    .filter(|l| !l.is_empty())
    .collect(),
```

`"a, B ,,c"` becomes `["a", "b", "c"]`. Lowercasing here means the comparison
later can be a plain equality check. Filtering empties means a trailing comma
or an unset variable both yield an empty list.

```rust
pub fn test_mode(&self) -> bool { !self.admin_logins.is_empty() }
```

**The whole feature hangs off this.** One variable, and the empty case is the
safe one — you cannot enable test mode by forgetting something, only by
explicitly setting a login.

```rust
allowed_campus_ids: parse_ids(&opt("ALLOWED_CAMPUS_IDS", DEFAULT_CAMPUS_IDS))
    .context("ALLOWED_CAMPUS_IDS must be comma-separated campus ids")?,
```

Which 42 campuses may play. The default is `53`, 42 Vienna, because the round
deadline is Vienna time. Unlike `ADMIN_LOGINS`, a typo here is a **startup
error** rather than a skipped entry: a silently dropped id would lock that
campus out with no clue why. An empty list lets every campus in.

```rust
pub fn campus_allowed(&self, campus_id: Option<i64>) -> bool {
    self.allowed_campus_ids.is_empty()
        || campus_id.is_some_and(|id| self.allowed_campus_ids.contains(&id))
}
```

An account with no campus at all (`None`) is refused whenever a list is set.

---

## `src/app.rs` — shared state and errors

### `AppState`

```rust
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    clock_offset: Arc<AtomicI64>,
}
```

Cloned per request, so every field is cheap to clone. Note `clock_offset` has
no `pub`: it is private to this module, reachable only through the methods
below, so no handler can write to it accidentally.

```rust
pub fn now(&self) -> DateTime<Utc> {
    Utc::now() + TimeDelta::seconds(self.clock_offset())
}
```

**The game's clock.** Every place that decides which round is open calls
`state.now()` rather than `Utc::now()`, so the admin clock controls work
everywhere at once. In production the offset is always 0 and this is exactly
`Utc::now()`.

The offset deliberately does *not* affect session expiry or the timestamps
written to rows — those use the real clock — so shifting it cannot log you out
or corrupt stored data.

`AtomicI64` allows mutation through a shared `&self` without a lock.
`Ordering::Relaxed` is sufficient because this value has no ordering
relationship with any other memory; it is a single counter read and written
independently.

### `AppError`

```rust
pub struct AppError(anyhow::Error);

impl<E> From<E> for AppError where E: Into<anyhow::Error> {
    fn from(err: E) -> Self { Self(err.into()) }
}
```

A newtype wrapper around `anyhow::Error`. The blanket `From` is what makes `?`
work on every error type in the codebase — sqlx errors, reqwest errors, URL
parse errors — without a single manual conversion.

(This looks like it should collide with the standard library's reflexive
`impl<T> From<T> for T`. It does not, because `AppError` does not implement
`std::error::Error` and so is not `Into<anyhow::Error>`. This is the pattern
from axum's own examples.)

```rust
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = ?self.0, "request failed");
        error_page(StatusCode::INTERNAL_SERVER_ERROR,
                   "Something broke on our side. Try again in a moment.")
    }
}
```

**The full error goes to the log; a generic sentence goes to the browser.**
That split is the point. Internal detail — file paths, SQL, connection strings
— is exactly what an attacker wants, and exactly what a user cannot use.

---

## `src/round.rs` — what "today" means

The subtlest file in the project, and the one worth reading twice. It contains
no I/O at all: pure functions from an instant to a round, which is why it can
be tested exhaustively without a database or a server.

### The definition

```rust
//! A round runs from one 12:42 Europe/Vienna deadline to the next, and is
//! identified by the Vienna calendar date its deadline falls on. So the round
//! labelled `2026-09-06` is open from 2026-09-05 12:42 until 2026-09-06 12:42.
```

That comment is load-bearing. There were two defensible choices — name a round
after the day it *opens* or the day it *closes* — and this project picked
closing day. The consequence: at 10:00 on the 6th you are playing for round
`2026-09-06`, and at 13:00 the same day you are playing for `2026-09-07`. That
string is the primary key in the database and the label in the UI.

```rust
pub const TZ: Tz = chrono_tz::Europe::Vienna;
pub const CUTOFF_HOUR: u32 = 12;
pub const CUTOFF_MIN: u32 = 42;
```

Named constants, not scattered literals. Moving the game to a different city or
time is a one-line change here.

### Converting a wall-clock time into an instant

```rust
fn cutoff_on(date: NaiveDate) -> DateTime<Utc> {
    let naive = date
        .and_hms_opt(CUTOFF_HOUR, CUTOFF_MIN, 0)
        .expect("12:42:00 is always a valid wall-clock time");
    TZ.from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| TZ.from_utc_datetime(&naive))
        .with_timezone(&Utc)
}
```

This is the heart of the file, and every line is defending against something.

A **`NaiveDate`** is a date with no timezone — "6 September 2026", which is not
yet a point in time. `and_hms_opt` attaches a wall-clock time to produce a
`NaiveDateTime` — "6 September 2026, 12:42" — still not a point in time,
because that means different instants in Vienna and Tokyo. It returns `Option`
because not every hour/minute/second combination is valid; 12:42:00 always is,
hence the `expect`.

`from_local_datetime` is the conversion that finally produces an instant, and
it returns a `LocalResult` with **three** possible outcomes, because a local
time can map to zero, one, or two instants:

- **One** — the normal case.
- **Zero** — the local time does not exist. When clocks spring forward at
  02:00, the wall clock jumps straight to 03:00 and 02:30 never happens.
- **Two** — the local time happens twice. When clocks fall back, 02:30 occurs
  once before and once after.

Vienna transitions at 02:00/03:00, so 12:42 is always the one-instant case.
`.earliest()` collapses all three to an `Option`, and `unwrap_or_else` supplies
a defensive fallback rather than panicking if the tz database ever changes.
This is a deliberate choice not to trust an assumption that is true today.

`.with_timezone(&Utc)` normalises to UTC for storage and comparison. The
project's rule: **reason in UTC, display in Vienna.**

### Which round is open

```rust
pub fn current(now: DateTime<Utc>) -> Self {
    let today = now.with_timezone(&TZ).date_naive();
    let today_cutoff = cutoff_on(today);
    if now < today_cutoff {
        Self { date: today, deadline: today_cutoff }
    } else {
        let tomorrow = today.succ_opt().expect("date is far from the calendar limit");
        Self { date: tomorrow, deadline: cutoff_on(tomorrow) }
    }
}
```

`now` is a **parameter, not `Utc::now()`**. That single decision is what makes
this function testable — every test passes a fixed instant — and what lets the
admin clock work by passing a shifted one.

`with_timezone(&TZ).date_naive()` asks "what is the date in Vienna right now?",
which is not necessarily the UTC date: at 23:30 UTC it is already tomorrow in
Vienna.

The comparison `now < today_cutoff` is **strict**, which decides the boundary
case: at exactly 12:42:00 the condition is false, so the round has rolled. Open
intervals are half-open — `[start, end)` — and being deliberate about that is
the difference between a correct system and one that misbehaves for one second
a day. There is a test named after exactly this.

### The small accessors

```rust
pub fn key(&self) -> String { self.date.format("%Y-%m-%d").to_string() }
```

The database key. `%Y-%m-%d` is chosen because it is fixed-width and sorts
lexicographically in the same order as chronologically — so SQL can compare
`round_date < ?` as plain strings and get the right answer. That property is
relied on throughout `db.rs`.

```rust
pub fn seconds_left(&self, now: DateTime<Utc>) -> i64 {
    (self.deadline - now).num_seconds().max(0)
}
```

Subtracting two `DateTime`s gives a `TimeDelta`. `.max(0)` clamps, so a
just-expired round reports 0 rather than a negative number the countdown would
render as nonsense.

```rust
pub fn deadline_human(&self) -> String {
    self.deadline.with_timezone(&TZ).format("%A %-d %B %Y at %H:%M %Z").to_string()
}
```

Back to Vienna for display. `%-d` is "day without a leading zero" (`6`, not
`06`), and `%Z` prints the zone abbreviation — which correctly shows `CEST` in
summer and `CET` in winter, because the instant knows its own offset.

### `format_duration`

```rust
let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
```

Integer division and remainder. `{m:02}` pads to two digits with zeros, so you
get `3:07:05` and not `3:7:5`. Rendered server-side so the countdown reads
correctly before JavaScript runs — and for people who have it off.

### The tests

Six of them, and they are the specification:

```rust
#[test]
fn the_deadline_instant_itself_belongs_to_the_next_round() {
    let r = Round::current(utc("2026-09-06T10:42:00Z"));
    assert_eq!(r.key(), "2026-09-07");
}

#[test]
fn winter_time_shifts_the_deadline_by_an_hour() {
    // 2026-01-15 is CET (UTC+1), so 12:42 local is 11:42 UTC.
    let r = Round::current(utc("2026-01-15T09:00:00Z"));
    assert_eq!(r.deadline, utc("2026-01-15T11:42:00Z"));
}
```

Note the second one: in September the deadline is `10:42Z`, in January it is
`11:42Z`. Same local time, different instant. If you refactor this file and
that test still passes, you probably did not break DST handling.

---

## `src/db.rs` — storage and the winner query

The largest file, and the only one containing SQL. Roughly half is tests.

### Connecting

```rust
let opts = SqliteConnectOptions::from_str(url)?
    .create_if_missing(true)
    .foreign_keys(true)
    .journal_mode(SqliteJournalMode::Wal)
    .busy_timeout(Duration::from_secs(5));
```

- `create_if_missing` — first run creates the file rather than erroring.
- `foreign_keys(true)` — **SQLite does not enforce foreign keys by default.**
  The `REFERENCES` clauses in the schema are decorative until you turn this on.
- `journal_mode(Wal)` — write-ahead logging: readers do not block the writer.
  It also creates `-wal` and `-shm` sidecar files, which is why backing up
  means copying all three or stopping the app first.
- `busy_timeout` — SQLite allows one writer at a time; rather than failing
  instantly when another write is in flight, wait up to five seconds.

```rust
sqlx::migrate!().run(&pool).await.context("running migrations")?;
```

`migrate!` is a macro that reads `migrations/` **at compile time** and embeds
the SQL in the binary. The deployed binary therefore needs no migration files
next to it, and cannot drift from the schema it was built against. It applies
anything not yet recorded in its bookkeeping table, so it is safe to run at
every boot.

```rust
fn sqlite_file_path(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() || rest == ":memory:" { None } else { Some(rest) }
}
```

Pulls the filesystem path out of a URL so `connect` can create the parent
directory. Handles both URL spellings, strips any `?mode=rwc` query string, and
returns `None` for in-memory databases which have no path. Tested directly.

### Timestamps

```rust
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}
```

Produces `2026-09-06T18:16:00Z`. Fixed width, always UTC, always seconds
precision — so string comparison in SQL matches chronological order, exactly
like `round_date`. Every timestamp in the database goes through this one
function, which is what makes that guarantee hold.

### Writing a guess

```rust
pub async fn insert_guess(db: &Db, round_date: &str, user_id: i64, value: i64) -> Result<bool> {
    let inserted = sqlx::query(
        "INSERT INTO guesses (round_date, user_id, value, submitted_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(round_date, user_id) DO NOTHING",
    )
    .bind(round_date).bind(user_id).bind(value).bind(ts(Utc::now()))
    .execute(db).await?
    .rows_affected();
    Ok(inserted > 0)
}
```

**The most important five lines in the project.** Guesses are final, so this
must never overwrite.

`ON CONFLICT ... DO NOTHING` relies on the composite primary key
`(round_date, user_id)`: a second insert for the same player and round violates
it, and instead of erroring, SQLite skips the row. `rows_affected()` is then 0,
which the function reports as `false`.

Why not check first and then insert? Because between the check and the insert,
another request could insert — two browser tabs, or a double-click. That is a
**time-of-check-to-time-of-use** race, and the window is small but real. `DO
NOTHING` makes the check and the write a single atomic statement, so the
database decides the winner of the race and exactly one insert can succeed.

The `?1`, `?2` placeholders with `.bind()` are **parameterised queries**. The
values never become part of the SQL text, so there is nothing to escape and SQL
injection is structurally impossible. Note this is true even for
`insert_guess`'s `value`, which comes straight from user input.

### The winner query

```sql
WITH rounds AS (
    SELECT round_date, COUNT(*) AS total
    FROM guesses WHERE round_date < ?1
    GROUP BY round_date
),
uniq AS (
    SELECT round_date, value
    FROM guesses WHERE round_date < ?1
    GROUP BY round_date, value
    HAVING COUNT(*) = 1
),
winners AS (
    SELECT round_date, MIN(value) AS value FROM uniq GROUP BY round_date
)
```

Three common table expressions — named subqueries that make the logic readable
in stages. Read them bottom-up:

- **`uniq`** groups guesses by `(round, value)` and keeps only groups of
  exactly one. This is the burn rule: a number two people picked forms a group
  of two and is discarded. `HAVING` filters *after* grouping, which is why it
  can see `COUNT(*)`; `WHERE` filters before grouping and cannot.
- **`winners`** takes the minimum of what survived, per round. **This is the
  whole game**: not the lowest guess, but the lowest of the *unique* guesses.
- **`rounds`** counts participation separately, because a round where nobody
  won still has players and still deserves a row.

`WHERE round_date < ?1` — where `?1` is the currently open round — is what
keeps an in-progress round out of every result. It works as a plain string
comparison because of the fixed-width `%Y-%m-%d` format.

The queries that use it `LEFT JOIN` from `rounds` to `winners`:

```sql
FROM rounds r
LEFT JOIN winners w ON w.round_date = r.round_date
LEFT JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
LEFT JOIN users   u ON u.id = g.user_id
```

`LEFT JOIN` keeps rows from the left side even when the right side matches
nothing, so a round with no unique number still appears — with `NULL` in the
winner columns. An inner join would silently drop those rounds, and the history
page would lie by omission.

The second join finds *who* guessed the winning value. It is unambiguous
precisely because that value was unique.

`RoundSummary` receives those NULLs as `Option`s, and `templates.rs` turns the
all-or-nothing group into `Option<WinnerView>`:

```rust
let winner = match (r.winner_login, r.winner_name, r.winning_value) {
    (Some(login), Some(display_name), Some(value)) => Some(WinnerView { ... }),
    _ => None,
};
```

### Reading rows into structs

```rust
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub display_name: String,
    pub image_url: Option<String>,
}
```

`FromRow` generates code mapping columns to fields **by name**, which is why
the queries use aliases like `u.login AS winner_login`. `Option<String>` maps a
nullable column; a plain `String` on a NULL column is a runtime decode error.

This project uses `sqlx::query_as` (runtime-checked) rather than the
`query_as!` macro (compile-time checked against a live database). The trade:
you lose compile-time verification of your SQL, and you gain a build that needs
no database — which is what lets the Docker image build anywhere.

### Test users

```rust
let (lowest,): (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM users").fetch_one(db).await?;
let id = lowest.unwrap_or(0).min(0) - 1;
```

Stand-in players get negative ids. Real 42 ids are positive, so the two can
never collide, and `SELECT * FROM users WHERE id < 0` finds every invented
player. `MIN` over an empty table is `NULL`, hence `Option`; `.unwrap_or(0)
.min(0)` makes the first invented id `-1` whether the table is empty or full of
real users.

### The tests

Ten of them run against a real temporary SQLite file, not mocks:

```rust
struct TempDb { path: std::path::PathBuf, db: Db }

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] { ... remove ... }
    }
}
```

`Drop` is Rust's destructor: the files are deleted when the value goes out of
scope, including if the test panics. The `-wal`/`-shm` suffixes are the WAL
sidecars mentioned earlier.

`:memory:` is deliberately *not* used, because each pooled connection would get
its own empty database and the tests would be nonsense.

The one to read first:

```rust
#[tokio::test]
async fn the_lowest_unique_number_wins_not_the_lowest_number() {
    // Two people grabbed 1, so it is burned. 2 is the lowest survivor.
    seed(&t.db, "2026-09-05", &[(1, 1), (2, 1), (3, 2), (4, 3)]).await;
    let r = round_summary(&t.db, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
    assert_eq!(r.winning_value, Some(2));
}
```

If you rewrite the winner query, this test is what tells you whether you got it
right. A naive `MIN(value)` returns 1 and fails here — which is the entire
reason the test exists.

---

## `src/auth.rs` — signing in with 42

The OAuth2 authorization-code flow. This file does the protocol; `handlers.rs`
does the routing around it.

### The flow, before any code

1. Visitor clicks "Sign in with 42". You generate a random `state`, remember
   it, and redirect them to intra with your `client_id` and `redirect_uri`.
2. They approve on intra's site. Intra redirects back to your `redirect_uri`
   with `?code=...&state=...`.
3. You check the `state` matches one you issued, then **your server** POSTs the
   `code` plus your `client_secret` to intra and gets an access token.
4. You call `/v2/me` with the token to find out who they are.
5. You create your own session and forget the token entirely.

The token exchange in step 3 happens **server to server**. The `code` travels
through the browser; the secret never does. That separation is the whole point
of this flow over simpler ones.

### Randomness

```rust
pub fn random_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}
```

Used for both session tokens and OAuth `state`, because both need the same
property: unguessable. `thread_rng` is seeded from the operating system and is
cryptographically secure — this matters enormously. A predictable session token
means anyone can forge a session. If you swap this for a faster non-crypto
generator, you have handed out everyone's account.

32 alphanumeric characters is about 190 bits of entropy.

### Building the authorize URL

```rust
let mut url = Url::parse(AUTHORIZE_URL)?;
url.query_pairs_mut()
    .append_pair("client_id", &cfg.client_id)
    .append_pair("redirect_uri", &cfg.redirect_uri)
    .append_pair("response_type", "code")
    .append_pair("scope", "public")
    .append_pair("state", state);
```

`query_pairs_mut` percent-encodes each value. That is why this is not a
`format!` call: `redirect_uri` contains `://` and `/`, which must arrive as
`%3A%2F%2F`. Hand-built query strings are a classic source of
intermittent-looking OAuth failures.

`response_type=code` selects this flow. `scope=public` is the minimum 42
offers — ask for no more than you need.

### The exchange

```rust
let resp = http.post(TOKEN_URL)
    .form(&[
        ("grant_type", "authorization_code"),
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", cfg.redirect_uri.as_str()),
    ])
    .send().await?;

if !resp.status().is_success() {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    bail!("42 token endpoint returned {status}: {body}");
}
```

`.form(...)` sends `application/x-www-form-urlencoded` in the **body**, not the
URL — so the secret does not land in logs, proxies, or browser history.

`redirect_uri` is sent again even though there is nothing to redirect to. The
spec requires it: the server checks it matches the one from step 1, which stops
an attacker who stole a code from redeeming it against a different callback.

Errors are `bail!`ed with the response body included, because intra's error
text ("invalid_grant", "redirect_uri_mismatch") is exactly what you need. It
reaches the log, never the browser — `AppError` sees to that.

Note `reqwest::Client` was built with `rustls-tls`, so certificate verification
is on by default. There is no code here disabling it, and there should never
be.

### Reading the user

```rust
#[derive(Debug, Deserialize)]
pub struct IntraUser {
    pub id: i64,
    pub login: String,
    pub displayname: Option<String>,
    pub image: Option<IntraImage>,
}
```

`Deserialize` generates JSON parsing. Field names match the API's spelling —
hence `displayname`, not `display_name`. Only the four fields used are
declared; serde ignores the rest of the large response.

Optional fields are `Option`, because an account may have no avatar. Declaring
`image: IntraImage` would make sign-in fail for those users with a confusing
decode error.

```rust
pub fn display_name(&self) -> &str {
    self.displayname.as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&self.login)
}
```

Falls back to the login when the display name is absent *or blank*. The
`.filter` is what catches `Some("")` and `Some("   ")`, which the API does
return and which would otherwise render as an empty name.

```rust
pub fn image_url(&self) -> Option<&str> {
    self.image.as_ref()?.link.as_deref()
}
```

`?` on an `Option` in a function returning `Option` — returns `None` early if
`image` is absent. A nested-optional unwrap in one line.

```rust
#[serde(default)]
pub campus: Vec<IntraCampus>,
#[serde(default)]
pub campus_users: Vec<IntraCampusUser>,
```

`/v2/me` lists every campus the account has been attached to (`campus`, with
names) and one link row per campus (`campus_users`), exactly one of which has
`is_primary: true`. `#[serde(default)]` turns a missing array into an empty
one instead of a decode error.

`primary_campus_id` picks the `is_primary` row, falling back to the only
campus when there is exactly one. Using the *primary* campus matters: a
student visiting Vienna from Paris gains a Vienna `campus_users` row but keeps
Paris as primary, so they are still judged as a Paris student.

### The session cookie

```rust
pub fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    cookie.set_path("/");
    cookie.set_max_age(CookieDuration::days(SESSION_TTL_DAYS));
    cookie
}
```

Each flag defends against something specific:

- **`HttpOnly`** — JavaScript cannot read `document.cookie`. If an XSS hole
  ever appears, the session is still not stealable through it.
- **`SameSite=Lax`** — the browser withholds this cookie on cross-site POSTs.
  **This is the project's CSRF defence.** Without it, another site could post a
  guess on a logged-in visitor's behalf. "Lax" still sends it on ordinary
  top-level navigation, so following a link into the site keeps you logged in —
  which is also what makes the OAuth redirect back from intra work.
- **`Secure`** — HTTPS only. Configurable because local development is plain
  HTTP; `compose.yml` forces it on in production.
- **`Path=/`** — valid site-wide.
- **`Max-Age`** — 30 days. Without it the cookie dies when the browser closes.

The cookie holds only the random token. Everything else lives server-side in
the `sessions` table, which is what makes sign-out able to actually revoke a
session rather than merely asking the browser to forget it.

```rust
/// A removal cookie must match the path the original was set on.
pub fn clearing_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookie
}
```

Browsers match cookies for deletion by name **and path**. Omitting `Path=/`
here produces a deletion that silently does nothing — a genuinely common bug.

---

## `src/handlers.rs` — the routes

One function per route, plus the shared helpers.

### Finding the visitor

```rust
async fn current_user(state: &AppState, jar: &CookieJar) -> Result<Option<User>, AppError> {
    let Some(token) = auth::session_token(jar) else {
        return Ok(None);
    };
    Ok(db::user_for_session(&state.db, &token).await?)
}
```

Called at the top of nearly every handler. `Ok(None)` means "nobody is signed
in", which is not an error — every page works logged out.

This is a plain function rather than a custom extractor. An extractor would be
more idiomatic axum; a function is less machinery and easier to read, and the
call is one line either way.

Note the expiry check lives in the SQL (`WHERE s.expires_at > ?`), so an
expired session simply fails to resolve. The hourly sweeper deletes the rows
later, but correctness never depends on it having run.

### Flash messages

```rust
fn notice_for(code: Option<&str>) -> Option<Notice> {
    Some(match code? {
        "saved" => Notice::ok("Locked in. Come back after 12:42 to see who took it."),
        "already" => Notice::error("You have already guessed this round, ..."),
        ...
        _ => return None,
    })
}
```

After a POST the app redirects, and the message survives as `?msg=saved` in the
URL. The critical detail: **the query parameter is a lookup key, never
displayed text.** An unknown code produces `None`, so
`/?msg=<script>alert(1)</script>` renders nothing at all.

Askama would escape it anyway. This is defence in depth: the dangerous data
never reaches the template.

`code?` early-returns `None` when there is no `msg` parameter. The bare
`return None` inside a match arm returns from the whole function, not the
match.

### Parsing a guess

```rust
fn parse_guess(raw: &str) -> Result<i64, &'static str> {
    let cleaned: String = raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != ',' && *c != '\u{202f}')
        .collect();

    if cleaned.is_empty() { return Err("empty"); }
    if !cleaned.chars().all(|c| c.is_ascii_digit()) { return Err("invalid"); }
    match cleaned.parse::<i64>() {
        Ok(0) => Err("too_small"),
        Ok(n) => Ok(n),
        Err(_) => Err("too_big"), // all digits, so the only way to fail is overflow
    }
}
```

Strips the separators people actually type — spaces, commas, underscores, and
`\u{202f}` (the narrow no-break space the app itself renders, so copying a
displayed number back in works).

The ordering is deliberate. After the all-digits check, the string cannot
contain a sign, a decimal point or an exponent, so `parse::<i64>()` can only
fail one way: **overflow**. That is what licenses the `Err(_) => "too_big"`
arm, and the comment says so. Without the preceding check, `"abc"` would also
land there and report a wrong message.

`Ok(0) => Err("too_small")` catches `0` and `0000`, since the game starts at 1.

The error type is `&'static str` — a code, not a sentence — which
`notice_for` turns into wording. That keeps user-facing prose in one place.

### The two-step guess

The most important flow in the app, because it is the irreversible one.

**Step one, `POST /guess`, writes nothing:**

```rust
let round = Round::current(state.now());
let round_key = round.key();

if db::my_guess(&state.db, &round_key, user.id).await?.is_some() {
    return Ok(Redirect::to("/?msg=already").into_response());
}

Ok(templates::render(&ConfirmTemplate {
    value_label: group_digits(value),
    // Canonical form, so what gets stored is exactly what was shown:
    // "007" was displayed as 7 and must be submitted as 7.
    value_raw: value.to_string(),
    round_key,
    ...
}))
```

It validates, checks there is no existing guess, and renders the confirmation
page. The number rides along in a hidden field.

`value_raw` is the *parsed* value re-serialised, not the raw input. Type `007`
and the confirmation shows `7` and submits `7`. Passing the original string
through would let the displayed and stored values drift apart — a small thing
that matters when the action cannot be undone.

**Step two, `POST /guess/confirm`, commits:**

```rust
let round_key = Round::current(state.now()).key();
if form.round != round_key {
    return Ok(Redirect::to("/?msg=rolled").into_response());
}

let msg = if db::insert_guess(&state.db, &round_key, user.id, value).await? {
    "saved"
} else {
    "already"   // lost a race with another tab; the first guess stands
};
```

Three defences, none of which trust the browser:

1. **The value is re-parsed.** The hidden field is attacker-controlled; it gets
   the same validation as typed input.
2. **The round is recomputed and compared.** If the deadline passed between the
   two pages, the guess is *refused* rather than silently landing in a round
   the player never saw. For a reversible action, quietly moving it would be
   friendlier; for an irreversible one, refusing is correct.
3. **The insert reports whether it wrote.** Posting straight to
   `/guess/confirm` and skipping the review page is allowed — and harmless,
   because `insert_guess` refuses a second guess atomically. Security lives in
   the database constraint, not in the page sequence.

That third point is the general lesson: **a confirmation page is a courtesy to
the user, never a security control.** Anyone can skip it. The rule is enforced
where it cannot be bypassed.

### `index`

```rust
let now = state.now();
let round = Round::current(now);
...
let seconds_left = round.seconds_left(now);
let guess_count = db::guess_count(&state.db, &round_key).await?;
let round_label = crate::round::format_round_date(&round_key);

Ok(templates::render(&IndexTemplate { ... round_key, round_label, ... }))
```

`now` is captured **once** into a variable and reused. Calling `state.now()`
repeatedly could straddle the deadline mid-request and render a page that
contradicts itself.

The locals before the struct literal exist for a borrow-checker reason worth
knowing: struct fields are evaluated in written order, so moving `round_key`
into the struct and *then* borrowing it for `guess_count` would not compile.
Computing first sidesteps it and reads better.

The page shows the count of guesses but never the guesses — the open round's
values are only exposed on `/admin`.

### `callback`

```rust
if let Some(err) = query.error {
    let detail = query.error_description.unwrap_or(err);
    return Ok(error_page(StatusCode::BAD_REQUEST,
        &format!("42 refused the sign-in: {detail}")));
}

let (Some(code), Some(state_token)) = (query.code, query.state) else {
    return Ok(error_page(StatusCode::BAD_REQUEST, "That sign-in link is incomplete..."));
};

if !db::consume_oauth_state(&state.db, &state_token).await? {
    return Ok(error_page(StatusCode::BAD_REQUEST, "That sign-in link has expired..."));
}

let access_token = auth::exchange_code(&state.http, &state.cfg, &code).await?;
let me = auth::fetch_me(&state.http, &access_token).await?;

if !state.cfg.campus_allowed(me.primary_campus_id()) {
    return Ok(error_page(StatusCode::FORBIDDEN,
        "This game is only open to students of this campus..."));
}

db::upsert_user(&state.db, me.id, &me.login, me.display_name(), me.image_url()).await?;

let session = auth::random_token();
db::create_session(&state.db, &session, me.id, auth::SESSION_TTL_DAYS).await?;

let jar = jar.add(auth::session_cookie(session, state.cfg.secure_cookies));
Ok((jar, Redirect::to("/")).into_response())
```

The `state` check is the security-critical line, and it comes **before** the
token exchange — no work is done for a request that cannot prove it started the
flow. `consume_oauth_state` deletes the row as it checks, in one statement, so
a state cannot be replayed.

The campus check sits **before** `upsert_user`: someone from another campus
leaves no user row and gets no session. The refusal names their home campus
when 42 provides it, so a confused visitor knows why.

`upsert_user` means the login refreshes the display name and avatar every time,
so a changed intra profile propagates without extra machinery.

The access token is used twice and then dropped. It is never stored — the app
has no further use for the 42 API, so keeping it would be pure liability.

Returning `(jar, Redirect)` works because `CookieJar` implements
`IntoResponseParts`: the tuple becomes a response with the `Set-Cookie` header
attached. `Redirect::to` is `303 See Other`, the correct status for
"POST/action finished, now go GET this".

### `healthz`

```rust
match sqlx::query("SELECT 1").execute(&state.db).await {
    Ok(_) => (StatusCode::OK, "ok").into_response(),
    Err(err) => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response(),
}
```

Touches the database rather than returning a constant, so a wedged pool reports
unhealthy instead of cheerfully saying OK. This is the one place SQL appears
outside `db.rs`, which is a small, deliberate inconsistency: a health check
that goes through the normal query layer would test less.

### The admin handlers

```rust
async fn require_admin(state: &AppState, jar: &CookieJar) -> Result<Option<User>, AppError> {
    if !state.cfg.test_mode() {
        return Ok(None);
    }
    Ok(current_user(state, jar).await?
        .filter(|u| state.cfg.is_admin(&u.login)))
}
```

Two gates: the instance must be in test mode, and the signed-in user must be
listed. `Option::filter` turns "signed in but not an admin" into `None`.

Every admin handler starts the same way, and a `None` becomes a **404, not a
403**:

```rust
fn admin_gone() -> Response {
    error_page(StatusCode::NOT_FOUND, "No such page.")
}
```

403 would confirm the route exists. 404 is indistinguishable from a typo, so a
live deployment gives away nothing. Each of the four routes checks
independently — there is no middleware doing it once, so forgetting the check
in a new admin route would be a real bug to watch for.

```rust
"deadline" => {
    let now = state.now();
    state.shift_clock(Round::current(now).seconds_left(now) + 1);
}
```

"End the round now". Jump to one second **past** the deadline, so the round is
definitively closed rather than sitting exactly on the boundary — which, per
`round.rs`, is already the next round, but the extra second removes any doubt.

### Demo accounts

```rust
let player = match db::user_by_login(&state.db, &login).await? {
    Some(u) if u.id < 0 => u,
    // Refuse to become a real person, even on a test box.
    Some(_) => return Ok(Redirect::to("/admin?msg=not_demo").into_response()),
    None => db::create_test_user(&state.db, &login).await?,
};
```

`admin_impersonate` swaps the session cookie so the admin browses as a
stand-in. The `Some(u) if u.id < 0` guard is the important line: **only
negative-id demo accounts can be impersonated.** A test instance uses the same
42 application as the live one, so real classmates can sign in to it, and
without this guard an admin could become one of them.

The admin's own token is parked in a second cookie:

```rust
if auth::admin_return_token(&jar).is_none() {
    if let Some(mine) = auth::session_token(&jar) {
        jar = jar.add(auth::admin_return_cookie(mine, state.cfg.secure_cookies));
    }
}
```

The `is_none()` check means impersonating twice in a row does not overwrite the
parked admin token with a demo one — otherwise the way home would be lost.

`admin_return` deliberately **does not** call `require_admin`:

```rust
/// Swaps back to the parked admin session. Deliberately does NOT call
/// require_admin: the caller is currently a demo account, so that check would
/// 404 and strand them.
```

It re-verifies that the parked session still resolves to an admin before
restoring it, so a stale cookie cannot grant anything, and deletes the
throwaway demo session on the way out.

### Ghost numbers

A ghost is a guess with `participates = 0`. It shows up on `/admin` and nowhere
else: excluded from `guess_count`, from the `rounds` total, from `uniq` (so it
cannot burn a real number), and from the join that names a winner. Four filters,
because missing any one of them would leak ghosts into the game in a different
way — which is what
`a_ghost_cannot_burn_a_real_number` and `ghosts_are_invisible_to_the_game` test.

```rust
/// Present only when the checkbox is ticked; HTML omits unchecked boxes.
#[serde(default)]
ghost: Option<String>,
```

An unchecked HTML checkbox sends **nothing at all**, rather than a false value.
`#[serde(default)]` plus `Option` is how you read one: absent means unchecked.
Getting this wrong gives a form that fails to submit whenever the box is
cleared.

---

## `src/stats.rs` — the day and trends pages

Pure functions over closed rounds' `(value, count)` tallies. No database and
no HTML, so all of it is tested directly.

### One day

`analyse` computes a round's headline numbers:

- **winner**: the first tally with a count of 1. The tallies are sorted, so
  the first unique value is the lowest one, the same rule as `WINNER_CTE`.
- **lowest_unpicked**: walk up from 1 and stop at the first gap.
- **most** / **least**: sorted by count, ties to the lower number, cut to
  five. "Least picked" only sees values somebody picked, so a zero never
  appears in it.

### The chart

`build` lays out the chart used on both pages in two parts, with nothing
grouped and nothing dropped:

- **Columns**, one per number from 1 up to where 95% of all picks fall
  (`CHART_PERMILLE`), between 10 and 100 of them. Capping the count keeps
  every column wide enough to read; a few stray picks at 300 or 400 would
  otherwise stretch the axis and squeeze the numbers people actually play
  into a sliver. The winner gets a column too if it is within 100.
- **The tail**: every number picked above the last column, listed lowest
  first with its count (`420 ×7`), winners marked.

Heights are a share of the tallest column, rounded up so a single pick always
shows. Only round numbers (from `nice_step`) get a label on the axis, so labels
never collide; winners are told apart by colour, and every column has its
number and count in its hover text.

`day_chart` colours a day: winner, picked once, shared. `range_chart` colours
several days added together: numbers that won at least one day, and the rest.
How many days a number won is in its hover text.

### Several days

`trend` takes per-day rows (`db::range_tallies`, oldest first) and works out:

- a `DayLine` per day (players, winner, lowest free number), by running
  `analyse` on each day;
- **totals**, every pick added up per value, and **wins**, how many days each
  value won;
- **halves**: the range split into an older and a newer half (an odd middle
  day goes to the newer one), with average players, winning number and
  lowest free number for each. Averages are integer hundredths, printed by
  `hundredths`;
- **rising** / **falling**: each value's share of all picks in the older half
  against the newer half, in tenths of a percent (`permille`). Values picked
  only once in the whole range are skipped as noise;
- **regulars**: the values picked on the most different days, the part that
  did not change.

Shares and day counts are looked up in `HashMap`s. A year of rounds holds tens
of thousands of distinct values, and scanning lists for each one made the
analysis quadratic; with maps a year analyses in well under a second even in a
debug build. The trends page prints the real time it took.

---

## `src/demo.rs` — random history for test instances

`generate` invents guesses for the `days` closed rounds ending yesterday, from
a pool of 300 `bot-###` players. Turnout grows over the months and dips at
weekends. Early on, most bots pile onto 1-5; later, more of them spread
higher. The trends page therefore has real drift to find. A share of bots
pick joke numbers (42, 69, 420, 1337) and a few go as high as 500. Nobody
goes further, because the point of the game is to go low. The share of each
kind of player is a small table in `pick`, and the shares always add up to 1.

`db::insert_demo` writes it in one transaction. It creates any missing bots
with negative ids, like every stand-in, and uses `INSERT OR IGNORE`, so running
it twice tops up instead of duplicating. It refuses to reuse a `bot-###` login
that belongs to a real (positive-id) account. `db::remove_demo` deletes the
bots; `ON DELETE CASCADE` takes their guesses with them. Hand-made stand-ins
and real players are untouched.

It is reachable only from `/admin` (`POST /admin/demo`,
`POST /admin/demo/remove`). Those routes go through `require_admin`, so they
answer 404 on a live instance with no `ADMIN_LOGINS`. The open round is never
touched.

---

## `src/templates.rs` — view models

The bridge between database rows and HTML. Its job is to make sure the
templates contain no logic: everything is decided here, and the template only
places already-finished values.

### `group_digits`

```rust
pub fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 { out.push('-'); }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}'); // narrow no-break space
        }
        out.push(ch);
    }
    out
}
```

Guesses can reach 19 digits, and `9223372036854775807` is unreadable. This
renders `9 223 372 036 854 775 807`.

`unsigned_abs()` rather than `abs()` is deliberate: `i64::MIN.abs()` **panics**,
because its magnitude does not fit in an `i64`. `unsigned_abs` returns a `u64`
and cannot fail. Guesses are always positive, so this can never trigger — which
is exactly why it is the kind of latent panic that survives to production.
Choosing the total function costs nothing.

The separator is `\u{202f}`, a narrow no-break space, so numbers never wrap
across lines mid-number. `parse_guess` strips this character specifically, so
copying a displayed number back into the form works.

`String::with_capacity` pre-allocates the exact final size — a micro-optimisation,
but it documents the expected length.

### The view structs

```rust
pub struct UserView { pub login: String, pub display_name: String, pub image_url: Option<String> }

impl From<User> for UserView { ... }
```

Why not hand `db::User` straight to the template? Because a view struct decides
what the page is *allowed* to see. `db::User` carries `id`; `UserView` does
not, so no template can accidentally render an internal id.

`From` implementations mean call sites read `.map(UserView::from)` or
`.map(Into::into)` rather than repeating field-by-field construction.

```rust
pub struct WinnerView {
    ...
    pub value_label: String,   // already formatted
}
```

Note `value_label: String`, not `value: i64`. Formatting happens here, so
`{{ w.value_label }}` in the template is a plain substitution. Templates that
call functions are harder to test and harder to read.

The all-or-nothing winner columns collapse in one place:

```rust
let winner = match (r.winner_login, r.winner_name, r.winning_value) {
    (Some(login), Some(display_name), Some(value)) => Some(WinnerView { ... }),
    _ => None,
};
```

Matching on a tuple of three `Option`s. The SQL guarantees they are all present
or all absent; the `_` arm handles the impossible case without panicking.

### `Notice`

```rust
pub struct Notice { pub kind: &'static str, pub text: String }

impl Notice {
    pub fn ok(text: impl Into<String>) -> Self { Self { kind: "ok", text: text.into() } }
    pub fn error(text: impl Into<String>) -> Self { Self { kind: "error", text: text.into() } }
}
```

`kind` is `&'static str` because it is only ever one of two literals, and it
goes straight into a CSS class: `<p class="notice {{ n.kind }}">`.

`impl Into<String>` accepts both `&str` and `String` at the call site.

### `render`

```rust
pub fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(%err, "template rendering failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "template rendering failed").into_response()
        }
    }
}
```

Askama's `render()` returns a `Result`. Rendering a template to a `String`
here, rather than using an integration crate that implements `IntoResponse` for
templates directly, keeps the project free of a version-compatibility
dependency between the template engine and the web framework — a common source
of upgrade pain.

`Html(...)` sets `Content-Type: text/html; charset=utf-8`. Without it the
browser would display the markup as plain text.

The error branch cannot render the error page, since that is itself a template
and might be the broken one — hence the bare string.

---

## `templates/` — the HTML

Askama compiles these into Rust code **at build time**. A typo in a field name
is a compile error, not a blank page at runtime. The cost is that editing HTML
requires a rebuild.

### `base.html` — the layout

```html
{% if test_mode %}
<div class="testbanner">
  Test instance &mdash; this is not the live game.
  {% match user %}
    {% when Some with (_u) %}<a href="/admin">Admin</a>
    {% when None %}
  {% endmatch %}
</div>
{% endif %}
```

Every page struct carries a `test_mode: bool`, so the banner cannot be
forgotten on a page. Making it impossible to have a test instance that looks
like the live one is worth the small repetition.

```html
<main>{% block content %}{% endblock %}</main>
```

`{% block %}` defines a hole child templates fill. Each child starts
`{% extends "base.html" %}` and supplies its own `content`.

The mechanism to understand: **the base template is compiled against the
child's struct.** So anything `base.html` references — `user`, `test_mode` —
must exist as a field on *every* template struct that extends it. Add a
reference to the layout and every page struct must grow that field; that is why
`ErrorTemplate` has `test_mode` even though it is always `false`.

```html
{% match user %}
  {% when Some with (u) %}
    <span class="me" title="{{ u.display_name }}">
      {% match u.image_url %}
        {% when Some with (src) %}<img class="avatar" src="{{ src }}" alt="">
        {% when None %}
      {% endmatch %}
```

`{% match %}` / `{% when %}` is how Askama destructures an `Option`. Both arms
must be present even when one is empty — the compiler enforces exhaustiveness
just as in Rust.

`{{ u.login }}` is **HTML-escaped automatically**, because the template file
ends in `.html`. A user whose display name is `<script>` renders as text. This
is the default and you have to work to disable it.

### `index.html`

```html
{% match my_guess_label %}
  {% when Some with (g) %}
    <p class="yours locked">Your number: <strong>{{ g }}</strong></p>
    <p class="hint">That is final for this round...</p>
  {% when None %}
    <form class="guessform" method="post" action="/guess">
```

**The form is not rendered at all once a guess exists.** The rule is enforced
in the database, but not showing an input that cannot work is the difference
between a clear interface and a confusing one.

```html
<input id="guess" name="guess" type="text" inputmode="numeric"
       pattern="[0-9 ,_]*" autocomplete="off" spellcheck="false"
       placeholder="1" required>
```

`type="text"`, not `type="number"`, deliberately. `type="number"` uses a
JavaScript double internally and **loses precision above 2^53**, silently
mangling large guesses. `inputmode="numeric"` still gets the numeric keypad on
phones; `pattern` gives client-side feedback. All of it is a convenience —
`parse_guess` re-validates server-side regardless.

```html
<h1 class="countdown" data-seconds="{{ seconds_left }}">{{ time_left }}</h1>
```

Both a machine-readable attribute for the script and a server-rendered value
for the text. With JavaScript off, the page still shows a correct time
remaining; it just does not tick.

### `confirm.html`

```html
<p class="bignumber">{{ value_label }}</p>
<p class="warning">
  This is your one guess for this round and <strong>it cannot be changed
  or withdrawn</strong>...
</p>
<form method="post" action="/guess/confirm">
  <input type="hidden" name="guess" value="{{ value_raw }}">
  <input type="hidden" name="round" value="{{ round_key }}">
  <button type="submit">Yes, lock in {{ value_label }}</button>
  <a class="cancel" href="/">Cancel</a>
</form>
```

The number is shown large, because the point of the page is that you read it
before committing. The button repeats the value rather than saying "Confirm" —
the last thing you see names what will happen.

"Cancel" is a plain link, not a button: cancelling should be a GET that changes
nothing.

The hidden fields carry the state between steps, which is why the server
re-validates both.

### `results.html`

```html
{% match r.winner %}
  {% when Some with (w) %}
    <td class="who">{{ w.display_name }} <span class="login">{{ w.login }}</span></td>
    <td class="num value">{{ w.value_label }}</td>
  {% when None %}
    <td class="nowinner" colspan="2">no unique number</td>
{% endmatch %}
```

The `None` arm uses `colspan="2"` to span both columns the winner would have
filled, keeping the table rectangular. A round where everyone collided is a
real outcome and gets a real row.

```html
{% if leaders.is_empty() %}
```

Askama can call methods on values. Useful, but each call is logic living in the
template — this project keeps it to trivial cases like `is_empty` and `len`.

---

## `static/` — CSS and the script

### `app.js`

The only JavaScript in the project. Everything works without it.

```js
var left = parseInt(el.dataset.seconds, 10);
if (!isFinite(left)) return;
```

The remaining seconds come **from the server**, not from `new Date()`. A
visitor with a wrong system clock still sees a correct countdown, because the
client never computes the deadline, it only decrements. The same tick redraws
the `[####------]` bar under the countdown, using the same flat-24-hour
formula as `stats::progress_bar`, so the server-rendered bar and the script
agree.

```js
setTimeout(function () { window.location.assign('/'); }, 1500);
```

When the countdown hits zero the round has rolled, so the page reloads to pick
up the new one. The 1.5-second delay avoids a reload storm if many tabs hit
zero simultaneously.

A second block keeps the Vienna time in the header, via `Intl.DateTimeFormat`
with `timeZone: 'Europe/Vienna'`.

A third block is loading feedback, the way a command-line tool does it: a
braille spinner (`⠋⠙⠹⠸…`) in the header while the next page loads, and in the
button that sent a form. A second submit of the same form is cancelled. The
button is deliberately **not** `disabled`: a disabled button leaves its own
`name=value` out of the form, and the admin clock buttons (`name="shift"`)
depend on theirs. Changing its text is safe, because the submitted value comes
from the `value` attribute. A `pageshow` handler puts everything back when the
Back button restores a page from the browser's cache.

Written in ES5 style (`var`, `function`) with no build step. It is small
enough that a toolchain would cost more than it saves.

### `style.css`

Plain CSS, no framework. The look is a terminal's materials in a brutalist
layout: Cascadia Mono and Windows Terminal's default "Campbell" colours,
set with thick 2px rules, huge numbers and hard edges. Nothing is rounded,
nothing glows, nothing fades.

```css
:root { --bg: #0c0c0c; --fg: #cccccc; --green: #16c60c; --blue: #3b78ff; ... }
```

Green is the one accent (live, winner, the brand). Red, yellow, blue and cyan
only ever mean something: an error, a warning, the guess box, a number picked
once. The font stack starts with Cascadia Mono, which ships with Windows 11
and Windows Terminal, and falls back through Consolas, Ubuntu Mono and Menlo.
No web font is downloaded, so no third party sees a visit.

A section header (`.rule`) is its name, a thick rule running to the edge, and
an optional tag on the right, all from one flex row and a `::after`.

The guess box is blue at rest and turns green while you type in it. A number
already locked in sits in a green-bordered box with a red "sealed" label.

The chart (`.chart`, `.cols`, `.col`) is a CSS grid of `--n` equal columns,
`repeat(var(--n), minmax(0, 1fr))`. The tail under it (`.tail`) is a wrapping
row of bordered chips. In the trends page's day-by-day table, `.barcell` puts
a short bar (`.minibar`, width `--w`) beside each value, so the column reads
as a chart without hiding the number.
Each column is a `.col-track` the height of the plot and a `.col-bar` whose
height is the server-computed share (`--h`), with a gap that is a percentage
of the column so it shrinks too. Labels hang below the thick baseline and may
be wider than their column. The count shows on hover, and the whole column is
the hover target, so even an empty number reports "nobody".

Accessibility is part of the look, not an afterthought. Secondary text
(`--dim`, `#8f8f8f`) is 6:1 against the background; every colour used for text
is at least 4.5:1. Everything reachable by keyboard gets the same 3px green
`:focus-visible` ring, a "skip to content" link appears on the first Tab, and
each page has a (visually hidden) `h1`. Navigation links, the range switches,
fold-out toggles and the account button are at least 44px tall, and on touch
screens (`pointer: coarse`) the smaller controls grow to 44px too. Text stays
16px on phones, because iOS zooms into any input set smaller. The winner is
marked on the chart by a ▼ as well as by colour. The loading spinner writes
its glyphs into an `aria-hidden` span and its message once into a
`role="status"` span, so a screen reader says "loading /results" once instead
of reading every frame.

Motion is in hard steps, never fades: sections switch on one after another
(`--b` is each section's position, `--beat` the gap), the countdown's bar and
the trends page's "analysing" bar fill cell by cell, chart columns grow in six
steps (the stagger is capped, so a wide chart still finishes quickly), and a freshly sealed guess gets its label stamped on in three frames.
Under `prefers-reduced-motion` every animation is cut to effectively zero.

```css
body { min-height: 100vh; display: flex; flex-direction: column; }
main { flex: 1; }
```

Keeps the footer at the bottom of short pages without floating it up on long
ones.

---

## `migrations/` — the schema

One file, `0001_init.sql`, embedded into the binary at build time.

```sql
CREATE TABLE IF NOT EXISTS users (
    id           INTEGER PRIMARY KEY,          -- the 42 intra user id
    login        TEXT    NOT NULL UNIQUE,
    ...
);
```

`id` is the id 42 gives them, not an id this app invents — so the same person
is the same row forever, even if they change their login. (Invented test
players occupy the negative range.)

```sql
CREATE TABLE IF NOT EXISTS guesses (
    round_date   TEXT    NOT NULL,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    value        INTEGER NOT NULL CHECK (value >= 1),
    submitted_at TEXT    NOT NULL,
    PRIMARY KEY (round_date, user_id)
);
```

**`PRIMARY KEY (round_date, user_id)` is the rule "one guess per player per
round", expressed where it cannot be bypassed.** Application code can have
bugs; this constraint cannot be talked out of it, and it is what
`ON CONFLICT DO NOTHING` keys off.

`CHECK (value >= 1)` enforces the lower bound in the database as well as in
`parse_guess`. Belt and braces, and it documents the rule to anyone reading the
schema.

`ON DELETE CASCADE` removes a user's guesses with the user — active only
because the pool sets `foreign_keys(true)`.

```sql
CREATE INDEX IF NOT EXISTS idx_guesses_round_value ON guesses(round_date, value);
```

The winner query groups by exactly `(round_date, value)`. The index means that
grouping reads an ordered structure instead of sorting the table.

`0002_ghost_guesses.sql` adds the ghost column to an existing table:

```sql
ALTER TABLE guesses ADD COLUMN participates INTEGER NOT NULL DEFAULT 1;
```

The `DEFAULT 1` is what makes this safe on a live database — every row already
there was a real entry, and SQLite backfills them without a rewrite. This is
also why it is a **new file** rather than an edit to `0001`: that migration has
already run on any deployed database and will never run again.

There is deliberately **no `winners` table**. Winners are derived from
`guesses`, so there is no scheduled job, nothing to run at 12:42, and no way
for a cached result to disagree with the underlying data. The cost is
recomputing a small query per page view; the benefit is one less thing that can
be wrong.

---

## The deployment files

### `Dockerfile`

Two stages. The first has the Rust toolchain and is over a gigabyte; the second
has neither and ships at 141 MB.

```dockerfile
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && cargo build --release \
 && rm -rf src
```

The dependency-caching trick. Docker caches each instruction keyed on the files
it touched, so building a *stub* program first means the ~200 crates compile in
a layer that only invalidates when `Cargo.toml` or `Cargo.lock` changes.
Editing `src/` then rebuilds only this crate — seconds instead of minutes.

```dockerfile
COPY src ./src
RUN touch src/main.rs && cargo build --release
```

`touch` because Cargo decides what to rebuild from modification times, and the
copied file can look older than the stub build's artefacts.

```dockerfile
FROM debian:bookworm-slim
RUN apt-get install -y --no-install-recommends ca-certificates \
 && useradd --system --uid 10001 ...
```

`ca-certificates` is required. Without it the outbound HTTPS call to
`api.intra.42.fr` fails certificate verification, and sign-in breaks in a way
that looks nothing like a missing CA bundle. Minimal base images routinely omit
it.

```dockerfile
COPY --from=builder /build/target/release/i_guess_42 /usr/local/bin/i_guess_42
COPY static ./static
```

Only the binary and `static/` cross the stage boundary. Templates and
migrations are already *inside* the binary — Askama and `sqlx::migrate!` both
embed at compile time — so there is nothing else to ship.

```dockerfile
VOLUME ["/app/data"]
USER app
ENV BIND_ADDR=0.0.0.0:3000
```

The database directory is a volume, so replacing the container does not delete
everyone's guesses. `USER app` drops root: a container escape lands as an
unprivileged user. `0.0.0.0` rather than the `127.0.0.1` default, because
inside a container "localhost" is unreachable from outside it.

### `compose.yml`

```yaml
environment:
  BIND_ADDR: 0.0.0.0:3000
  DATABASE_URL: sqlite:///app/data/game.db
  SECURE_COOKIES: "true"
  FT_REDIRECT_URI: https://${SITE_DOMAIN:?set SITE_DOMAIN in .env}/auth/callback
```

These **override** `.env`, so production cannot accidentally run with
`SECURE_COOKIES=false`. The redirect URI is derived from `SITE_DOMAIN` rather
than written twice, so the two cannot disagree.

`${SITE_DOMAIN:?message}` fails the command with that message if the variable
is unset — a missing domain stops the deploy instead of producing a broken
certificate request.

```yaml
healthcheck:
  test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/3000' || exit 1"]
```

The runtime image has no `curl`, so this uses bash's `/dev/tcp` to check the
port accepts connections.

### `Caddyfile`

```
{$SITE_DOMAIN} {
	encode gzip

	reverse_proxy app:3000 {
		header_up X-Forwarded-Proto {scheme}
	}

	log { output stdout; format console }
}
```

Almost nothing, and TLS is handled. Caddy requests a Let's Encrypt certificate
for `SITE_DOMAIN` on first boot and renews it forever. `app:3000` resolves
through Docker's internal DNS to the other service, and `{$SITE_DOMAIN}` reads
the environment variable compose passes in.

`header_up X-Forwarded-Proto` tells the app it is really being reached over
HTTPS. This app does not currently need it — it builds redirect URLs from
`FT_REDIRECT_URI` rather than from request headers — so it is there for logging
and for whatever comes next. Logs go to stdout so `docker compose logs caddy`
shows certificate problems, which is where they always turn up.

This is why the app itself speaks only plain HTTP and knows nothing about
certificates.

### `deploy/`

- **`provision.sh`** — creates the EC2 instance, security group, key pair and
  Elastic IP. Idempotent: each step checks for an existing resource first, so
  re-running is safe.
- **`cloud-init.sh`** — runs once on first boot to install Docker and add a
  swap file, because a t3.micro's 1 GB of RAM is tight.
- **`deploy.sh`** — builds the image **locally** and pipes it over SSH
  (`docker save | gzip | ssh 'docker load'`). Compiling Rust on a 1 GB instance
  would fail; this needs no container registry either.
- **`teardown.sh`** — deletes everything, so the billing stops. Requires typing
  the instance id to confirm.

---

## If you want to change X

| Change | Where |
|---|---|
| The deadline time or timezone | `CUTOFF_HOUR`, `CUTOFF_MIN`, `TZ` in `round.rs` |
| Round length (e.g. weekly) | `Round::current` in `round.rs`; the `key()` format feeds the database |
| Allow changing a guess again | `insert_guess` back to `ON CONFLICT DO UPDATE`, and re-show the form in `index.html` |
| The winner rule | `WINNER_CTE` in `db.rs` — and fix the tests, which encode the current rule |
| Minimum or maximum guess | `parse_guess` in `handlers.rs` **and** the `CHECK` in the migration |
| Any user-facing wording | `notice_for` in `handlers.rs` for messages; `templates/` for page text |
| Colours and layout | the `:root` custom properties in `static/style.css` |
| Session lifetime | `SESSION_TTL_DAYS` in `auth.rs` |
| Who gets `/admin` | the `ADMIN_LOGINS` environment variable — no code change |
| Whether ghosts count | the four `participates = 1` filters in `db.rs` |
| Add a page | a handler in `handlers.rs`, a struct in `templates.rs`, a file in `templates/`, a route in `main.rs` |
| Add a column | a **new** migration file; never edit `0001_init.sql`, since it has already run on the live database |

### Things that will bite you

- **Adding a field to `base.html`** means adding it to *every* template struct,
  because the layout compiles against each child.
- **`Form<T>` must be the last handler argument.** The error does not say so.
- **Editing an applied migration** does nothing on an existing database and
  makes new deployments disagree with old ones. Always add a new file.
- **Changing the winner query** without running `cargo test` is how you ship a
  game that rewards the lowest guess instead of the lowest unique one. The
  tests exist precisely for that mistake.
