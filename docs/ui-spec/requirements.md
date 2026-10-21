# i guess low — UI/UX · Requirements

**Spec:** `ui-ux-improvements` · **Version:** 2.0 · **Date:** 2026-10-20
**Sources:** v1.1 of this spec + *v1.6.2 "soul" fix spec* + *UI Refinement Spec*, with approved decisions (below).
**Format:** each requirement has a user story, a **Why** (the principle, so it can be overruled with intent), and acceptance criteria in EARS notation.
**Baseline:** local build checked 2026-10-20 20:21. Items marked *(observed done)* only need verification.

## What changed from v1.1

| Topic | v1.1 | v2.0 | Why |
| --- | --- | --- | --- |
| Primary button colour | green | **blue** (`--action`) | One hue per meaning: blue = do something, green = state/win. A green button reads "done", not "press me". |
| Blue elsewhere | removed as off-palette | **blue for links, sign-in, focus** | Blue is the live site's cool accent; without it the palette is all warm-neutral and reads flat. |
| Copy register | full sentences | **lowercase fragments** | The terminal frame promises terse, declarative text; sentences read like a help doc in costume. |
| Strategy hint | visible paragraph | **deleted** | Explaining the strategy spoils the tension the game should make players feel. |
| Rules | 3 rules + split tie line | **3 lines, tie indented under 03** | Three points scan at a glance; ties explained once, where they apply. |
| Player identity | stable anonymous labels | **real usernames** | Decision: privacy isn't a goal for this version; names make results human. |
| Grey levels | unchanged | **#fff / #c4c4c4 / #6e6e6e** | Current greys are ~15 points apart and read as one flat priority. |
| Personality | none | **blinking blue cursor in the input** | One deliberate detail against restraint is how terminal UIs get character. |
| Progress bar | mono blocks | **green mono blocks** | Matches the live site; an open round's progress counts as state. |

## Glossary

| Term | Meaning |
| --- | --- |
| Round | One daily game; guesses accepted until the close time (12:42 Europe/Vienna). |
| Close time | Server-authoritative deadline of the current round. |
| Held guess | A number entered by a logged-out visitor, kept in their session until they sign in. |
| Locked guess | A guess stored server-side for a user and round. |
| Day page | Per-round page at `/g/day/<YYYY-MM-DD>`. |
| `--action` / `--state` | Blue = anything clickable or focused. Green = status, brand chip, progress, wins. |
| Fragment | Lowercase phrase without full sentence structure, e.g. `locked: 7 · reveal 12:42`. |

## Non-goals

- Game rules, scoring, backend data model. **Why:** this is a presentation spec; rule changes need their own review.
- Email reminders, push notifications. **Why:** not approved; `.ics` covers the reminder need with no infrastructure.
- Changing the live player count. **Why:** explicitly declined.
- A second (sans-serif) font. **Why:** monospace everywhere is the core of the terminal identity.
- Features that exist only on the live site. **Why:** refinement spec — match the feel, not the feature list.
- Minimum-age statement (unless legal review requires it).

---

## Phase 0 — Identity and trust (ship before v1.6.2 goes live)

### R1 · Imprint
**Story:** As a visitor, I want to know who runs the site.
**Why:** Austrian §5 ECG / §25 MedienG require it, and "not given" reads as abandoned.

1. WHILE on a test instance, THE SYSTEM SHALL show `operator details not shown on test instance`.
2. WHILE in production, THE SYSTEM SHALL show operator name/team and contact email in one line under "impressum".
3. THE SYSTEM SHALL NOT show "not given" anywhere.
4. THE imprint page SHALL have exactly one `h1`.

### R2 · Countdown and round status *(mostly observed done)*
**Story:** As a player, I want to know when the round closes.
**Why:** The countdown is the heartbeat of the game; it must be correct and readable, but secondary to the action.

1. THE countdown SHALL be labelled `closes in` and show a duration (`16h 20m`).
2. THE close time and timezone SHALL sit beside it (`closes 12:42 vienna`).
3. THE countdown SHALL derive from a server-provided close timestamp and re-sync on `visibilitychange`.
4. THE countdown SHALL have an `aria-label` and SHALL NOT use `aria-live`. **Why:** a per-second live region floods screen readers.
5. THE countdown SHALL fit without overflow at 320 px.
6. Round state, close time, countdown and progress SHALL form one status block.
7. THE round date SHALL be the round that closes next. *(Observed fixed: 2026-10-21 shown at 2026-10-20 20:21, 16h 20m left.)*

### R3 · Progress bar *(observed done; colour confirmed)*
**Why:** Block glyphs keep both the accessible semantics and the terminal look; a smooth web bar reads as a generic widget.

1. THE progress bar SHALL expose `role="progressbar"`, `aria-valuenow`, `aria-valuetext`.
2. THE bar SHALL render as monospace blocks: filled in `--state` green, empty in `--edge`.
3. THE bar SHALL update at most once per minute and render fully at 320 px.

### R4 · Single primary action
**Story:** As a first-time visitor, I want one obvious thing to do.
**Why:** Three "log in to play" buttons diluted each other. The solid blue block should mean "the one thing to do" — recognisable by shape even in greyscale.

1. EACH page state SHALL have exactly one solid-filled `--action` button.
2. AT 375 × 812 px THE number input and primary button SHALL be visible without scrolling.
3. Other routes to the same action SHALL be text links.

### R5 · Link previews and icons
**Why:** Growth is link-pasting into group chats; a preview card turns grey text into an invitation.

1. Public pages SHALL emit `og:title`, `og:description`, `og:image` (1200×630), `og:url`, `og:type`, and `twitter:card=summary_large_image`.
2. `og:description` SHALL be a fragment: `lowest unique number wins. one guess a day.`
3. THE SYSTEM SHALL serve `favicon.svg` and `apple-touch-icon.png`; `/favicon.ico` SHALL NOT 404.

---

## Phase 1 — Colour, copy and character

### R21 · Colour roles
**Story:** As a player, I want to tell at a glance what I can click and what is just information.
**Why:** Each colour gets exactly one meaning. When green meant state, brand *and* action, it signalled none of them. Blue is the universal "interactive" convention and restores the warm↔cool tension of a real terminal.

1. THE SYSTEM SHALL define tokens: `--bg #0c0c0c`, `--fg #c4c4c4`, `--hi #ffffff`, `--dim #6e6e6e`, `--edge #262626`, `--panel #121212`, `--action #3b78ff`, `--action-hi #5a8cff`, `--state #16c60c`, `--loss #e74856`, `--env #c19c00`.
2. Links, nav links, sign-in links, the primary button and `:focus-visible` SHALL use `--action`. **Why focus too:** if action is blue but focus is green, two hues mean "interactive".
3. Status ("open"), the "low" brand chip, the progress fill and win numbers SHALL use `--state`.
4. No button or link SHALL use `--state` as its fill or text colour.
5. Loss/taken states and errors SHALL use `--loss`; only the test-instance notice SHALL use `--env`.
6. THE active nav tab SHALL stay a white inversion (`--hi` background, `--bg` text). **Why:** "you are here" is state, not action, so it shouldn't compete with blue.
7. Bold emphasis SHALL use `--hi` and be visually distinct from links.
8. `--dim` SHALL be used only for short labels and meta text (≈3.9:1), never paragraphs. **Why:** below AA for small body text.
9. Contrast: `--action` text on `--bg` ≥ 4.5:1; white bold on `--action` ≥ 3:1.

### R22 · Copy register
**Story:** As a visitor, I want the page to read like a terminal, not a help page.
**Why:** Long sentences compete with the countdown and the input. Fragments keep the game in the centre. Privacy and legal pages are the exception — clarity there beats style.

1. All UI copy SHALL be lowercase fragments without em-dashes or subordinate clauses, except on privacy and imprint pages.
2. THE hero SHALL be the tagline `lowest unique number wins`. **Why:** one confident line beats three explanatory ones, and the line already exists in the footer.
3. THE home page SHALL NOT show a strategy hint. **Why (decision):** players discover the crowding dynamic themselves.
4. Rules SHALL be exactly:
   ```
   01  one whole number above 0. once a day.
   02  all numbers revealed at 12:42.
   03  lowest number only one person picks wins.
       match anyone → you both lose it.
   ```
5. Copy that restates a visible button or element SHALL be removed.
6. Approved replacements:

| Where | Now (observed) | Becomes |
| --- | --- | --- |
| hero | pick the lowest number nobody else picks. one guess a day. | `lowest unique number wins` |
| status | not playing yet · pick a number below | `not playing yet ↓` |
| strategy ¶ | everyone wants a low number… you'll sign in next to lock it in. | *(deleted)* |
| yesterday | 17 / won by player #c378 · 273 played | `17` / `<username> won · 273 played` |
| results lead | the last round, … won by player #c378. see every pick → | `2026-10-20 · 273 played · <username> won with 17 → every pick` |
| held guess | create an account to lock in your 7 | `sign in to lock 7` |
| locked | ✓ you're in · your number: 7 · closes… | `locked: 7 · reveal 12:42 · 17h 32m` |
| late guess | — | `too late · round closed` |
| closed during sign-in | — | `round closed while you signed in · 7 not entered · next round 12:42` |
| empty results | the first round closes today at 12:42… | `no rounds yet · first reveal 12:42` |
| empty trends | no rounds have closed yet… | `no data yet · check back after 12:42` |
| loading | — | `loading…` |
| error | — | `couldn't load · retry →` |

7. Results and trends SHALL prefer labels and figures over prose summaries.

### R23 · Personality beat
**Story:** As a visitor, I want the site to feel like it has a character.
**Why:** Character comes from one deliberate flourish against lots of restraint. The input is where everyone looks, and a cursor also says "type here".

1. THE number input SHALL show a blinking `▊` cursor in `--action`, 1 s steps.
2. WHEN `prefers-reduced-motion` is set, THE cursor SHALL be static.
3. THE site SHALL keep its existing character carriers: lowercase, the ticking countdown, the bold "low" chip.
4. No other decorative animation SHALL be added. **Why:** spread everywhere, character becomes noise.

### R16 · Visual system
**Why:** The terminal look comes from one font, black, thin lines and restraint; readability is fixed with spacing, not a new typeface.

1. ALL text SHALL be monospace; the font stack begins `ui-monospace, SFMono-Regular, "Cascadia Mono", Menlo, Consolas, monospace`. **Why:** the current stack starts with Cascadia Mono, which most Mac/iOS/Android devices lack.
2. Long text SHALL use line height ≥ 1.6 and ≤ 72ch measure.
3. Sizes SHALL come from 13 / 16 / 20 / 28 / 48 px; spacing from an 8 px unit.
4. Dividers SHALL be 1 px `--edge`, at most one per section header. **Why:** spacing should group content, not lines.
5. No broad grey panels or decorative elements beyond the status block. **Why (refinement spec):** grey surfaces made the build feel generic.
6. Hover on links/buttons: `--action-hi` fill with `--bg` text.

---

## Phase 2 — First guess and round close

### R6 · Round page order
**Why:** A first-time visitor should understand and act in seconds; everything else is secondary.

1. Order: tagline → status block → number input + primary button → rules → yesterday → secondary links.
2. Secondary links (yesterday's analysis) SHALL be text links, not buttons.

### R7 · Guess before sign-in
**Why:** A signup wall before the first touch loses most arrivals from shared links.

1. THE input SHALL work when logged out; submit SHALL hold the guess in the server session and show `sign in to lock N`.
2. Sign-in SHALL show SSO (Google, GitHub, 42) first, email/password second.
3. AFTER sign-in, WHEN the round is open, THE SYSTEM SHALL lock the held guess exactly once.
4. IF the round closed during sign-in, THE SYSTEM SHALL show the R22 "closed during sign-in" copy.
5. IF the user already has a guess, THE SYSTEM SHALL keep it and show `already locked: N`.

### R8 · Locked state and calendar
**Why:** Players need proof their guess counts and a nudge to come back for the reveal.

1. WHEN locked, THE round page SHALL show `locked: N · reveal 12:42 · <time left>` in place of the input.
2. A text link `add to calendar` SHALL download an `.ics` for the close time linking to the day page.
3. THE `.ics` SHALL import into Apple, Google and Outlook calendars.

### R10 · Round close integrity
**Why:** A lost or doubled guess is the fastest way to lose a daily player's trust.

1. Submit SHALL disable on click; the endpoint SHALL be idempotent per user and round.
2. The locked display SHALL come from server state.
3. A guess after close SHALL get `too late · round closed`, never be dropped silently.
4. Identical numbers from different users SHALL all be stored.
5. After close, THE page SHALL show `round closed · revealing…` until results exist.

---

## Phase 3 — Results, trends, navigation

### R9 · Data context and states
**Why:** Numbers without scope mislead; empty, loading and error must look different or users can't tell "nothing yet" from "broken".

1. Every metric SHALL show its scope (`this round` / `all-time`) and trends their date range, as labels.
2. THE SYSTEM SHALL show **usernames** for players on results, day pages and leaderboards. **Why (decision):** privacy isn't a goal for this version; names make the game social.
3. THE privacy page SHALL state that usernames are shown publicly on results. **Why:** GDPR transparency still requires telling people what's public.
4. Loading, empty and error states SHALL each use their R22 copy and look distinct.
5. Filters and the date picker SHALL be disabled until one round has closed.
6. Processing duration SHALL NOT appear in the main view; on test instances it MAY sit in a collapsed `test details`.
7. Trends SHALL render first results within 1 s on a mid-range phone. *(Observed earlier: ~4 s blank, then "done in 4.1 ms".)*

### R11 · Day page reveal
**Why:** The reveal is the payoff of the daily loop; a table of numbers wastes it.

1. Lead line as figures: `2026-10-20 · 273 played · <username> won with 17`.
2. A number line or histogram: taken numbers struck in `--loss`, winner in `--state`, own pick marked.
3. A text summary and table alternative for keyboard and screen-reader users.
4. Rankings show ≤ 10 rows first; expand controls say what they reveal (`show all 273`) and can collapse.

### R12 · Sharing
**Why:** Players posting results in group chats is the growth loop.

1. `/g/day/<date>` SHALL be the permanent round URL, with its own OG image of that round's histogram.
2. A `share` link SHALL copy text that never includes the player's own number:
   ```
   i guess low · oct 20
   my pick: 🟥 (taken by 3)
   winner: 17 · 273 players
   🔥 4-day streak
   <origin>/g/day/2026-10-20
   ```
3. Use the Web Share API where available, otherwise clipboard + `copied`.

### R13 · Trends
**Why:** Figures answer "which numbers win?" faster than paragraphs.

1. Open with a figures line for the range, e.g. `7 rounds · median winner 9 · most crowded: 1`.
2. Compact charts with labels and legends.
3. Comparison windows labelled as dates (`10-15→10-17 vs 10-18→10-21`).
4. Selected range (7d / 30d / all) shown as white inversion + `aria-pressed`.

### R14 · Streaks and history
**Why:** A visible streak gives a reason to come back tomorrow.

1. Logged-in hero line: `🔥 4 · played 12 · won 1 · best 3`.
2. `/g/me` lists date · your pick · winner · won/lost, newest first.
3. A missed round resets the streak; computed from existing data.

### R20 · Navigation
**Why:** Day pages were dead ends and results had no way into them; players should move round ↔ past rounds ↔ their record without the back button.

1. Nav: `round · results · trends · me` (`me` when logged in), with `aria-current` on the active tab.
2. Under 641 px the nav SHALL fit one row or become a bottom bar; it SHALL NOT wrap.
3. Day pages SHALL show `← <prev date>` / `<next date> →` and, while a round is open, `play today →`.
4. Results SHALL list recent rounds (date · winner · players) linking to day pages, above the leaderboard.
5. Footer `privacy · imprint` on every page.

---

## Phase 4 — Legal, accessibility, responsive, platform

### R15 · Privacy page
**Why:** GDPR Art. 13 completeness. The refinement spec keeps full detail here: compact presentation, complete content.

1. Headed sections; legal basis per purpose; retention periods; named providers (Google, GitHub, 42, Resend, Cloudflare) with transfer notes; Datenschutzbehörde complaint line; public-username notice (R9.3).
2. Privacy tables SHALL stack into labelled rows below 641 px. **Why:** tables must not force the page wider than the screen.
3. A one-line "what this provider shares" note next to SSO buttons.
4. The "one cookie, no banner" claim SHALL be verified or corrected.

### R17 · Accessibility and forms
**Why:** Keep the strong baseline while the colours change underneath it.

1. `aria-current="page"` on the active nav item; one `h1` per page.
2. Decorative badges (`42`) `aria-hidden`.
3. Inputs keep the 3 px `--action` focus ring.
4. Login/signup errors: focus-managed `role="alert"` summary + inline messages, in `--loss`.
5. Preserve labels, autocomplete, skip link, reduced motion, 44 px coarse-pointer targets.

### R18 · Responsive
1. Tablet breakpoint 641–1024 px.
2. No horizontal page overflow at 320 px; tables scroll inside their container or stack.
3. Footer links meet 44 px targets on coarse pointers.

### R19 · Platform basics
1. `manifest.webmanifest` (theme `#0c0c0c`, standalone, `start_url /g/`).
2. `robots.txt` + sitemap of public pages.
3. `Cache-Control` / `ETag` on the shell; countdown script deferred.

### R24 · Verification
**Why (refinement spec):** visual review of a stale preview approves the wrong thing.

1. A visual review SHALL only count after the local preview is reloaded and shows the edited source (e.g. new tokens visible in computed styles).
2. Every phase SHALL be checked at 375 px and 1280 px.

---

## Traceability

| Req | Soul spec | Refinement spec | Earlier sources |
| --- | --- | --- | --- |
| R1 | — | legal detail kept | plan #1, spec P0 |
| R2–R3 | §3 | progress green | plan #3–4 |
| R4 | §2 solid block | one primary action | plan #5 |
| R5 | — | — | plan #2 |
| R6–R8, R10 | — | — | plan #6, #7, #9 |
| R9 | §4 labels note (superseded by usernames) | — | spec P0, plan #8 |
| R11–R14 | §4 results lead | figures over prose | plan #7, spec P1 |
| R15 | — | privacy detail, table stacking | plan #11 |
| R16 | — | black, mono, thin dividers | plan #12 |
| R17–R19 | §2 focus | blue focus | plan #10, #13, #14 |
| R20 | — | — | live check v1.1 |
| R21 | §1–§2 | visual direction | — |
| R22 | §4 | copy | plan #15 (reversed) |
| R23 | §5 | — | — |
| R24 | — | acceptance | — |
