# To do before v2.0.0 goes live

What the `global` branch needs from the maintainer, not from code. Tick items
off as they are done; the code side is tracked in [SPEC.md](SPEC.md).

## 1. Release scope

- [ ] Ship v2.0.0 only once all four phases of SPEC.md are built (accounts,
      global round, points, bets), or decide to ship earlier.

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
- [ ] Optional: `TURNSTILE_SITE_KEY` / `TURNSTILE_SECRET` (Cloudflare) for a
      bot check on sign-up.
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
