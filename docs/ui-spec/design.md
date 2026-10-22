# i guess low — UI/UX · Design

**Spec:** `ui-ux-improvements` · **Version:** 2.0 · **Implements:** `requirements.md` R1–R24
**Constraint:** no changes to game rules, scoring or the persistent data model. Stack not assumed; names are descriptive.
Every decision carries a **Why** so it can be overruled with intent.

## 1. Phases

| Phase | Requirements | Theme | Why this order |
| --- | --- | --- | --- |
| 0 | R1–R5 | Identity and trust | Legal gap + things a first visitor sees; must land before v1.6.2 ships. |
| 1 | R16, R21–R23 | Colour, copy, character | The "soul" regression is unshipped — fix it before anything new is built on top of the wrong tokens and copy. |
| 2 | R6–R8, R10 | First guess, round close | Activation: turns arrivals into players. |
| 3 | R9, R11–R14, R20 | Results, trends, navigation | Retention + growth loop; builds on the new tokens and copy voice. |
| 4 | R15, R17–R19, R24 | Legal, a11y, responsive, platform | Mostly parallel; R24 gates every phase. |

## 2. Decisions

### D1 · Server-authoritative timing (R2, R10)
- Payload carries `round.closesAt` (ISO 8601) and `serverNow`; client keeps `offset = serverNow − Date.now()`, re-computed on load and `visibilitychange`.
- Visible text ticks per second; `aria-label` and progress update once per minute.
- At `remaining ≤ 0` → `round closed · revealing…`, poll for results.
- **Why:** client clocks drift and skew across timezones/DST; the real cutoff is the server's. Once-a-minute ARIA updates avoid screen-reader noise.

### D2 · Environment flag (R1, R9.6)
- `ENVIRONMENT = test | production` drives the `[test instance]` line (`--env`), imprint variant and `test details` block.
- **Why:** one switch prevents test-only text leaking into production.

### D3 · Held guess (R7)
- Logged-out submit → validate → store `{roundId, number}` in the server session → sign-in with return target.
- Callback: same round + open + no existing guess → lock via D4, else show the matching R22 message; always clear.
- **Why:** session (not URL/localStorage) keeps the number off shared links and survives the OAuth round-trip; reusing D4 means one lock path to test.

### D4 · Idempotent locking (R10)
- Idempotency per `(user, round)`; repeats return the existing guess with 200. Late requests get a distinct "closed" response.
- Never unique on `(round, number)`.
- **Why:** double-clicks and retries must be harmless; identical numbers are the game's core mechanic, so they must all persist.

### D5 · Player stats (R14)
- Computed on read from guesses + results; streak counts consecutive closed rounds with a guess, open round counts only if guessed.
- **Why:** no schema change (non-goal), and the data already exists.

### D6 · Usernames in public views (R9.2–R9.3)
- Results, day pages, leaderboards and share previews show the account username.
- SSO accounts without a chosen username get one prompt after first sign-in (prefilled from the provider handle, editable).
- Privacy page states usernames are public.
- **Why:** decision — privacy is not a goal for this version, and names make wins social. The prompt exists because SSO handles may be real names users didn't intend to publish; one editable step keeps that their choice.

### D7 · Colour tokens and roles (R21)
```css
:root{
  --bg:#0c0c0c; --fg:#c4c4c4; --hi:#ffffff; --dim:#6e6e6e;
  --edge:#262626; --panel:#121212;
  --action:#3b78ff; --action-hi:#5a8cff;
  --state:#16c60c; --loss:#e74856; --env:#c19c00;
}
a,.nav a{color:var(--action);text-decoration:underline 1px;text-underline-offset:3px}
a:hover,.nav a:hover{background:var(--action-hi);color:var(--bg);text-decoration:none}
.btn.primary{background:var(--action);border-color:var(--action);color:#fff;font-weight:700}
.btn.primary:hover{background:var(--action-hi);border-color:var(--action-hi)}
:focus-visible{outline:3px solid var(--action);outline-offset:3px}
.nav a.on{background:var(--hi);color:var(--bg);text-decoration:none}
.status-open,.chip-low,.win,.progress-fill{color:var(--state)}
.loss,.taken,.err{color:var(--loss)}
```
- Rename existing `--green`/`--blue`/`--red` to role names; delete unused aliases after migration.
- **Why role names, not hue names:** `--state` can't accidentally end up on a button the way `--green` did; the name carries the rule.
- **Why the grey ramp:** #fff/#c4c4c4/#6e6e6e gives three clearly separated priorities; the old #8f8f8f read as "disabled".
- **Why the nav-active stays white:** location is state; blue is reserved for "do something".

| Element | Token | Why |
| --- | --- | --- |
| primary button, links, sign-in, focus | `--action` | clickable = blue, everywhere |
| open badge, "low" chip, progress, wins | `--state` | outcome/status = green |
| taken numbers, losses, errors | `--loss` | negative state |
| test line | `--env` | environment only |
| active nav | `--hi` inversion | location, not action |
| meta labels | `--dim` | quiet but short (contrast ~3.9:1) |

### D8 · Copy system (R22)
- All UI strings live in one copy file/table (the R22.6 table is the source).
- Pattern: `<state>: <value> · <value> · <link →>`; lowercase; `·` separates, `→` marks a link, `↓` points to the input.
- Privacy and imprint are exempt; they keep full sentences.
- **Why one file:** keeps the register consistent and makes future copy passes a single diff. **Why the pattern:** a fixed grammar is what makes terminal output feel coherent.

### D9 · Personality beat (R23)
```css
.guess-field{position:relative}
.guess-field::after{content:"▊";color:var(--action);animation:blink 1s steps(1) infinite}
@keyframes blink{50%{opacity:0}}
@media (prefers-reduced-motion:reduce){.guess-field::after{animation:none}}
```
- Hide the pseudo-cursor while the input has focus and a native caret is shown.
- **Why on a wrapper, not the input:** `::after` doesn't render on `<input>`. **Why hide on focus:** two carets read as a glitch.

### D10 · Progress bar (R3)
- Keep the existing `role="progressbar"` element; render ~30 cells: filled `█` in `--state`, empty `░` in `--edge`; percentage label in `--dim`.
- **Why:** already accessible (observed), keeps the green the live site uses, looks terminal not web-widget.

### D11 · Navigation (R20)
- Routes: `/g/`, `/g/results`, `/g/trends?days=7|30|all`, `/g/day/<date>`, new `/g/me`.
- Under 641 px: one row (`i guess low` · tabs · `sign in`); if four tabs don't fit at 320 px → fixed bottom bar, mono, 44 px, white-inversion active.
- Day header: `← 2026-10-20   round 2026-10-21 [closed]   2026-10-22 →`.
- **Why:** reuses existing routes (less churn), and prev/next turns dead-end pages into a browsable history.

### D12 · Trends performance (R9.7)
- Precompute per range on round close (or cache server-side); remove the client-side analysis delay; drop the "done in X ms" line from the main view.
- **Why:** observed ~4 s blank page; the reported timing was wrong and is telemetry anyway.

### D13 · Calendar, share, OG (R5, R8, R12)
- `.ics`: `DTSTART` = `closesAt` UTC, 5 min, `SUMMARY:i guess low · reveal`, `URL:/g/day/<date>`, stable `UID`.
- OG image per round generated at close, cached at `/g/day/<date>/og.png`; site-wide static card for other pages.
- Share text built client-side; never includes own number; `navigator.share` → clipboard fallback.
- **Why:** calendar replaces email reminders with zero infrastructure; spoiler-safe share text keeps the next round fair.

## 3. Page layouts

### 3.1 Round page (R4, R6, R8, R14, R21–R23)
```
[test instance] not the live game                       ← --env, test only
i guess [low]   round · results · trends · me     20:21 vienna   sign in
lowest unique number wins                                ← hero, --hi

┌ round 2026-10-21 · [open]   closes 12:42 vienna ┐
│ closes in  16h 20m                               │
│ █████████░░░░░░░░░░░░░░░░░░░░  31%               │
│ 1 player this round           not playing yet ↓  │
└──────────────────────────────────────────────────┘
[ 7▊ ]  [ lock it in ]                                   ← one blue block
01  one whole number above 0. once a day.
02  all numbers revealed at 12:42.
03  lowest number only one person picks wins.
    match anyone → you both lose it.
yesterday  17  kai won · 273 played → every pick
```
States: logged out · logged in · `locked: 7 · reveal 12:42 · 16h 20m · add to calendar` · `round closed · revealing…`.

### 3.2 Day page (R11, R12, R20)
```
← 2026-10-19        round 2026-10-20 [closed]        2026-10-21 →
2026-10-20 · 273 played · kai won with 17
[histogram: taken in red, 17 green, your pick marked]
view as table · share
play today →
```

### 3.3 Results (R9, R20)
`recent rounds` list (date · winner · players → day) → `most days won` (≤10, usernames) → `show all 213`.

### 3.4 Trends (R13)
`7 rounds · median winner 9 · most crowded: 1` → `[7d] 30d all` → compact charts → `10-15→10-17 vs 10-18→10-21` deltas.

### 3.5 Auth (R7, R15.3, R17.4)
SSO buttons (outlined `--action`) + one-line provider notes → `or` → email/password → error summary in `--loss`.

### 3.6 Legal (R1, R15)
Mono, line height 1.6, ≤72ch, headed sections; tables stack below 641 px.

## 4. Testing

| Area | Tests | Why |
| --- | --- | --- |
| Tokens (R21) | computed-style check: no button/link uses `--state`; focus is `--action` | colour roles regress silently |
| Copy (R22) | string audit against the R22.6 table; no em-dash outside legal pages | register drifts one string at a time |
| Timing (D1) | clock skew ±5 min; hidden tab 10 min; DST dates | correctness of the cutoff |
| Locking (D4) | double-click, ±1 s around close, 50 concurrent identical numbers | trust |
| Held guess (D3) | success, cancel, close mid-sign-in, already guessed | activation path |
| Usernames (D6) | SSO first sign-in prompt; names on results/day/share | new public data |
| Cursor (D9) | reduced motion → static; focus → hidden | motion accessibility |
| Responsive | 320 / 375 / 768 / 1280; primary action above fold at 375 | small screens |
| Verification (R24) | reload preview, confirm new tokens in computed styles before review | no reviews of stale builds |

## 5. Open questions

1. Production imprint name and contact email (R1.2).
2. Retention periods per data category (R15.1).
3. Confirm no minimum-age statement is needed for your legal basis.
4. Should existing SSO users be prompted for a username retroactively, or only new sign-ups? (D6)
5. Data-source field to tell test vs live data (R9) — still unverified in code.
