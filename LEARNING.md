# Building this yourself, 0–100

A roadmap for building this game from an empty directory, on your own, by
reading documentation and getting things wrong until they work.

It assumes you can already write basic Rust (structs, enums, `Result`,
iterators, borrowing) and have used Docker enough to run someone else's
`docker run` command. It does **not** assume you have written a web server,
touched SQL from Rust, implemented OAuth, or deployed anything.

This document deliberately does not give you the answers. It tells you what to
learn, roughly in what order, where the authoritative sources are, and which
design questions you will have to decide for yourself. The interesting part of
this project is the decisions, and handing those over would waste it.

---

## The specification

Build a website where:

- People sign in with their 42 intra account. No passwords of your own.
- Each signed-in person submits **one whole number** per round. They may change
  it until the round closes.
- Numbers start at 1 and have **no upper bound** you impose.
- A round closes every day at **12:42 Europe/Vienna**.
- The winner is the **lowest number that exactly one person chose**. A number
  two people chose is burned, however low it is. If every number is duplicated,
  nobody wins that round.
- Nobody can see anyone else's number until the round closes.
- Past rounds and their winners stay visible.

That is the whole thing. It is small enough to finish and awkward enough to
teach you something.

---

## How to work this out without asking an AI

This is the actual skill. The stages below matter less than this section.

**Read the entire error message.** The Rust compiler is unusually good. It
often prints the fix. When it prints `error[E0277]`, run
`rustc --explain E0277` for a page about that class of error.

**`cargo doc --open`.** Builds documentation for your project *and every
dependency you actually have*, at the versions you actually have. This is
better than a web search, which will hand you a different major version and
waste an hour.

**Use docs.rs, and click `source`.** Every item on docs.rs links to its source.
When documentation is thin — which it often is — reading the implementation is
usually faster than guessing. Pin the version in the URL.

**Read the crate's `examples/` directory on GitHub.** For most web and database
crates this is the real documentation. Whole working programs, kept compiling
by CI. Start there before you start from a blank file.

**Reproduce it small.** When something baffles you, make a new project with
`cargo new scratch` and rebuild just the confusing part in twenty lines. Nearly
always you find the cause while stripping things away.

**Read the specification, not a blog post about it.** For anything with an RFC
or a vendor API document, go to the source. Blog posts are abbreviated, often
outdated, and frequently wrong about the security-relevant details.

**`cargo tree`** when two crates disagree about a shared dependency, and
`cargo tree -i <crate>` to find who pulled something in.

**Search the crate's GitHub issues** before concluding you are stupid.
Sometimes you have found a real bug, or a known sharp edge.

**Ask humans**: <https://users.rust-lang.org> is welcoming and fast, if you
bring a minimal reproduction and say what you already tried.

**Know the difference between stuck and thrashing.** Stuck is when you do not
understand something — keep reading, you are learning. Thrashing is changing
things at random hoping the error goes away. When you notice thrashing, stop,
and go write the smallest program that demonstrates the problem.

---

## Stage 0 — Shore up the Rust you will lean on

You do not need to be an expert. You do need to be comfortable with a handful
of things this project uses constantly, and being shaky on them will feel like
the *web framework* is confusing when actually it is the language.

Make sure you are solid on:

- `Result`, `Option`, the `?` operator, and writing your own error type
- Traits, trait bounds, and what `impl Trait` means in argument and return
  position
- Lifetimes — enough to read `&'a str` in a signature without flinching
- `String` vs `&str`, `Vec<T>` vs `&[T]`, and when you need `.to_string()`
- Closures, and why some of them need `move`
- Modules and visibility: `mod`, `pub`, `use`, `crate::`

Then learn **async**, because every web framework in Rust is async and it is
genuinely a different mental model:

- What `async fn` actually returns, and why nothing happens until you `.await`
- What an executor/runtime is and why you need Tokio
- Why holding a lock across an `.await` is a trap

| Resource | Use it for |
|---|---|
| [The Rust Book](https://doc.rust-lang.org/book/) | Chapters 8–10, 13, 17. Re-read error handling and traits. |
| [Rust by Example](https://doc.rust-lang.org/rust-by-example/) | Quick reminders with runnable code |
| [Tokio tutorial](https://tokio.rs/tokio/tutorial) | The best practical async introduction. Do it properly, top to bottom. |
| [Async Book](https://rust-lang.github.io/async-book/) | When you want to know *why* rather than *how* |
| [Rust Atomics and Locks](https://marabos.nl/atomics/) | Free online. Only if concurrency starts biting you. |

**Do not skip the Tokio tutorial.** Almost every confusing thing that happens
in stages 1–7 is an async thing wearing a web framework costume.

---

## Stage 1 — A web server that answers

**Goal:** `curl localhost:3000` prints something you wrote.

Pick a framework. `axum`, `actix-web`, `poem` and `rocket` are all reasonable.
Whichever you choose, commit to it and read *its* documentation rather than
mixing tutorials.

Learn:

- What a route is, and how the framework maps a path and method to a function
- How the framework gets data out of a request and into your function's
  arguments — the extractor/guard pattern. This is the central abstraction and
  it will feel like magic until it doesn't.
- How your function's return value becomes an HTTP response
- Shared application state, and why it must be cheap to clone
- Middleware/layers, and what `tower` is if you chose axum

**Questions you must answer yourself:**

- Where does configuration come from, and what should happen at startup if a
  required secret is missing?
- What should a handler return when something goes wrong halfway through? You
  will want one error type that converts into a response. Work out how.

**Where to look:** the framework's docs.rs page, and its `examples/` directory
on GitHub. For axum specifically, the examples directory is excellent and
covers most of what this project needs.

**The trap:** extractor ordering rules. Most frameworks require the
body-consuming extractor to come last, and the error message when you get it
wrong is famously unhelpful — it will complain about an unsatisfied trait bound
rather than saying "move this argument". Learn to recognise it.

---

## Stage 2 — HTML that the server renders

**Goal:** your page has a heading, a form, and a stylesheet, and none of it is a
`format!` call.

Options: compile-time templates (`askama`, `maud`) or runtime templates
(`tera`, `minijinja`). Compile-time means typos become build errors; runtime
means you can edit HTML without recompiling. Both are defensible. Pick one and
understand the trade you made.

Learn:

- Template inheritance — one layout, many pages
- Loops and conditionals in your chosen template language
- **Auto-escaping**: what it protects you from, and when it is not applied
- How to serve static files (CSS, JS) from the framework

**Questions you must answer yourself:**

- If a user can put text into your database, and that text ends up on a page,
  what stops them putting `<script>` in it? Find out what your template engine
  does by default, and verify it rather than assuming.
- Should the page work with JavaScript disabled? Decide deliberately.

**Where to look:** [MDN's HTML forms guide](https://developer.mozilla.org/en-US/docs/Learn/Forms)
for the form side. Your template crate's docs for the rest. Read about
[XSS on the OWASP cheat sheet](https://cheatsheetseries.owasp.org/cheatsheets/Cross_Site_Scripting_Prevention_Cheat_Sheet.html).

---

## Stage 3 — The rules, as pure logic, with tests

**Goal:** functions that decide which round it is and who won, with tests, and
no web server or database anywhere near them.

Do this stage *before* touching a database. It is the most valuable habit in
the whole project: the rules are the part that must be right, and they are far
easier to get right when they are pure functions you can call from a test.

Learn:

- `#[cfg(test)]`, `#[test]`, `assert_eq!`
- How to write a test that documents an edge case rather than restating the
  implementation
- Date and time handling: `chrono` and `chrono-tz`, or the standard library
  plus a timezone crate
- The difference between an *instant* (a point on the timeline) and a *civil
  date-time* (what a wall clock says somewhere)

**The two problems worth thinking hard about:**

### What is a "day"?

Rounds close at 12:42 Europe/Vienna. That is not midnight, and Vienna is not
UTC, and Vienna changes offset twice a year.

- If a round runs from 12:42 to 12:42, which calendar date *names* it? There
  are two defensible answers. Pick one, write it down, and be consistent — this
  decision leaks into your database, your URLs and your UI.
- At exactly 12:42:00, is the round open or closed? Half-open intervals exist
  for a reason.
- What happens on the two days a year when the offset changes? Convince
  yourself with a test using a real date, not by reasoning about it.
- Never ask the operating system for "today" in local time. Get an instant in
  UTC, and convert deliberately to the zone you mean.

Read [Falsehoods programmers believe about time](https://gist.github.com/timvisee/fcda9bbdff88d45cc9061606b4b923ca)
first, for humility. Then the [IANA tz database](https://www.iana.org/time-zones)
overview so you know where the rules come from.

### Lowest *unique*, not lowest

"Lowest number nobody else picked" is not "smallest number". If two people pick
1, then 1 is out entirely and 2 might win. Work out how you would express that
as a query or an algorithm. Then handle:

- Nobody wins, because every number was duplicated
- One player, who trivially wins with whatever they chose
- Someone changing their guess, which can *un-burn* a number for someone else

Write a test for each of those before you write the logic. They are cheap tests
and they will all fail interestingly at least once.

**Questions you must answer yourself:**

- The numbers are unbounded. What type holds them? What is your actual ceiling,
  and what happens when someone pastes 400 digits into the form? Whatever you
  choose, decide what the error message says.

---

## Stage 4 — Persistence

**Goal:** guesses survive a restart.

SQLite is the right default here: one file, no server, and this game has one
writer at a time. Postgres is the right choice if you want to learn the thing
you will meet at work. Either is fine; SQLite is less to set up.

Learn:

- Enough SQL to be dangerous: `SELECT`, `INSERT`, `JOIN`, `GROUP BY`,
  `HAVING`, aggregate functions
- The difference between `WHERE` and `HAVING` — this project needs it
- Primary keys, composite primary keys, foreign keys, and `UNIQUE`
- Indexes: what they cost, what they buy
- Migrations: why schema changes belong in version-controlled files rather than
  in your head
- A Rust SQL library — `sqlx`, `diesel` or `rusqlite`. `sqlx` writes real SQL
  and checks it; `diesel` gives you a typed query builder. Different
  philosophies, both good.
- Connection pooling: what a pool is and why you do not open a connection per
  request

**Questions you must answer yourself:**

- How do you store "this person's guess for this round" such that submitting
  twice *replaces* rather than duplicates? There is a SQL feature for exactly
  this. Find it.
- Do you compute the winner when the round closes and store it, or work it out
  every time someone asks? Both are real designs with real trade-offs. Storing
  it means something must run at 12:42 — what, and what if the server was
  asleep? Computing it means a slightly heavier query. Decide, and be able to
  say why.
- How do you keep an open round's guesses secret? This is an access-control
  question, not a database one, and it is easy to leak it through a page you
  forgot about.

| Resource | Use it for |
|---|---|
| [SQLite documentation](https://www.sqlite.org/docs.html) | The reference. The "quirks" page is genuinely important. |
| [Use The Index, Luke!](https://use-the-index-luke.com/) | Free, excellent, teaches how indexes actually work |
| [SQLBolt](https://sqlbolt.com/) | Interactive, an hour, worth it if `GROUP BY` is new |
| your SQL crate's `examples/` | How to wire it to Rust |

**The trap:** if you use SQLite, learn what WAL mode is and what it means for
copying the database file while the app is running. You will care about this
the first time you try to take a backup.

---

## Stage 5 — Signing in with 42

**Goal:** clicking a button sends you to intra, and you come back knowing who
the visitor is.

This is the stage where you should slow down and read a specification, because
the failure modes are security failures rather than crashes.

Learn:

- The OAuth2 **authorization code flow**, end to end, before writing anything.
  Draw it. Who redirects whom, what travels in the URL, what travels in a POST
  body, what must never appear in a URL.
- Why the authorization code is exchanged **server-side** for a token, and why
  your client secret must never reach the browser.
- The `state` parameter: what attack it prevents, why it must be
  unguessable, and why it must be single-use.
- Scopes, and asking for the least you need.

**Questions you must answer yourself:**

- Where do you keep the `state` between the redirect out and the redirect back?
  It has to survive a round trip through someone else's website.
- What happens if someone hits your callback URL directly with a made-up code?
  Or replays a real callback URL twice?
- The redirect URI has to match what you registered, exactly. What does that
  mean for running the same code on localhost and in production?

| Resource | Use it for |
|---|---|
| [OAuth 2.0 Simplified](https://aaronparecki.com/oauth-2-simplified) | Read this first. Clear, correct, free. |
| [RFC 6749](https://datatracker.ietf.org/doc/html/rfc6749) | The actual specification, for when "Simplified" is ambiguous |
| [42 API documentation](https://api.intra.42.fr/apidoc) | Endpoints, scopes, and what the user object contains |
| [oauth.net](https://oauth.net/2/) | Overview and links to the surrounding specs |

Register your application at
<https://profile.intra.42.fr/oauth/applications/new>. You get a UID and a
secret. The secret goes in an environment variable, never in git.

**The trap:** you will get a `redirect_uri_mismatch` at least once. It is
usually a trailing slash, `http` vs `https`, or `localhost` vs `127.0.0.1`.

---

## Stage 6 — Staying signed in

**Goal:** the site remembers you across page loads, and "sign out" works.

Learn:

- How cookies actually work: `Set-Cookie`, expiry, `Path`, `Domain`
- The flags, and precisely what each defends against: `HttpOnly`, `Secure`,
  `SameSite`
- **Session tokens versus signed/encrypted cookies.** Two legitimate designs:
  store a random opaque token and look the session up server-side, or put
  signed state in the cookie itself. Understand what each costs — especially,
  how you revoke a session under each.
- Where your randomness comes from. Not `rand::random()` on a whim — find out
  which generator is cryptographically secure and why it matters here.
- CSRF: what it is, and how `SameSite` relates to it. Your form that submits a
  guess is a state-changing POST, so this is your problem now.

**Questions you must answer yourself:**

- How long should a session last, and what actually happens when it expires?
- Sessions accumulate forever unless something removes them. What removes them?
- When someone signs out, is the session dead *server-side*, or have you only
  deleted the cookie? These are very different.

| Resource | Use it for |
|---|---|
| [OWASP Session Management Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html) | The checklist. Read it twice. |
| [OWASP CSRF Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html) | Decide deliberately what you rely on |
| [MDN: Set-Cookie](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Set-Cookie) | The mechanics |
| [MDN: SameSite](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Set-Cookie/SameSite) | Lax vs Strict vs None |

---

## Stage 7 — Make it not embarrassing

**Goal:** you cannot break your own site in five minutes of trying.

Go and attack it. Genuinely try. Submit an empty form. Submit `-1`, `0`, `3.5`,
`abc`, `1e9`, four hundred digits, emoji. Submit twice quickly. Submit one
second before the deadline. Open two browsers. Sign out in one and click around
in the other. Edit the hidden field in the form. Request a page that does not
exist.

Then decide, for each, what *should* happen — and make it happen.

Learn:

- Input validation at the boundary, and parsing rather than checking
- What your framework does with a 404 by default, and how to replace it
- The POST/redirect/GET pattern, and why refreshing after a form submission
  should not resubmit it
- Structured logging (`tracing`), and what you must never log — secrets, session
  tokens, anything you would not paste in a public channel
- Never showing internal error detail to the browser, while keeping it in your
  logs

**Questions you must answer yourself:**

- A user submits a guess at exactly the moment the round rolls over. Which
  round does it land in, and does the user find out?
- If someone forges the hidden round field in your form, what happens? The
  general lesson: never trust the client for anything that determines meaning.

---

## Stage 8 — Put it in a container

**Goal:** `docker run` starts your app on a machine with no Rust installed.

Learn:

- **Multi-stage builds.** A Rust build image is over a gigabyte; your runtime
  image should be a fraction of that. Build in one stage, copy the binary into
  a clean one.
- Layer caching, and how to order your Dockerfile so that editing `main.rs`
  does not recompile every dependency. This is the single biggest quality-of-
  life difference in a Rust Dockerfile.
- Running as a **non-root** user, and why.
- Volumes: which directory holds your database, and what happens to it when the
  container is replaced. Get this wrong and you delete everyone's data on your
  first update.
- `ENTRYPOINT` vs `CMD`, environment variables, `EXPOSE`
- Compose, for running your app alongside other containers

**Questions you must answer yourself:**

- Which files does your binary need at *runtime*, as opposed to at build time?
  Templates and migrations may or may not be baked into the binary depending on
  your choices in stages 2 and 4. Find out for yours, and only ship what is
  needed.
- Your app binds an address. What must that be inside a container, and why is
  it not what you use locally?
- Does your runtime image trust TLS certificates? A minimal base image may have
  no CA bundle, and your outbound HTTPS calls to the 42 API will fail in a
  confusing way.

| Resource | Use it for |
|---|---|
| [Dockerfile reference](https://docs.docker.com/reference/dockerfile/) | Every instruction |
| [Multi-stage builds](https://docs.docker.com/build/building/multi-stage/) | The core technique here |
| [Compose file reference](https://docs.docker.com/reference/compose-file/) | Services, volumes, networks |

---

## Stage 9 — Put it on the internet

**Goal:** a stranger can visit a URL and play.

Learn:

- DNS: what an **A record** is, what TTL means, and why your change did not
  take effect yet. `dig` is your friend.
- TLS: why you need it, and how a certificate authority decides you own a
  domain. Read how the ACME challenge actually works — it explains every
  failure you will hit.
- **A reverse proxy.** Caddy obtains and renews certificates automatically and
  is by far the least painful starting point; nginx and Traefik are the
  alternatives. Understand what the proxy adds: TLS termination, and a stable
  front door for an app that only speaks plain HTTP.
- A server to run it on. EC2 or Lightsail on AWS, or Hetzner, or a Raspberry Pi
  on your desk. The application does not care.
- Firewalls / security groups: which ports are open, to whom. SSH open to the
  entire internet is a bad habit even when the key is strong.
- `cloud-init` or equivalent, so a fresh machine configures itself instead of
  you remembering what you typed.

**Questions you must answer yourself:**

- Your machine's public IP can change when it restarts. What does that do to
  DNS, and how do you stop it happening?
- Certificate issuance requires the CA to reach your server *by name* over the
  public internet. What does that imply about the order of your steps?
- The tiny free-tier instances have very little RAM. Can you compile Rust on
  one? If not, where does the build happen and how does the result get there?
- What is your backup? "The database is on a disk attached to one machine" is
  not a backup. How do you get a copy off, and have you tried *restoring* it?

| Resource | Use it for |
|---|---|
| [Let's Encrypt: How It Works](https://letsencrypt.org/how-it-works/) | Read before debugging certificates |
| [Caddy documentation](https://caddyserver.com/docs/) | Automatic HTTPS with almost no config |
| [AWS EC2 User Guide](https://docs.aws.amazon.com/ec2/) | Instances, security groups, elastic IPs |
| [cloud-init docs](https://cloudinit.readthedocs.io/) | First-boot configuration |

**Costs are real.** Understand what you are being billed for before you start
an instance, and know how to destroy everything you created. Set a billing
alarm on day one.

---

## Stage 10 — The things you only learn by running it

Once it is live and people use it:

- Watch the logs during a real 12:42. Something will surprise you.
- Take a backup. Then restore it into a fresh environment and confirm the data
  is really there. An untested backup is a rumour.
- Write down how to deploy a change, then follow your own notes. They will be
  wrong the first time.
- Think about what happens if two people submit at the same instant, and
  whether your database actually prevents what you think it prevents.

---

## The genuinely hard parts

If you only think carefully about five things, make it these:

1. **Round boundaries.** Not midnight, not UTC, and the offset changes twice a
   year. Get this wrong and the game is subtly broken for months before anyone
   can articulate why.
2. **Lowest unique.** The query is short and very easy to write incorrectly in
   a way that looks right on your three test rows. Test it with duplicates.
3. **Derived versus stored state.** Deciding whether winners are computed or
   recorded shapes everything else. There is no scheduler in this project's
   design — work out whether you want one.
4. **The OAuth `state` parameter.** Everything else in the flow fails loudly.
   This one fails silently and only matters when someone attacks you.
5. **Secrets and git.** One `.gitignore` line stands between your client secret
   and the internet. Add it before your first commit, not after.

---

## How to know you have finished

- Somebody who is not you signs in and plays without asking you a question.
- You can state, without looking, what happens at 12:42:00 exactly.
- Your tests fail when you deliberately break the winner logic.
- You can restore the database onto a new machine from a backup.
- You can destroy the whole deployment and rebuild it from your own notes.
- `git log` shows the project, and no secret has ever been committed.

---

## Then make it yours

The version in this repository stops at the specification above. Obvious next
problems, roughly in order of difficulty:

- A page showing your own history: every guess you have made and how close you
  came
- Statistics once a round closes — the distribution of guesses, which numbers
  were burned
- Email or Slack notification when you win
- Rate limiting, so one person cannot hammer the submit endpoint
- Move from SQLite to Postgres, and find out what that does to your queries
- Run two instances behind a load balancer, and discover why SQLite made that
  hard
- Replace the server-rendered pages with an API and a frontend, and form your
  own opinion on whether that was an improvement

---

## Reading list

**The one book to buy if you buy one:**
[Zero To Production In Rust](https://www.zero2prod.com/) by Luca Palmieri. It
builds a real web service in Rust with a database, migrations, a serious
testing strategy, and deployment. It uses a different web framework than you
may pick, and it does not matter — the structure, testing and deployment
chapters map onto this project almost exactly. It is the closest thing to a
guided version of what you are attempting.

**Free and worth your time:**

- [The Rust Book](https://doc.rust-lang.org/book/) — the foundation
- [Tokio tutorial](https://tokio.rs/tokio/tutorial) — async, properly
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) — how to
  design types other people can use, including future you
- [OWASP Cheat Sheet Series](https://cheatsheetseries.owasp.org/) — the web
  security checklist, free and authoritative
- [Use The Index, Luke!](https://use-the-index-luke.com/) — SQL performance
- [MDN Web Docs](https://developer.mozilla.org/) — HTTP, cookies, HTML, CSS
- [Jon Gjengset's YouTube channel](https://www.youtube.com/@jonhoo) — long,
  unhurried, deep Rust. Watch when you want to understand rather than ship.
- [This Week in Rust](https://this-week-in-rust.org/) — stay current

**When you are stuck:**

- <https://users.rust-lang.org> — bring a minimal reproduction
- The crate's GitHub issues
- `rustc --explain E0499` and friends

---

Take longer than you think you should on stages 3 and 5. The rest is typing.
