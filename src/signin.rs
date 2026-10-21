//! Signing in with an email and a password, and everything around it:
//! creating the account, confirming the address, and resetting a forgotten
//! password. The pages behind `/login`, `/signup`, `/verify`, `/forgot` and
//! `/reset`.
//!
//! What a visitor learns from these pages is kept to what they already know.
//! Signing up with an address that has an account looks exactly like signing
//! up with a new one (the owner gets a mail saying so instead), a reset is
//! "sent" whether or not the address exists, and a failed sign-in says
//! "wrong email or password" and takes as long either way.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::app::{AppError, AppState, ClientIp};
use crate::credentials::{self, normalize_email, normalize_username, password_problem, username_problem};
use crate::db::{self, NewAccount, Pool, Viewer};
use crate::handlers::current_viewer;
use crate::limits;
use crate::mail;
use crate::templates::{
    self, ForgotTemplate, FormErrors, LoginTemplate, MessageTemplate, Notice, ResetTemplate,
    SignupTemplate, UserView, VerifyTemplate,
};
use crate::auth;

/// How long an emailed link works.
const VERIFY_FOR: chrono::Duration = chrono::Duration::hours(24);
const RESET_FOR: chrono::Duration = chrono::Duration::hours(1);

/// Where a password sign-in lands: the round everyone can play.
const AFTER_SIGN_IN: &str = "/g";

fn user_view(viewer: Option<Viewer>) -> Option<UserView> {
    viewer.map(UserView::from)
}

/// The visitor's address as a key for the rate limiter. Without one (tests,
/// or a proxy that sent nothing) everyone shares a key, which errs on the
/// side of limiting.
fn ip_key(ip: &ClientIp) -> String {
    ip.0.map_or_else(|| "unknown".to_string(), |ip| ip.to_string())
}

/// The keyed hash stored for an address, if IP_HASH_KEY is set.
pub fn ip_hash(state: &AppState, ip: &ClientIp) -> Option<String> {
    let key = state.cfg.ip_hash_key.as_deref()?;
    let month = chrono::Utc::now().format("%Y-%m").to_string();
    Some(credentials::ip_hash(key, ip.0?, &month))
}

fn minutes(d: std::time::Duration) -> u64 {
    d.as_secs().div_ceil(60).max(1)
}

/// Signs `account_id` in and sends them on to `to` - unless they typed a
/// number into the global round before signing in, in which case it is
/// locked for them now and they land on the round instead. Every way of
/// signing in ends here, so a held number is handled the same for all.
pub(crate) async fn signed_in_to(state: &AppState, jar: CookieJar, account_id: i64, to: &str) -> Result<Response, AppError> {
    let token = auth::random_token();
    db::create_session(&state.db, &token, account_id, auth::SESSION_TTL_DAYS).await?;
    let mut jar = jar.add(auth::session_cookie(token, state.cfg.secure_cookies));
    let mut to = to.to_string();
    if let Some(held_token) = crate::held::token(&jar) {
        // Always cleared, whatever becomes of it.
        jar = jar.remove(crate::held::clearing_cookie());
        if let Some(held) = state.held.take(&held_token) {
            to = lock_held(state, account_id, held).await?.to_string();
        }
    }
    Ok((jar, Redirect::to(&to)).into_response())
}

/// Locks a held number for a freshly signed-in account, exactly once, and
/// says where to go to hear how that went.
async fn lock_held(state: &AppState, account_id: i64, held: crate::held::Held) -> Result<&'static str, AppError> {
    let round = crate::round::Round::current(state.now()).key();
    if held.round != round {
        return Ok("/g?msg=held_late");
    }
    if !db::account_verified(&state.db, account_id).await? {
        return Ok("/g?msg=held_unverified");
    }
    crate::economy::collect(&state.db, account_id, &round).await?;
    Ok(
        match db::lock_global_guess(&state.db, &round, account_id, held.value, crate::economy::GUESS_STAKE).await? {
            db::Locked::In | db::Locked::AlreadySame => "/g?msg=saved",
            db::Locked::AlreadyOther => "/g?msg=held_existing",
            db::Locked::Broke => "/g?msg=broke",
            db::Locked::Voided => "/g?msg=voided",
        },
    )
}

/// Sends a mail, and logs rather than fails if it does not go out: the
/// visitor is told the same thing either way.
async fn send(state: &AppState, mail: mail::Mail) -> bool {
    match state
        .mailer
        .send(&state.db, &state.http, state.cfg.mail_daily_limit, &mail)
        .await
    {
        Ok(sent) => sent,
        Err(err) => {
            tracing::error!(error = ?err, purpose = mail.purpose, "sending a mail failed");
            false
        }
    }
}

/// Makes a verification link for `account_id` and mails it to `email`.
pub(crate) async fn send_verification(state: &AppState, account_id: i64, email: &str) -> Result<bool, AppError> {
    let (token, hash) = credentials::new_link_token();
    db::store_link_token(&state.db, account_id, "verify", &hash, VERIFY_FOR).await?;
    let link = format!("{}/verify?token={token}", state.cfg.public_url);
    Ok(send(state, mail::verify(email, &link)).await)
}

// ------------------------------------------------------------------ /login

#[derive(Deserialize, Default)]
pub struct LoginQuery {
    pub msg: Option<String>,
}

fn login_page(state: &AppState, viewer: Option<Viewer>, notice: Option<Notice>, email: String) -> LoginTemplate {
    LoginTemplate {
        pool: Pool::Global,
        user: user_view(viewer),
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        errors: FormErrors::default(),
        email,
        email_signup: state.cfg.email_signup(),
        github: state.cfg.github().is_some(),
        google: state.cfg.google().is_some(),
    }
}

/// "create an account to lock in your 7", for a visitor with a held number.
fn held_notice(state: &AppState, jar: &CookieJar) -> Option<Notice> {
    let held = state.held.peek(&crate::held::token(jar)?)?;
    Some(Notice::ok(format!(
        "create an account to lock in your {}.",
        crate::templates::group_digits(held.value)
    )))
}

pub async fn login_page_handler(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    let notice = match query.msg.as_deref() {
        Some("signed_out") => Some(Notice::ok("Signed out.")),
        Some("password_set") => Some(Notice::ok("Password changed. Sign in with the new one.")),
        _ => held_notice(&state, &jar),
    };
    Ok(templates::render(&login_page(&state, viewer, notice, String::new())))
}

#[derive(Deserialize)]
pub struct EmailLoginForm {
    /// An email address or a username.
    pub login: String,
    pub password: String,
}

pub async fn login_email(
    State(state): State<AppState>,
    ip: ClientIp,
    jar: CookieJar,
    Form(form): Form<EmailLoginForm>,
) -> Result<Response, AppError> {
    let refuse = |message: String, typed: String| {
        let mut page = login_page(&state, None, None, typed);
        page.errors = FormErrors::on("login", message);
        Ok((StatusCode::UNAUTHORIZED, templates::render(&page)).into_response())
    };
    let typed = form.login.trim().to_string();
    // As stored: an email normalised, a username lowercased. A malformed
    // address cannot have an account; it gets what any failure gets.
    let wanted = match credentials::looks_like_email(&typed) {
        true => normalize_email(&typed).unwrap_or_else(|_| typed.to_lowercase()),
        false => normalize_username(&typed),
    };
    let found = db::password_login_any(&state.db, &wanted).await?;

    // Counted per account (by its email) when there is one, so switching
    // between email and username does not buy extra tries; otherwise per
    // what was typed.
    let by_account = format!("login:email:{}", found.as_ref().map_or(wanted.as_str(), |f| f.2.as_str()));
    let by_ip = format!("login:ip:{}", ip_key(&ip));
    for (key, rule) in [(&by_account, limits::LOGIN_PER_EMAIL), (&by_ip, limits::LOGIN_PER_IP)] {
        if !state.limits.allows(key, rule) {
            let wait = minutes(state.limits.retry_after(key, rule));
            return refuse(format!("Too many attempts. Try again in {wait} minutes."), typed);
        }
    }

    let ok = credentials::verify_password(&form.password, found.as_ref().map(|f| f.1.as_str())).await?;
    match found {
        Some((account_id, _, _)) if ok => signed_in_to(&state, jar, account_id, AFTER_SIGN_IN).await,
        _ => {
            state.limits.record(&by_account);
            state.limits.record(&by_ip);
            refuse("Wrong username, email or password.".to_string(), typed)
        }
    }
}

// ----------------------------------------------------------------- /signup

fn signup_page(state: &AppState, notice: Option<Notice>, email: String, username: String) -> SignupTemplate {
    SignupTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        errors: FormErrors::default(),
        enabled: state.cfg.email_signup(),
        email,
        username,
        turnstile_site_key: state.cfg.turnstile_site_key.clone(),
    }
}

pub async fn signup_page_handler(State(state): State<AppState>, jar: CookieJar) -> Response {
    let notice = held_notice(&state, &jar);
    templates::render(&signup_page(&state, notice, String::new(), String::new()))
}

#[derive(Deserialize)]
pub struct SignupForm {
    pub email: String,
    pub username: String,
    pub password: String,
    #[serde(rename = "cf-turnstile-response", default)]
    pub turnstile: String,
}

/// The page every completed sign-up ends on, new address or not.
fn check_inbox(state: &AppState) -> Response {
    templates::render(&MessageTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        title: "check your inbox",
        lines: vec![
            "we sent a link to that address. open it to confirm the address and sign in.".into(),
            "nothing there after a few minutes? look in spam, or sign in and ask for another link.".into(),
        ],
        link: Some(("/login", "sign in")),
    })
}

pub async fn signup(
    State(state): State<AppState>,
    ip: ClientIp,
    Form(form): Form<SignupForm>,
) -> Result<Response, AppError> {
    let username = normalize_username(&form.username);
    let typed_email = form.email.trim().to_string();
    let refuse = |field: &str, text: &str| {
        let mut page = signup_page(&state, None, typed_email.clone(), username.clone());
        page.errors = FormErrors::on(field, text);
        Ok((StatusCode::UNPROCESSABLE_ENTITY, templates::render(&page)).into_response())
    };
    if !state.cfg.email_signup() {
        return Ok(templates::render(&signup_page(&state, None, String::new(), String::new())));
    }

    let email = match normalize_email(&form.email) {
        Ok(e) => e,
        Err(why) => return refuse("email", why),
    };
    if let Some(why) = username_problem(&username) {
        return refuse("username", why);
    }
    if let Some(why) = password_problem(&form.password, &email, &username) {
        return refuse("password", why);
    }
    if !turnstile_passed(&state, &form.turnstile, &ip).await {
        return refuse("", "The are-you-human check didn't pass. Try again.");
    }
    if !db::username_available(&state.db, &username).await? {
        return refuse("username", "That username is taken.");
    }
    // Counted only for a sign-up that got this far, so typos are free.
    if !state.limits.take(&format!("signup:{}", ip_key(&ip)), limits::SIGNUP_PER_IP) {
        return refuse("", "Too many new accounts from here. Try again in an hour.");
    }

    let hash = credentials::hash_password(&form.password).await?;
    match db::create_password_account(&state.db, &email, &username, &hash, ip_hash(&state, &ip).as_deref()).await? {
        NewAccount::Created(account) => {
            // Counts towards the address's mails for the hour, like a resend.
            state.limits.record(&format!("mail:{email}"));
            send_verification(&state, account.id, &email).await?;
            tracing::info!(account = account.id, "new password account");
        }
        NewAccount::EmailTaken => {
            // Tell the owner, not the visitor.
            if state.limits.take(&format!("mail:{email}"), limits::MAIL_PER_ADDRESS) {
                send(&state, mail::exists(&email, &state.cfg.public_url)).await;
            }
        }
        NewAccount::UsernameTaken => return refuse("username", "That username is taken."),
    }
    Ok(check_inbox(&state))
}

#[derive(Deserialize)]
struct TurnstileReply {
    #[serde(default)]
    success: bool,
}

/// Whether the Turnstile widget's answer checks out with Cloudflare. Always
/// true when no Turnstile keys are set. If Cloudflare cannot be reached the
/// sign-up is let through: a CAPTCHA outage must not lock everyone out, and
/// the per-address limit still applies.
async fn turnstile_passed(state: &AppState, response: &str, ip: &ClientIp) -> bool {
    let Some(secret) = state.cfg.turnstile_secret.as_deref() else {
        return true;
    };
    if response.is_empty() {
        return false;
    }
    let mut form = vec![("secret", secret.to_string()), ("response", response.to_string())];
    if let Some(addr) = ip.0 {
        form.push(("remoteip", addr.to_string()));
    }
    let sent = state.http.post(&state.cfg.turnstile_verify_url).form(&form).send().await;
    match sent {
        Ok(resp) => resp.json::<TurnstileReply>().await.map(|r| r.success).unwrap_or(false),
        Err(err) => {
            tracing::warn!(%err, "Turnstile unreachable; letting the sign-up through");
            true
        }
    }
}

// ----------------------------------------------------------------- /verify

#[derive(Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    pub token: String,
}

fn dead_link(state: &AppState) -> Response {
    let mut resp = templates::render(&MessageTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        title: "that link doesn't work",
        lines: vec![
            "it has expired or was already used. links from us work once, for a day \
             (password links for an hour)."
                .into(),
        ],
        link: Some(("/login", "sign in")),
    });
    *resp.status_mut() = StatusCode::GONE;
    resp
}

/// Shows a button, and confirms nothing yet; see verify.html.
pub async fn verify_page(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Result<Response, AppError> {
    let hash = credentials::hash_link_token(&q.token);
    if db::peek_link_token(&state.db, "verify", &hash).await?.is_none() {
        return Ok(dead_link(&state));
    }
    Ok(templates::render(&VerifyTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        token: q.token,
    }))
}

#[derive(Deserialize)]
pub struct TokenForm {
    pub token: String,
}

pub async fn verify(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<TokenForm>,
) -> Result<Response, AppError> {
    let hash = credentials::hash_link_token(&form.token);
    let Some(account_id) = db::use_link_token(&state.db, "verify", &hash).await? else {
        return Ok(dead_link(&state));
    };
    db::mark_email_verified(&state.db, account_id).await?;
    signed_in_to(&state, jar, account_id, "/g?msg=verified").await
}

/// Another verification link, for a signed-in account that lost the first.
pub async fn resend_verification(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    let Some((email, false)) = db::password_email(&state.db, viewer.account.id).await? else {
        return Ok(Redirect::to("/g").into_response());
    };
    if !state.limits.take(&format!("mail:{email}"), limits::MAIL_PER_ADDRESS) {
        return Ok(Redirect::to("/g?msg=resend_wait").into_response());
    }
    let msg = if send_verification(&state, viewer.account.id, &email).await? {
        "resent"
    } else {
        "mail_failed"
    };
    Ok(Redirect::to(&format!("/g?msg={msg}")).into_response())
}

// --------------------------------------------------------- /forgot, /reset

pub async fn forgot_page(State(state): State<AppState>) -> Response {
    templates::render(&ForgotTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice: None,
    })
}

#[derive(Deserialize)]
pub struct ForgotForm {
    pub email: String,
}

pub async fn forgot(
    State(state): State<AppState>,
    Form(form): Form<ForgotForm>,
) -> Result<Response, AppError> {
    if let Ok(email) = normalize_email(&form.email) {
        if let Some((account_id, _)) = db::password_login(&state.db, &email).await? {
            if state.limits.take(&format!("mail:{email}"), limits::MAIL_PER_ADDRESS) {
                let (token, hash) = credentials::new_link_token();
                db::store_link_token(&state.db, account_id, "reset", &hash, RESET_FOR).await?;
                let link = format!("{}/reset?token={token}", state.cfg.public_url);
                send(&state, mail::reset(&email, &link)).await;
            }
        }
    }
    Ok(templates::render(&MessageTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        title: "check your inbox",
        lines: vec![
            "if that address has an account, a link to pick a new password is on its way. \
             it works for an hour."
                .into(),
        ],
        link: Some(("/login", "back to sign in")),
    }))
}

fn reset_page(state: &AppState, token: String, notice: Option<Notice>) -> ResetTemplate {
    ResetTemplate {
        pool: Pool::Global,
        user: None,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        token,
    }
}

pub async fn reset_page_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Result<Response, AppError> {
    let hash = credentials::hash_link_token(&q.token);
    if db::peek_link_token(&state.db, "reset", &hash).await?.is_none() {
        return Ok(dead_link(&state));
    }
    Ok(templates::render(&reset_page(&state, q.token, None)))
}

#[derive(Deserialize)]
pub struct ResetForm {
    pub token: String,
    pub password: String,
}

pub async fn reset(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ResetForm>,
) -> Result<Response, AppError> {
    let hash = credentials::hash_link_token(&form.token);
    // Check the new password before using the link up, so a refused one can
    // be retyped on the same link.
    let Some(account_id) = db::peek_link_token(&state.db, "reset", &hash).await? else {
        return Ok(dead_link(&state));
    };
    let email = db::password_email(&state.db, account_id).await?.map(|e| e.0).unwrap_or_default();
    let username = db::account_username(&state.db, account_id).await?.unwrap_or_default();
    if let Some(why) = password_problem(&form.password, &email, &username) {
        let page = reset_page(&state, form.token, Some(Notice::error(why)));
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, templates::render(&page)).into_response());
    }
    if db::use_link_token(&state.db, "reset", &hash).await?.is_none() {
        return Ok(dead_link(&state));
    }
    let new_hash = credentials::hash_password(&form.password).await?;
    db::set_password(&state.db, account_id, &new_hash).await?;
    // The link came through their inbox, which proves the address too.
    db::mark_email_verified(&state.db, account_id).await?;
    signed_in_to(&state, jar, account_id, "/g?msg=password_set").await
}

#[cfg(test)]
mod tests {
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::Form;
    use axum_extra::extract::cookie::CookieJar;

    use super::*;
    use crate::testkit::{app_with, body, config, jar, location, set_cookie};

    const PASSWORD: &str = "correct horse battery";

    fn no_ip() -> ClientIp {
        ClientIp(None)
    }

    fn signup_form(email: &str, username: &str, password: &str) -> Form<SignupForm> {
        Form(SignupForm {
            email: email.into(),
            username: username.into(),
            password: password.into(),
            turnstile: String::new(),
        })
    }

    async fn sign_up(state: &AppState, email: &str, username: &str, password: &str) -> Response {
        signup(State(state.clone()), no_ip(), signup_form(email, username, password))
            .await
            .unwrap()
    }

    async fn log_in(state: &AppState, email: &str, password: &str) -> Response {
        login_email(
            State(state.clone()),
            no_ip(),
            CookieJar::new(),
            Form(EmailLoginForm { login: email.into(), password: password.into() }),
        )
        .await
        .unwrap()
    }

    /// The token in the last mail's link.
    fn token_in(mail: &mail::Mail) -> String {
        let from = mail.text.find("token=").expect("a link with a token") + "token=".len();
        mail.text[from..].split_whitespace().next().unwrap().to_string()
    }

    async fn count(db: &db::Db, sql: &str) -> i64 {
        sqlx::query_as::<_, (i64,)>(sql).fetch_one(db).await.unwrap().0
    }

    /// A test instance, so email sign-up is on and mail is kept in memory.
    async fn test_app() -> (crate::db::test_support::TempDb, AppState, std::sync::Arc<std::sync::Mutex<Vec<mail::Mail>>>) {
        app_with(config(&["megardes"])).await
    }

    #[tokio::test]
    async fn signing_up_sends_a_link_that_confirms_and_signs_in() {
        let (t, state, sent) = test_app().await;

        let resp = sign_up(&state, " Ann@Example.com ", "Ann", PASSWORD).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(set_cookie(&resp, auth::SESSION_COOKIE), None, "not signed in until confirmed");
        assert!(body(resp).await.contains("check your inbox"));

        let mails = sent.lock().unwrap().clone();
        assert_eq!(mails.len(), 1);
        assert_eq!((mails[0].to.as_str(), mails[0].purpose), ("ann@example.com", "verify"));
        assert!(mails[0].text.contains("http://test/verify?token="));
        let token = token_in(&mails[0]);
        // Only the hash is kept.
        assert_eq!(count(&t.db, &format!("SELECT COUNT(*) FROM email_tokens WHERE token_hash = '{token}'")).await, 0);

        let (username, verified): (String, Option<String>) =
            sqlx::query_as("SELECT username, verified_at FROM accounts").fetch_one(&t.db).await.unwrap();
        assert_eq!((username.as_str(), verified), ("ann", None));

        // Opening the link only shows a button, so a mail scanner opening it
        // does not use it up.
        for _ in 0..2 {
            let page = verify_page(State(state.clone()), Query(TokenQuery { token: token.clone() })).await.unwrap();
            assert_eq!(page.status(), StatusCode::OK);
            assert!(body(page).await.contains("confirm and sign in"));
        }
        let resp = verify(State(state.clone()), CookieJar::new(), Form(TokenForm { token: token.clone() }))
            .await
            .unwrap();
        assert_eq!(location(&resp), "/g?msg=verified");
        let session = set_cookie(&resp, auth::SESSION_COOKIE).expect("signed in");
        let v = db::viewer_for_session(&t.db, &session).await.unwrap().unwrap();
        assert_eq!(v.account.username, "ann");
        assert!(v.account.verified);
        assert!(v.intra.is_none());

        // Once only.
        let again = verify(State(state.clone()), CookieJar::new(), Form(TokenForm { token: token.clone() }))
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::GONE);
        let page = verify_page(State(state.clone()), Query(TokenQuery { token })).await.unwrap();
        assert_eq!(page.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn signing_up_with_a_known_email_looks_the_same_and_tells_the_owner() {
        let (t, state, sent) = test_app().await;
        sign_up(&state, "ann@example.com", "ann", PASSWORD).await;
        sent.lock().unwrap().clear();

        let first = body(sign_up(&state, "bob@example.com", "bob", PASSWORD).await).await;
        sent.lock().unwrap().clear();
        let resp = sign_up(&state, "ANN@example.com", "someoneelse", "another fine password").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body(resp).await, first, "the page gives nothing away");

        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 2, "no second account");
        let mails = sent.lock().unwrap().clone();
        assert_eq!(mails.len(), 1);
        assert_eq!((mails[0].to.as_str(), mails[0].purpose), ("ann@example.com", "exists"));
        assert!(mails[0].text.contains("http://test/forgot"));
    }

    #[tokio::test]
    async fn bad_sign_ups_are_refused_with_what_was_typed_kept() {
        let (t, state, sent) = test_app().await;
        db::upsert_user(&t.db, 7, "megardes", "M", None).await.unwrap();
        sign_up(&state, "taken@example.com", "takenname", PASSWORD).await;
        let before = count(&t.db, "SELECT COUNT(*) FROM accounts").await;
        sent.lock().unwrap().clear();

        for (email, username, password, says) in [
            ("not-an-email", "ann", PASSWORD, "look like an email"),
            ("x@mailinator.com", "ann", PASSWORD, "Throwaway"),
            ("ann@example.com", "a", PASSWORD, "3 to 20"),
            ("ann@example.com", "admin", PASSWORD, "reserved"),
            ("ann@example.com", "MeGardes", PASSWORD, "taken"),
            ("ann@example.com", "takenname", PASSWORD, "taken"),
            ("ann@example.com", "ann", "short", "at least 10"),
            ("ann@example.com", "ann", "1234567890", "most common"),
            ("ann@example.com", "ann", "ann@example.com", "be your email address"),
        ] {
            let resp = sign_up(&state, email, username, password).await;
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{email} {username}");
            let html = body(resp).await;
            assert!(html.contains(says), "{email} {username}: expected {says:?}");
            assert!(html.contains(&format!(r#"value="{email}""#)), "email kept");
            assert!(!html.contains(password) || password == email, "the password is never echoed");
        }
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, before);
        assert!(sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_live_site_without_mail_offers_no_email_sign_up() {
        let (t, state, _) = app_with(config(&[])).await;
        let resp = sign_up(&state, "ann@example.com", "ann", PASSWORD).await;
        assert!(body(resp).await.contains("isn't available"));
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 0);
        let page = body(login_page_handler(State(state), CookieJar::new(), Query(LoginQuery::default())).await.unwrap()).await;
        assert!(!page.contains(r#"action="/login/email""#));
        assert!(page.contains("/login/42"));
    }

    /// Signs up and confirms; returns the account id.
    async fn confirmed(state: &AppState, sent: &std::sync::Arc<std::sync::Mutex<Vec<mail::Mail>>>, email: &str, username: &str) -> i64 {
        sign_up(state, email, username, PASSWORD).await;
        let token = token_in(sent.lock().unwrap().last().unwrap());
        let resp = verify(State(state.clone()), CookieJar::new(), Form(TokenForm { token })).await.unwrap();
        let session = set_cookie(&resp, auth::SESSION_COOKIE).unwrap();
        db::viewer_for_session(&state.db, &session).await.unwrap().unwrap().account.id
    }

    #[tokio::test]
    async fn signing_in_needs_the_right_password_and_says_the_same_thing_otherwise() {
        let (t, state, sent) = test_app().await;
        let ann = confirmed(&state, &sent, "ann@example.com", "ann").await;

        let wrong = log_in(&state, "ann@example.com", "not the password").await;
        let unknown = log_in(&state, "nobody@example.com", PASSWORD).await;
        for resp in [wrong, unknown] {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(set_cookie(&resp, auth::SESSION_COOKIE), None);
            assert!(body(resp).await.contains("Wrong username, email or password."));
        }

        let resp = log_in(&state, " ANN@example.com", PASSWORD).await;
        assert_eq!(location(&resp), "/g");
        let session = set_cookie(&resp, auth::SESSION_COOKIE).unwrap();
        assert_eq!(db::viewer_for_session(&t.db, &session).await.unwrap().unwrap().account.id, ann);

        // A banned account gets the same answer as a wrong password.
        sqlx::query("UPDATE accounts SET banned_at = 'x' WHERE id = ?1").bind(ann).execute(&t.db).await.unwrap();
        let resp = log_in(&state, "ann@example.com", PASSWORD).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unconfirmed_account_can_sign_in_but_not_play_until_it_confirms() {
        let (_t, state, sent) = test_app().await;
        sign_up(&state, "ann@example.com", "ann", PASSWORD).await;
        let resp = log_in(&state, "ann@example.com", PASSWORD).await;
        let session = set_cookie(&resp, auth::SESSION_COOKIE).unwrap();

        let page = body(
            crate::handlers::global_index(State(state.clone()), jar(&session), Query(crate::handlers::IndexQuery { msg: None }))
                .await
                .unwrap(),
        )
        .await;
        assert!(page.contains("confirm your email"));
        assert!(page.contains(r#"action="/verify/resend""#));

        // Another link on request, but not endlessly.
        sent.lock().unwrap().clear();
        let mut outcomes = vec![];
        for _ in 0..4 {
            let resp = resend_verification(State(state.clone()), jar(&session)).await.unwrap();
            outcomes.push(location(&resp).to_string());
        }
        // The sign-up mail counted against the same address limit.
        assert_eq!(outcomes, ["/g?msg=resent", "/g?msg=resent", "/g?msg=resend_wait", "/g?msg=resend_wait"]);
        assert_eq!(sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_username_signs_in_as_well_as_an_email() {
        let (t, state, sent) = test_app().await;
        let ann = confirmed(&state, &sent, "ann@example.com", "ann").await;

        for typed in ["ann", " ANN "] {
            let resp = log_in(&state, typed, PASSWORD).await;
            assert_eq!(location(&resp), "/g", "{typed:?}");
            let session = set_cookie(&resp, auth::SESSION_COOKIE).unwrap();
            assert_eq!(db::viewer_for_session(&t.db, &session).await.unwrap().unwrap().account.id, ann);
        }
        let resp = log_in(&state, "ann", "not the password").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(body(resp).await.contains(r#"value="ann""#), "what was typed is kept");

        // An account without a password, found by its username, gets the
        // same answer as one that does not exist.
        crate::testkit::student(&t.db, 1001, "megardes", "s").await;
        let no_password = body(log_in(&state, "megardes", PASSWORD).await).await;
        let nobody = body(log_in(&state, "nobody", PASSWORD).await).await;
        assert!(no_password.contains("Wrong username, email or password."));
        assert!(nobody.contains("Wrong username, email or password."));
    }

    #[tokio::test]
    async fn switching_between_username_and_email_buys_no_extra_tries() {
        let (_t, state, sent) = test_app().await;
        confirmed(&state, &sent, "ann@example.com", "ann").await;
        for n in 0..limits::LOGIN_PER_EMAIL.max {
            let typed = if n % 2 == 0 { "ann" } else { "ann@example.com" };
            log_in(&state, typed, "guess guess guess").await;
        }
        for typed in ["ann", "ann@example.com"] {
            let resp = log_in(&state, typed, PASSWORD).await;
            assert!(body(resp).await.contains("Too many attempts"), "{typed}");
        }
    }

    #[tokio::test]
    async fn repeated_failures_lock_the_address_even_with_the_right_password() {
        let (_t, state, sent) = test_app().await;
        confirmed(&state, &sent, "ann@example.com", "ann").await;
        for _ in 0..limits::LOGIN_PER_EMAIL.max {
            assert_eq!(log_in(&state, "ann@example.com", "guess guess guess").await.status(), StatusCode::UNAUTHORIZED);
        }
        let resp = log_in(&state, "ann@example.com", PASSWORD).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(body(resp).await.contains("Too many attempts"));
    }

    #[tokio::test]
    async fn a_reset_link_sets_a_new_password_and_signs_everyone_else_out() {
        let (t, state, sent) = test_app().await;
        let ann = confirmed(&state, &sent, "ann@example.com", "ann").await;
        let old_session = set_cookie(&log_in(&state, "ann@example.com", PASSWORD).await, auth::SESSION_COOKIE).unwrap();
        sent.lock().unwrap().clear();

        // Unknown addresses get the same page and no mail.
        let unknown = body(forgot(State(state.clone()), Form(ForgotForm { email: "nobody@example.com".into() })).await.unwrap()).await;
        assert!(sent.lock().unwrap().is_empty());
        let known = body(forgot(State(state.clone()), Form(ForgotForm { email: "Ann@example.com".into() })).await.unwrap()).await;
        assert_eq!(unknown, known);
        let mail = sent.lock().unwrap().last().unwrap().clone();
        assert_eq!(mail.purpose, "reset");
        let token = token_in(&mail);

        let page = reset_page_handler(State(state.clone()), Query(TokenQuery { token: token.clone() })).await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);

        // A refused password leaves the link usable.
        let weak = reset(State(state.clone()), CookieJar::new(), Form(ResetForm { token: token.clone(), password: "short".into() }))
            .await
            .unwrap();
        assert_eq!(weak.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let resp = reset(
            State(state.clone()),
            CookieJar::new(),
            Form(ResetForm { token: token.clone(), password: "a brand new password".into() }),
        )
        .await
        .unwrap();
        assert_eq!(location(&resp), "/g?msg=password_set");
        let new_session = set_cookie(&resp, auth::SESSION_COOKIE).unwrap();
        assert_eq!(db::viewer_for_session(&t.db, &new_session).await.unwrap().unwrap().account.id, ann);
        assert!(db::viewer_for_session(&t.db, &old_session).await.unwrap().is_none(), "the old session ended");

        assert_eq!(log_in(&state, "ann@example.com", PASSWORD).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(location(&log_in(&state, "ann@example.com", "a brand new password").await), "/g");

        let again = reset(State(state.clone()), CookieJar::new(), Form(ResetForm { token, password: "yet another password".into() }))
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn a_reset_confirms_an_unconfirmed_address() {
        let (t, state, sent) = test_app().await;
        sign_up(&state, "ann@example.com", "ann", PASSWORD).await;
        forgot(State(state.clone()), Form(ForgotForm { email: "ann@example.com".into() })).await.unwrap();
        let token = token_in(sent.lock().unwrap().last().unwrap());
        reset(State(state.clone()), CookieJar::new(), Form(ResetForm { token, password: "a brand new password".into() }))
            .await
            .unwrap();
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts WHERE verified_at IS NOT NULL").await, 1);
    }

    #[tokio::test]
    async fn an_expired_link_does_not_work() {
        let (t, state, sent) = test_app().await;
        sign_up(&state, "ann@example.com", "ann", PASSWORD).await;
        let token = token_in(sent.lock().unwrap().last().unwrap());
        sqlx::query("UPDATE email_tokens SET expires_at = '2000-01-01T00:00:00Z'").execute(&t.db).await.unwrap();
        let resp = verify(State(state.clone()), CookieJar::new(), Form(TokenForm { token })).await.unwrap();
        assert_eq!(resp.status(), StatusCode::GONE);
        // A verify link is not a reset link either.
        sign_up(&state, "bob@example.com", "bob", PASSWORD).await;
        let token = token_in(sent.lock().unwrap().last().unwrap());
        let resp = reset_page_handler(State(state.clone()), Query(TokenQuery { token })).await.unwrap();
        assert_eq!(resp.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn no_mail_goes_out_past_the_daily_limit_but_resets_get_headroom() {
        let mut cfg = config(&["megardes"]);
        cfg.mail_daily_limit = 1;
        let (t, state, sent) = app_with(cfg).await;
        sign_up(&state, "ann@example.com", "ann", PASSWORD).await; // uses the one
        sign_up(&state, "bob@example.com", "bob", PASSWORD).await; // over: not sent
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 2, "the account still exists");

        forgot(State(state.clone()), Form(ForgotForm { email: "ann@example.com".into() })).await.unwrap();
        assert_eq!(sent.lock().unwrap().last().unwrap().purpose, "reset");
    }

    #[tokio::test]
    async fn the_turnstile_check_is_asked_when_configured() {
        use axum::routing::post;
        // A stand-in for Cloudflare that passes exactly the answer "good".
        let fake = axum::Router::new().route(
            "/siteverify",
            post(|axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| async move {
                let ok = f.get("secret").map(String::as_str) == Some("s3cret")
                    && f.get("response").map(String::as_str) == Some("good");
                axum::Json(serde_json::json!({ "success": ok }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

        let mut cfg = config(&["megardes"]);
        cfg.turnstile_site_key = Some("site".into());
        cfg.turnstile_secret = Some("s3cret".into());
        cfg.turnstile_verify_url = format!("http://{addr}/siteverify");
        let (t, state, _) = app_with(cfg).await;

        let page = body(signup_page_handler(State(state.clone()), CookieJar::new()).await).await;
        assert!(page.contains(r#"data-sitekey="site""#));

        for answer in ["", "bad"] {
            let mut form = signup_form("ann@example.com", "ann", PASSWORD);
            form.0.turnstile = answer.into();
            let resp = signup(State(state.clone()), no_ip(), form).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{answer:?}");
        }
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 0);

        let mut form = signup_form("ann@example.com", "ann", PASSWORD);
        form.0.turnstile = "good".into();
        let resp = signup(State(state.clone()), no_ip(), form).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn sign_ups_from_one_address_are_limited() {
        let (t, state, _) = test_app().await;
        let ip = || ClientIp(Some("203.0.113.9".parse().unwrap()));
        for n in 0..4 {
            let form = signup_form(&format!("p{n}@example.com"), &format!("player{n}"), PASSWORD);
            signup(State(state.clone()), ip(), form).await.unwrap();
        }
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 3);
        // Another address is not held up.
        let form = signup_form("p9@example.com", "player9", PASSWORD);
        signup(State(state.clone()), ClientIp(Some("203.0.113.10".parse().unwrap())), form).await.unwrap();
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 4);
    }

    #[tokio::test]
    async fn the_signup_address_is_stored_only_as_a_keyed_hash() {
        let mut cfg = config(&["megardes"]);
        cfg.ip_hash_key = Some("k".into());
        let (t, state, _) = app_with(cfg).await;
        let ip = ClientIp(Some("203.0.113.9".parse().unwrap()));
        signup(State(state.clone()), ip, signup_form("ann@example.com", "ann", PASSWORD)).await.unwrap();
        let (hash,): (Option<String>,) = sqlx::query_as("SELECT signup_ip_hash FROM accounts").fetch_one(&t.db).await.unwrap();
        let hash = hash.unwrap();
        assert_eq!(hash.len(), 16);
        assert!(!hash.contains("203"));
    }
}
