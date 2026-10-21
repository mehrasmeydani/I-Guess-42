# i guess low — UI/UX · Tasks

**Spec:** `ui-ux-improvements` · **Version:** 2.0
Each task: requirements (`→ Rx.y`), design decision (`D#`), and a one-line **why** for its place in the order. A phase is done when its acceptance criteria pass **and** R24 verification is done (reload the local preview, confirm the change is live, check 375 px + 1280 px).

## Phase 0 — Identity and trust (before v1.6.2 ships)

- [ ] **0.1 Environment flag** → R1.1, R9.6 · D2 — *why first:* imprint, test line and test details all depend on it.
- [ ] **0.2 Imprint** — test line, production one-liner under "impressum", single `h1`. → R1 — *legal exposure.*
- [ ] **0.3 Verify countdown + status block** — already built; check server timestamp, re-sync, `aria-label`, 320 px. → R2 · D1 — *observed mostly done; verify, don't rebuild.*
- [ ] **0.4 Verify progress bar** — role + values present; switch rendering to green `█` / `░` cells. → R3 · D10
- [ ] **0.5 One primary action above the fold at 375 px** → R4
- [ ] **0.6 OG/Twitter tags, static card, favicon set** → R5 · D13 — *cheap, high reach.*

## Phase 1 — Colour, copy, character (the "soul" fix)

- [ ] **1.1 Token migration** — add role tokens, new grey ramp; rename `--green/--blue/--red` uses to `--state/--action/--loss`. → R21.1 · D7 — *why before copy:* every later visual check depends on the right tokens.
- [ ] **1.2 Apply roles** — links, nav links, sign-in, primary button, focus → `--action`; open/low/progress/wins → `--state`; nav active stays white inversion. → R21.2–R21.7 · D7
- [ ] **1.3 `--dim` audit** — only short labels/meta, no paragraphs. → R21.8
- [ ] **1.4 Contrast check** → R21.9
- [ ] **1.5 Copy file** — move all UI strings into one table/file. → R22 · D8 — *why:* makes the copy pass one diff.
- [ ] **1.6 Copy pass** — hero tagline, delete strategy paragraph, rules as 3 lines, all R22.6 replacements. → R22.1–R22.7
- [ ] **1.7 Blinking cursor** — wrapper `::after`, reduced-motion static, hidden on focus. → R23 · D9 — *why last in phase:* the beat should land on the finished, restrained page.
- [ ] **1.8 Visual system** — mono stack order, line height 1.6 on prose, 5-step scale, ≤1 divider per section, no broad grey panels. → R16
- [ ] **1.9 Soul acceptance** — 0 elements with `--state` as button/link; links + button + focus all blue; emphasis/body/dim visibly distinct; no multi-sentence copy on the home page; one personality beat present. → R21–R23

## Phase 2 — First guess and round close

- [ ] **2.1 Round page order** → R6 · §3.1
- [ ] **2.2 Idempotent lock endpoint** → R10.1, R10.3, R10.4 · D4 — *why before held guess:* D3 reuses it.
- [ ] **2.3 Held guess + sign-in order** → R7.1–R7.2 · D3
- [ ] **2.4 Sign-in callback cases** → R7.3–R7.5 · D3
- [ ] **2.5 Locked state + `.ics`** → R8 · D13
- [ ] **2.6 Closed → revealing state** → R10.5 · D1
- [ ] **2.7 Tests** — double-click, ±1 s around close, concurrent identical numbers, held-guess cases, calendar imports. → R7, R8, R10

## Phase 3 — Results, trends, navigation

- [ ] **3.1 Usernames** — show usernames everywhere players appear; first-sign-in username prompt for SSO accounts; privacy line. → R9.2–R9.3 · D6 — *why first in phase:* every results view depends on what a player is called.
- [ ] **3.2 Scope labels + loading/empty/error states** → R9.1, R9.4, R9.5
- [ ] **3.3 Trends performance + remove "done in X ms"** → R9.6, R9.7 · D12
- [ ] **3.4 Day page reveal** — figures lead line, histogram, table alternative, ≤10 rows + labelled expand. → R11 · §3.2
- [ ] **3.5 Share + per-round OG image** → R12 · D13
- [ ] **3.6 Trends figures line, date-labelled windows, range state** → R13 · §3.4
- [ ] **3.7 Streak line + `/g/me`** → R14 · D5
- [ ] **3.8 Navigation** — `me` tab, one-row/bottom-bar mobile nav, day prev/next + `play today →`, recent rounds on results, footer everywhere. → R20 · D11
- [ ] **3.9 Walk-through** — round → day → previous day → today without the back button.

## Phase 4 — Legal, accessibility, responsive, platform (parallel from Phase 2)

- [ ] **4.1 Privacy rewrite** — sections, legal basis, retention, providers, transfers, Datenschutzbehörde, public usernames; tables stack on mobile. → R15.1–R15.2
- [ ] **4.2 SSO provider notes + cookie claim check** → R15.3–R15.4
- [ ] **4.3 A11y fixes** — `aria-current`, one `h1`, `aria-hidden` badges, blue focus ring on inputs, error summary. → R17
- [ ] **4.4 Responsive** — tablet breakpoint, no overflow at 320 px, footer targets. → R18
- [ ] **4.5 Platform** — manifest, robots + sitemap, caching, deferred script. → R19
- [ ] **4.6 Final verification** — axe scan, keyboard + screen reader, 320/375/768/1280, install to home screen. → R24

## Blocked on open questions (design.md §5)

- 0.2 production text → Q1 · 4.1 → Q2, Q3 · 3.1 → Q4 · 3.2 data badges → Q5
