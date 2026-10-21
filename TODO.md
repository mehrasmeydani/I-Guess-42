# To do before v2.0.0 goes live

What the `global` branch needs from the maintainer, not from code. Tick items
off as they are done; the code side is tracked in [SPEC.md](SPEC.md).

## 1. Release scope

- [x] All four phases of SPEC.md are built (accounts, global round, points,
      bets). v2.0.0 can ship once the items below are done.

## 2. Production `.env`

- [ ] **GitHub**: a second OAuth app for production (one callback URL per
      app): `https://iguesslow.com/auth/github/callback`. Its id and secret go in
      `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET`.
- [ ] **Google**: add `https://iguesslow.com/auth/google/callback` as a
      redirect URI and `iguesslow.com` as an authorised domain, then
      **Publish app** (testing mode only lets listed test users in).
- [ ] **Resend**: domain already verified. Set `RESEND_API_KEY` and
      `MAIL_FROM=i guess low <noreply@iguesslow.com>`.
- [ ] `IP_HASH_KEY`: a long random string (`openssl rand -hex 32`).
- [ ] `MODERATORS`: 42 logins, comma-separated.
- [ ] `IMPRINT_NAME`, `IMPRINT_EMAIL` (and `IMPRINT_ADDRESS` if needed).
- [ ] Check `FT_CLIENT_SECRET` is current: the live server answered 42's token
      endpoint with 401 `invalid_client` before.

## 3. Open from testing

- [ ] The verification mail never arrived locally. Check Resend → Emails, or
      the log line after "send another link", to see where it stops.

## 4. Legal

- [ ] Have the privacy page reviewed: legal bases, the wording on Resend's
      US transfer, and whether Turnstile sets cookies (not testable without
      keys).
- [ ] Decide whether a minimum-age statement is needed (UI/UX spec Q4).
- [ ] Confirm the retention periods stated on /privacy fit (sessions 30 days,
      logs capped at 30 MB, emailed links 24 h / 1 h, mail log 2 days).

## 4b. UI spec v2.0 decisions (docs/ui-spec)

- [ ] **Contrast.** The spec's `--dim #6e6e6e` (3.9:1) and white bold text
      on the blue button (~3.9:1) are below WCAG AA for small text; axe flags
      both on every page. The spec accepts this (R21.8, R21.9). If you want AA:
      `--dim: #8a8a8a` (5.2:1) and button text in `--bg` (5:1) - two lines in
      static/style.css.
- [x] **Departures from the spec, by request:** blue only on the lock-in
      button and the charts (not links, nav or focus); the sealed box and
      stamp kept for a locked number; no grey panel behind the status block.
- [ ] **Username prompt (Q4).** Done for new GitHub / Google / other-campus 42
      sign-ups only; existing SSO accounts are not asked retroactively.
- [ ] **Range control:** R13.4 asks for `aria-pressed`, which is only valid
      on buttons; the ranges are links, so they carry `aria-current` instead.

## 5. Tests only a person can do

- [ ] Import a round's `.ics` into Apple Calendar, Google Calendar, Outlook.
- [ ] Paste a public link into WhatsApp and Discord; check the preview cards.
- [ ] Install to the home screen on iOS and Android.
- [ ] A screen-reader pass (VoiceOver, TalkBack). Automated: 0 axe violations.

## 6. Release day

- [ ] Run the migration check on a copy of the live backup:
      `MIGRATE_CHECK_DB=/path/to/backup.db cargo test migrating_a_copy -- --ignored --nocapture`
- [ ] Merge `global` → `dev` → `main`, bump `version.txt` to 2.0.0, tag the
      merge commit, deploy.
