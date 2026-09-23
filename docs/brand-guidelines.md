# i guess low: brand guidelines

The source of truth for how the game looks and sounds. The design tokens live
at the top of `static/style.css` and follow this file.

## Name

**i guess low**, always lower case, including at the start of a sentence. It
describes the strategy (go low) and the site's domain, `iguesslow.com`. The
wordmark is "i guess" in the body font followed by "low" in a green mono
badge.

## Voice

Players are students checking in once a day, often on a phone. The site should
read like a well-made tool made by a fellow student: clear first, a little
dry, never cute.

| Trait | Means | Do | Don't |
|---|---|---|---|
| Clear | Say what happens and what to do next | "You'll confirm it on the next screen." | "Ready to roll?" |
| Plain | Everyday words, no jargon | "Nobody sees anyone's number until 12:42." | "Submissions are sealed pending resolution." |
| Calm | The game is small; don't shout | "Heads up. This is your one guess." | "WARNING!!! FINAL!!!" |
| Fair | Explain outcomes, including losing | "Every number was picked by at least two people." | "Nobody won lol" |

**Mechanics:** sentence case everywhere (headings, buttons, navigation). Buttons
start with a verb ("Lock it in", "Sign in", "Search"). Numbers are digits.
Times are 24-hour and say "Vienna time" where it matters.

## Colour

Dark only. One accent, green, for "live", "yours" and "won". Red, amber and
blue only ever carry meaning (error, warning, the guess field), always next to
words, never on their own.

| Role | Token | Value | Contrast |
|---|---|---|---|
| Background | `--bg` | `#0b0d10` | |
| Panel | `--surface` | `#12151a` | |
| Raised (cards, stats) | `--surface-raised` | `#181c22` | |
| Text | `--text` | `#e6e9ee` | 14:1 or more |
| Secondary text | `--text-muted` | `#9aa3af` | 6.7:1 or more |
| Accent / success | `--accent` | `#22c55e` | 7.5:1 or more |
| Danger | `--danger` | `#f05252` | 4.9:1 or more |
| Warning | `--warning` | `#f5a524` | 8.4:1 or more |
| Form control edges | `--border-control` | `#5f6978` | 3:1 |

Every text colour is checked against every surface it sits on (WCAG AA, 4.5:1).
A darker grey than `--text-muted` fails for text and is only used for
placeholders and dividers.

## Type

| Use | Font | Notes |
|---|---|---|
| Everything you read | Instrument Sans | 16px body, 1.6 line height |
| Every number, time and date in data | JetBrains Mono | tabular figures, so a ticking clock does not jitter |

Scale: 12, 14, 16, 18, 24, 32, 48, 72px. Both fonts are open source (SIL OFL,
licences in `static/fonts/`) and served from the site itself.

## Shape and space

Spacing on a 4px grid (4, 8, 12, 16, 24, 32, 48, 64). Corners: 6px for
controls, 10px for cards, 14px for panels. Borders are 1px; the guess field
alone gets 2px, because it is the one thing on the page that matters most.

## Motion

Quick and quiet: sections fade up 8px once on arrival (240ms, 50ms apart),
chart bars grow, the live dot pulses. Hover changes take 150ms. All of it
switches off under "reduce motion".

## Accessibility

Part of the brand, not an extra: visible focus ring, skip link, a heading on
every page, 44px touch targets on phones, 16px minimum text, nothing conveyed
by colour alone.
