//! Signing in through someone else: 42 intra, GitHub and Google. All three
//! are the OAuth2 authorization-code flow - send the visitor to the
//! provider with a single-use `state`, take the `code` they come back with,
//! trade it for a token, and ask who it belongs to.
//!
//! The same flow also *links* a login to an account that is already signed
//! in (`/link/{provider}`), so one person can come in through any of them.
//! Logins are never joined up on their own, not even when two of them report
//! the same email address: whoever controls an address at one provider would
//! otherwise walk into an account made somewhere else.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::CookieJar;
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::app::{error_page, AppError, AppState, ClientIp};
use crate::auth;
use crate::credentials::username_from;
use crate::db::{self, Linked, OAuthState};
use crate::handlers::current_viewer;
use crate::signin::{ip_hash, signed_in_to};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Intra,
    Github,
    Google,
}

impl Provider {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "42" => Some(Provider::Intra),
            "github" => Some(Provider::Github),
            "google" => Some(Provider::Google),
            _ => None,
        }
    }

    /// The `identities.provider` value, and the path segment.
    pub fn key(self) -> &'static str {
        match self {
            Provider::Intra => "42",
            Provider::Github => "github",
            Provider::Google => "google",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Intra => "42",
            Provider::Github => "GitHub",
            Provider::Google => "Google",
        }
    }

    /// Whether this instance has the keys to offer it. 42 always: it is how
    /// the site began.
    pub fn enabled(self, state: &AppState) -> bool {
        match self {
            Provider::Intra => true,
            Provider::Github => state.cfg.github().is_some(),
            Provider::Google => state.cfg.google().is_some(),
        }
    }

    fn redirect_uri(self, state: &AppState) -> String {
        match self {
            // Registered on the intra application, byte for byte; it predates
            // the other two and keeps its address.
            Provider::Intra => state.cfg.redirect_uri.clone(),
            other => format!("{}/auth/{}/callback", state.cfg.public_url, other.key()),
        }
    }
}

// ------------------------------------------------------------------ PKCE

/// A PKCE verifier: 64 characters from the unreserved set (RFC 7636).
fn pkce_verifier() -> String {
    format!("{}{}", auth::random_token(), auth::random_token())
}

/// The S256 challenge for a verifier: base64url(sha256(verifier)), unpadded.
fn pkce_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

// ----------------------------------------------------------- the way out

/// Sends the visitor off to `provider`, remembering the flow under a fresh
/// `state`. `link` is the signed-in account that is adding this login.
async fn start(state: &AppState, provider: Provider, link: Option<i64>) -> Result<Response, AppError> {
    let token = auth::random_token();
    let verifier = (provider != Provider::Intra).then(pkce_verifier);
    let flow = OAuthState {
        provider: provider.key().to_string(),
        link_account_id: link,
        pkce_verifier: verifier.clone(),
    };
    db::store_oauth_state(&state.db, &token, &flow).await?;

    let redirect_uri = provider.redirect_uri(state);
    let url = match provider {
        Provider::Intra => auth::authorize_url(&state.cfg, &token)?,
        Provider::Github => {
            let (client_id, _) = state.cfg.github().expect("checked by the caller");
            let mut url = Url::parse(&format!("{}/login/oauth/authorize", state.cfg.github_web_url))?;
            url.query_pairs_mut()
                .append_pair("client_id", client_id)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", "read:user user:email")
                .append_pair("state", &token)
                .append_pair("code_challenge", &pkce_challenge(verifier.as_deref().unwrap_or_default()))
                .append_pair("code_challenge_method", "S256");
            url.into()
        }
        Provider::Google => {
            let (client_id, _) = state.cfg.google().expect("checked by the caller");
            let mut url = Url::parse(&state.cfg.google_auth_url)?;
            url.query_pairs_mut()
                .append_pair("client_id", client_id)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("response_type", "code")
                .append_pair("scope", "openid email")
                .append_pair("state", &token)
                .append_pair("code_challenge", &pkce_challenge(verifier.as_deref().unwrap_or_default()))
                .append_pair("code_challenge_method", "S256")
                .append_pair("prompt", "select_account");
            url.into()
        }
    };
    let jar = CookieJar::new().add(auth::oauth_state_cookie(token, state.cfg.secure_cookies));
    Ok((jar, Redirect::to(&url)).into_response())
}

fn unavailable() -> Response {
    error_page(StatusCode::NOT_FOUND, crate::copy::NO_PROVIDER)
}

/// `/login/{provider}`: sign in, or make an account, through a provider.
pub async fn login(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Response, AppError> {
    match Provider::parse(&provider).filter(|p| p.enabled(&state)) {
        Some(p) => start(&state, p, None).await,
        None => Ok(unavailable()),
    }
}

/// `/login/42`, kept as its own route because it is where every link to
/// "log in to play" in the 42 pool points.
pub async fn login_42(State(state): State<AppState>) -> Result<Response, AppError> {
    start(&state, Provider::Intra, None).await
}

/// `/link/{provider}`: add a login to the signed-in account.
pub async fn link(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(provider): Path<String>,
) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    match Provider::parse(&provider).filter(|p| p.enabled(&state)) {
        Some(p) => start(&state, p, Some(viewer.account.id)).await,
        None => Ok(unavailable()),
    }
}

// ------------------------------------------------------- the way back in

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// The checks every callback starts with: the provider did not refuse, the
/// link is complete, its `state` is the one this browser started with
/// (`auth::OAUTH_STATE_COOKIE`), and one we issued, for this provider, and
/// not used before. Gives the code and the flow, or the page to show.
async fn arrived(
    state: &AppState,
    provider: Provider,
    jar: &CookieJar,
    query: CallbackQuery,
) -> Result<Result<(String, OAuthState), Response>, AppError> {
    if let Some(err) = query.error {
        let detail = query.error_description.unwrap_or(err);
        // Askama escapes this on the way into the page.
        return Ok(Err(error_page(
            StatusCode::BAD_REQUEST,
            &crate::copy::provider_refused(&provider.name().to_lowercase(), &detail),
        )));
    }
    let (Some(code), Some(token)) = (query.code, query.state) else {
        return Ok(Err(error_page(
            StatusCode::BAD_REQUEST,
            crate::copy::LINK_INCOMPLETE,
        )));
    };
    // Checked before the state is used up, so a link opened in someone
    // else's browser cannot spend it either.
    if jar.get(auth::OAUTH_STATE_COOKIE).map(|c| c.value()) != Some(token.as_str()) {
        return Ok(Err(error_page(StatusCode::BAD_REQUEST, crate::copy::LINK_OTHER_BROWSER)));
    }
    // Consumed before anything else happens, so a state cannot be replayed.
    match db::consume_oauth_state(&state.db, &token).await? {
        Some(flow) if flow.provider == provider.key() => Ok(Ok((code, flow))),
        _ => Ok(Err(error_page(
            StatusCode::BAD_REQUEST,
            crate::copy::LINK_USED,
        ))),
    }
}

/// A provider that did not answer as it should. Logged in full; the visitor
/// gets a page saying so rather than a bare 500.
fn provider_failed(provider: Provider, err: anyhow::Error) -> Response {
    tracing::error!(error = ?err, provider = provider.key(), "sign-in with a provider failed");
    error_page(
        StatusCode::BAD_GATEWAY,
        &crate::copy::provider_failed(&provider.name().to_lowercase()),
    )
}

/// Who a provider says the visitor is.
struct Person {
    subject: String,
    /// A verified address, if the provider shared one.
    email: Option<String>,
    /// What to build a username on.
    name_hint: String,
}

/// Adds the login to the account that started linking it. Only that same
/// account may finish: otherwise someone could start linking on their own
/// account and have a victim's browser complete it, tying the victim's login
/// to the attacker's account.
async fn finish_link(
    state: &AppState,
    jar: &CookieJar,
    provider: Provider,
    account_id: i64,
    person: &Person,
) -> Result<Result<(), Response>, AppError> {
    let viewer = current_viewer(state, jar).await?;
    if viewer.map(|v| v.account.id) != Some(account_id) {
        return Ok(Err(error_page(
            StatusCode::BAD_REQUEST,
            crate::copy::LINK_OTHER_SESSION,
        )));
    }
    let msg = match db::link_identity(&state.db, provider.key(), &person.subject, account_id, person.email.as_deref()).await? {
        Linked::Added => return Ok(Ok(())),
        Linked::Already => "already_linked",
        Linked::Elsewhere => "linked_elsewhere",
    };
    Ok(Err(Redirect::to(&format!("/account?msg={msg}")).into_response()))
}

/// Signs in the account behind a login, or makes one. `may_create` is asked
/// before a new account is made, and can refuse with a page.
async fn finish_sign_in(
    state: &AppState,
    jar: CookieJar,
    ip: &ClientIp,
    provider: Provider,
    person: Person,
    may_create: Option<Response>,
) -> Result<Response, AppError> {
    match db::identity_account(&state.db, provider.key(), &person.subject).await? {
        Some((account_id, true)) => return signed_in_to(state, jar, account_id, "/g").await,
        Some((_, false)) => {
            return Ok(error_page(StatusCode::FORBIDDEN, crate::copy::CANT_SIGN_IN));
        }
        None => {}
    }
    // The same verified address as an account that exists: that account,
    // with this login added, however it was made first. Only if its address
    // was proven, though; an unconfirmed password login on it could be a
    // stranger's, so it goes, and the provider's account is made fresh.
    if let Some(email) = &person.email {
        match db::email_owner(&state.db, email).await? {
            Some(owner) if owner.proven => {
                if !owner.active {
                    return Ok(error_page(StatusCode::FORBIDDEN, crate::copy::CANT_SIGN_IN));
                }
                db::link_identity(&state.db, provider.key(), &person.subject, owner.account_id, Some(email)).await?;
                tracing::info!(account = owner.account_id, provider = provider.key(), "joined up by email");
                return signed_in_to(state, jar, owner.account_id, "/g").await;
            }
            Some(owner) => db::drop_unproven_login(&state.db, owner.account_id).await?,
            None => {}
        }
    }
    if let Some(refusal) = may_create {
        return Ok(refusal);
    }
    let account = db::create_provider_account(
        &state.db,
        provider.key(),
        &person.subject,
        person.email.as_deref(),
        &username_from(&person.name_hint),
        ip_hash(state, ip).as_deref(),
    )
    .await?;
    tracing::info!(account = account.id, provider = provider.key(), "new account");
    // A provider's handle may be a real name its owner never meant to make
    // public here: one step to pick the username before playing (D6).
    signed_in_to(state, jar, account.id, "/welcome").await
}

// ---------------------------------------------------------------- GitHub

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct GithubUser {
    id: i64,
    login: String,
    /// RFC 3339, e.g. "2011-01-25T18:44:36Z".
    created_at: String,
}

#[derive(Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

async fn github_person(state: &AppState, code: &str, verifier: &str) -> anyhow::Result<(Person, chrono::DateTime<chrono::Utc>)> {
    let (client_id, secret) = state.cfg.github().ok_or_else(|| anyhow::anyhow!("GitHub is not configured"))?;
    let reply: TokenReply = state
        .http
        .post(format!("{}/login/oauth/access_token", state.cfg.github_web_url))
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("client_id", client_id),
            ("client_secret", secret),
            ("code", code),
            ("redirect_uri", &Provider::Github.redirect_uri(state)),
            ("code_verifier", verifier),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // GitHub answers 200 with an `error` field when the code is bad.
    let token = reply
        .access_token
        .ok_or_else(|| anyhow::anyhow!("no access token: {}", reply.error.unwrap_or_default()))?;

    let api = &state.cfg.github_api_url;
    let get = |path: &str| {
        state
            .http
            .get(format!("{api}{path}"))
            .bearer_auth(&token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "i_guess_42")
    };
    let user: GithubUser = get("/user").send().await?.error_for_status()?.json().await?;
    let emails: Vec<GithubEmail> = get("/user/emails").send().await?.error_for_status()?.json().await?;
    let email = emails
        .into_iter()
        .find(|e| e.primary && e.verified)
        .map(|e| e.email.to_lowercase());
    let created = chrono::DateTime::parse_from_rfc3339(&user.created_at)?.with_timezone(&chrono::Utc);
    Ok((
        Person {
            // The numeric id: a login can be renamed and then taken by
            // someone else, an id cannot.
            subject: user.id.to_string(),
            email,
            name_hint: user.login,
        },
        created,
    ))
}

pub async fn github_callback(
    State(state): State<AppState>,
    jar: CookieJar,
    ip: ClientIp,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let (code, flow) = match arrived(&state, Provider::Github, &jar, query).await? {
        Ok(found) => found,
        Err(page) => return Ok(page),
    };
    let verifier = flow.pkce_verifier.clone().unwrap_or_default();
    let (person, created) = match github_person(&state, &code, &verifier).await {
        Ok(found) => found,
        Err(err) => return Ok(provider_failed(Provider::Github, err)),
    };

    if let Some(account_id) = flow.link_account_id {
        return Ok(match finish_link(&state, &jar, Provider::Github, account_id, &person).await? {
            Ok(()) => Redirect::to("/account?msg=linked").into_response(),
            Err(page) => page,
        });
    }

    let min_age = chrono::Duration::days(state.cfg.github_min_age_days);
    let too_new = (chrono::Utc::now() - created < min_age).then(|| {
        error_page(
            StatusCode::FORBIDDEN,
            &crate::copy::github_too_new(state.cfg.github_min_age_days),
        )
    });
    finish_sign_in(&state, jar, &ip, Provider::Github, person, too_new).await
}

// ---------------------------------------------------------------- Google

#[derive(Deserialize)]
struct GoogleUser {
    sub: String,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
}

async fn google_person(state: &AppState, code: &str, verifier: &str) -> anyhow::Result<GoogleUser> {
    let (client_id, secret) = state.cfg.google().ok_or_else(|| anyhow::anyhow!("Google is not configured"))?;
    let reply: TokenReply = state
        .http
        .post(&state.cfg.google_token_url)
        .form(&[
            ("client_id", client_id),
            ("client_secret", secret),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &Provider::Google.redirect_uri(state)),
            ("code_verifier", verifier),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = reply
        .access_token
        .ok_or_else(|| anyhow::anyhow!("no access token: {}", reply.error.unwrap_or_default()))?;
    // The userinfo endpoint, over TLS with that token, says who it is; no ID
    // token signature to check.
    Ok(state
        .http
        .get(&state.cfg.google_userinfo_url)
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

pub async fn google_callback(
    State(state): State<AppState>,
    jar: CookieJar,
    ip: ClientIp,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let (code, flow) = match arrived(&state, Provider::Google, &jar, query).await? {
        Ok(found) => found,
        Err(page) => return Ok(page),
    };
    let verifier = flow.pkce_verifier.clone().unwrap_or_default();
    let user = match google_person(&state, &code, &verifier).await {
        Ok(found) => found,
        Err(err) => return Ok(provider_failed(Provider::Google, err)),
    };
    let email = user
        .email
        .filter(|_| user.email_verified)
        .map(|e| e.to_lowercase());
    let Some(email) = email else {
        return Ok(error_page(
            StatusCode::FORBIDDEN,
            crate::copy::GOOGLE_NO_EMAIL,
        ));
    };
    let person = Person {
        subject: user.sub,
        name_hint: email.split('@').next().unwrap_or_default().to_string(),
        email: Some(email),
    };

    if let Some(account_id) = flow.link_account_id {
        return Ok(match finish_link(&state, &jar, Provider::Google, account_id, &person).await? {
            Ok(()) => Redirect::to("/account?msg=linked").into_response(),
            Err(page) => page,
        });
    }
    finish_sign_in(&state, jar, &ip, Provider::Google, person, None).await
}

// -------------------------------------------------------------------- 42

/// `/auth/callback`: back from 42.
///
/// A student of an allowed campus plays both pools: their `users` row is
/// refreshed, and their account plays the 42 pool as it. A student of any
/// other campus may still play the global pool - their 42 login is a sign-in
/// like GitHub or Google - but gets no `users` row, so the 42 pool never
/// hears of them.
pub async fn callback_42(
    State(state): State<AppState>,
    jar: CookieJar,
    ip: ClientIp,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let (code, flow) = match arrived(&state, Provider::Intra, &jar, query).await? {
        Ok(found) => found,
        Err(page) => return Ok(page),
    };
    let me = match auth::exchange_code(&state.http, &state.cfg, &code).await {
        Ok(token) => match auth::fetch_me(&state.http, &state.cfg, &token).await {
            Ok(me) => me,
            Err(err) => return Ok(provider_failed(Provider::Intra, err)),
        },
        Err(err) => return Ok(provider_failed(Provider::Intra, err)),
    };
    let campus = me.primary_campus_id();
    let allowed = state.cfg.campus_allowed(campus);
    let person = Person {
        subject: me.id.to_string(),
        email: None,
        name_hint: me.login.clone(),
    };

    if let Some(account_id) = flow.link_account_id {
        if let Err(page) = finish_link(&state, &jar, Provider::Intra, account_id, &person).await? {
            return Ok(page);
        }
        if allowed {
            db::upsert_user(&state.db, me.id, &me.login, me.display_name(), me.image_url()).await?;
            if !db::attach_intra(&state.db, account_id, me.id).await? {
                return Ok(Redirect::to("/account?msg=linked_elsewhere").into_response());
            }
        }
        return Ok(Redirect::to("/account?msg=linked").into_response());
    }

    if !allowed {
        tracing::info!(
            login = %me.login,
            ?campus,
            campus_name = me.primary_campus_name().unwrap_or("?"),
            "42 sign-in from another campus: global pool only"
        );
        return finish_sign_in(&state, jar, &ip, Provider::Intra, person, None).await;
    }

    if let Some((_, false)) = db::identity_account(&state.db, "42", &person.subject).await? {
        return Ok(error_page(StatusCode::FORBIDDEN, crate::copy::CANT_SIGN_IN));
    }
    db::upsert_user(&state.db, me.id, &me.login, me.display_name(), me.image_url()).await?;
    let account = db::account_for_intra(&state.db, me.id, &me.login).await?;
    signed_in_to(&state, jar, account.id, "/").await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::routing::{get, post};
    use serde_json::{json, Value};

    use super::*;
    use crate::db::test_support::TempDb;
    use crate::testkit::{app_with, body, config, jar, location, set_cookie, stranger};

    /// One fake server for all three providers. A code stands for a person:
    /// the test says who `code` is, and the server answers accordingly.
    #[derive(Clone, Default)]
    struct Fake {
        people: Arc<Mutex<HashMap<String, Value>>>,
        /// The PKCE verifiers the token endpoints were sent, by code.
        verifiers: Arc<Mutex<HashMap<String, String>>>,
        broken: Arc<Mutex<bool>>,
    }

    impl Fake {
        fn person(&self, token: &str) -> Value {
            let code = token.trim_start_matches("Bearer ").trim_start_matches("tok-");
            self.people.lock().unwrap().get(code).cloned().unwrap_or(Value::Null)
        }
    }

    async fn token(axum::extract::State(f): axum::extract::State<Fake>, axum::Form(form): axum::Form<HashMap<String, String>>) -> Response {
        if *f.broken.lock().unwrap() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let code = form.get("code").cloned().unwrap_or_default();
        if let Some(v) = form.get("code_verifier") {
            f.verifiers.lock().unwrap().insert(code.clone(), v.clone());
        }
        if !f.people.lock().unwrap().contains_key(&code) {
            // GitHub's way of saying no: 200 with an error.
            return axum::Json(json!({ "error": "bad_verification_code" })).into_response();
        }
        axum::Json(json!({ "access_token": format!("tok-{code}"), "token_type": "bearer" })).into_response()
    }

    fn bearer(headers: &axum::http::HeaderMap) -> String {
        headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
    }

    async fn fake_server() -> (Fake, String) {
        let fake = Fake::default();
        let router = axum::Router::new()
            .route("/oauth/token", post(token))
            .route("/login/oauth/access_token", post(token))
            .route("/google/token", post(token))
            .route(
                "/v2/me",
                get(|axum::extract::State(f): axum::extract::State<Fake>, h: axum::http::HeaderMap| async move {
                    axum::Json(f.person(&bearer(&h))["intra"].clone())
                }),
            )
            .route(
                "/user",
                get(|axum::extract::State(f): axum::extract::State<Fake>, h: axum::http::HeaderMap| async move {
                    axum::Json(f.person(&bearer(&h))["github"].clone())
                }),
            )
            .route(
                "/user/emails",
                get(|axum::extract::State(f): axum::extract::State<Fake>, h: axum::http::HeaderMap| async move {
                    axum::Json(f.person(&bearer(&h))["emails"].clone())
                }),
            )
            .route(
                "/google/userinfo",
                get(|axum::extract::State(f): axum::extract::State<Fake>, h: axum::http::HeaderMap| async move {
                    axum::Json(f.person(&bearer(&h))["google"].clone())
                }),
            )
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (fake, base)
    }

    async fn setup() -> (TempDb, AppState, Fake) {
        let (fake, base) = fake_server().await;
        let mut cfg = config(&["megardes"]);
        cfg.redirect_uri = "http://test/auth/callback".into();
        cfg.ft_api_url = base.clone();
        cfg.github_client_id = Some("gh-id".into());
        cfg.github_client_secret = Some("gh-secret".into());
        cfg.github_min_age_days = 30;
        cfg.github_web_url = base.clone();
        cfg.github_api_url = base.clone();
        cfg.google_client_id = Some("g-id".into());
        cfg.google_client_secret = Some("g-secret".into());
        cfg.google_auth_url = format!("{base}/google/auth");
        cfg.google_token_url = format!("{base}/google/token");
        cfg.google_userinfo_url = format!("{base}/google/userinfo");
        let (t, state, _) = app_with(cfg).await;
        (t, state, fake)
    }

    fn github_user(id: i64, login: &str, days_old: i64, email: Option<&str>) -> Value {
        let created = (chrono::Utc::now() - chrono::Duration::days(days_old)).to_rfc3339();
        let emails = match email {
            Some(e) => json!([{ "email": "other@x.com", "primary": false, "verified": true },
                              { "email": e, "primary": true, "verified": true }]),
            None => json!([]),
        };
        json!({ "github": { "id": id, "login": login, "created_at": created }, "emails": emails })
    }

    fn google_user(sub: &str, email: &str, verified: bool) -> Value {
        json!({ "google": { "sub": sub, "email": email, "email_verified": verified } })
    }

    fn intra_user(id: i64, login: &str, campus: i64) -> Value {
        json!({ "intra": {
            "id": id, "login": login, "displayname": login.to_uppercase(),
            "campus": [{ "id": campus, "name": "Somewhere" }],
            "campus_users": [{ "campus_id": campus, "is_primary": true }]
        }})
    }

    /// Starts a flow and returns the `state` and the URL the visitor was sent to.
    async fn begin(state: &AppState, provider: &str, session: Option<&str>) -> (String, Url) {
        let resp = match session {
            Some(s) => link(State(state.clone()), jar(s), Path(provider.into())).await.unwrap(),
            None if provider == "42" => login_42(State(state.clone())).await.unwrap(),
            None => login(State(state.clone()), Path(provider.into())).await.unwrap(),
        };
        let url = Url::parse(location(&resp)).unwrap();
        let st = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
        (st, url)
    }

    /// Comes back from `provider` with `code`, as the browser would.
    async fn back(state: &AppState, provider: &str, code: &str, st: &str, session: Option<&str>) -> Response {
        let q = Query(CallbackQuery {
            code: Some(code.into()),
            state: Some(st.into()),
            error: None,
            error_description: None,
        });
        let j = session.map(jar).unwrap_or_default().add(axum_extra::extract::cookie::Cookie::new(auth::OAUTH_STATE_COOKIE, st.to_string()));
        let ip = ClientIp(None);
        match provider {
            "github" => github_callback(State(state.clone()), j, ip, q).await.unwrap(),
            "google" => google_callback(State(state.clone()), j, ip, q).await.unwrap(),
            _ => callback_42(State(state.clone()), j, ip, q).await.unwrap(),
        }
    }

    /// The whole round trip: out to the provider and back as `code`.
    async fn round_trip(state: &AppState, provider: &str, code: &str, session: Option<&str>) -> Response {
        let (st, _) = begin(state, provider, session).await;
        back(state, provider, code, &st, session).await
    }

    async fn viewer_of(state: &AppState, resp: &Response) -> db::Viewer {
        let s = set_cookie(resp, auth::SESSION_COOKIE).expect("a session");
        db::viewer_for_session(&state.db, &s).await.unwrap().unwrap()
    }

    async fn count(db: &db::Db, sql: &str) -> i64 {
        sqlx::query_as::<_, (i64,)>(sql).fetch_one(db).await.unwrap().0
    }

    #[tokio::test]
    async fn a_sign_in_only_finishes_in_the_browser_that_started_it() {
        let (t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("c1".into(), github_user(583231, "The-Octocat", 400, Some("Octo@Example.com")));
        // Starting hands this browser the state, for the callbacks only.
        let resp = login(State(state.clone()), Path("github".into())).await.unwrap();
        let url = Url::parse(location(&resp)).unwrap();
        let st = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
        assert_eq!(set_cookie(&resp, auth::OAUTH_STATE_COOKIE).as_deref(), Some(st.as_str()));
        let raw = resp.headers().get_all(axum::http::header::SET_COOKIE).iter().map(|v| v.to_str().unwrap().to_string()).find(|c| c.starts_with(auth::OAUTH_STATE_COOKIE)).unwrap();
        assert!(raw.contains("HttpOnly") && raw.contains("SameSite=Lax") && raw.contains("Path=/auth") && raw.contains("Max-Age=900"), "{raw}");

        // The link opened in another browser, with no cookie or a different
        // one: refused, nothing signed in, and the state not used up.
        for other in [None, Some("someone-elses")] {
            let mut j = CookieJar::new();
            if let Some(v) = other {
                j = j.add(axum_extra::extract::cookie::Cookie::new(auth::OAUTH_STATE_COOKIE, v));
            }
            let q = Query(CallbackQuery { code: Some("c1".into()), state: Some(st.clone()), error: None, error_description: None });
            let resp = github_callback(State(state.clone()), j, ClientIp(None), q).await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            assert!(body(resp).await.contains(crate::copy::LINK_OTHER_BROWSER));
        }
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 0);
        // The browser that started it still can.
        let resp = back(&state, "github", "c1", &st, None).await;
        assert_eq!(viewer_of(&state, &resp).await.account.username, "the-octocat");
    }

    #[tokio::test]
    async fn github_makes_an_account_once_and_signs_it_in_after() {
        let (t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("c1".into(), github_user(583231, "The-Octocat", 400, Some("Octo@Example.com")));

        let (st, url) = begin(&state, "github", None).await;
        assert!(url.as_str().starts_with(&format!("{}/login/oauth/authorize?", state.cfg.github_web_url)));
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "gh-id");
        assert_eq!(q["redirect_uri"], "http://test/auth/github/callback");
        assert_eq!(q["code_challenge_method"], "S256");

        let resp = back(&state, "github", "c1", &st, None).await;
        assert_eq!(location(&resp), "/welcome");
        // PKCE: the token endpoint got the verifier behind the challenge.
        let verifier = fake.verifiers.lock().unwrap()["c1"].clone();
        assert_eq!(pkce_challenge(&verifier), q["code_challenge"]);

        let v = viewer_of(&state, &resp).await;
        assert_eq!(v.account.username, "the-octocat");
        assert!(v.account.verified);
        assert!(v.intra.is_none());
        let (subject, email): (String, Option<String>) =
            sqlx::query_as("SELECT subject, email FROM identities WHERE provider = 'github'").fetch_one(&t.db).await.unwrap();
        assert_eq!((subject.as_str(), email.as_deref()), ("583231", Some("octo@example.com")));

        // Again: the same account, no new one, straight to the round.
        let again = round_trip(&state, "github", "c1", None).await;
        assert_eq!(location(&again), "/g");
        assert_eq!(viewer_of(&state, &again).await.account.id, v.account.id);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn a_brand_new_github_account_cannot_make_an_account() {
        let (t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("new".into(), github_user(1, "fresh", 3, None));
        let resp = round_trip(&state, "github", "new", None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(body(resp).await.contains("under 30 days"));
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 0);
    }

    #[tokio::test]
    async fn the_same_email_is_the_same_account_however_you_sign_in() {
        let (t, state, fake) = setup().await;
        // Email and password first, confirmed.
        let hash = crate::credentials::hash_password("correct horse battery").await.unwrap();
        let ann = match db::create_password_account(&t.db, "ann@example.com", "ann", &hash, None).await.unwrap() {
            db::NewAccount::Created(a) => a.id,
            other => panic!("{other:?}"),
        };
        db::mark_email_verified(&t.db, ann).await.unwrap();

        // Then Google with that address, any case: into ann's account, signed in.
        fake.people.lock().unwrap().insert("g".into(), google_user("g-1", "Ann@example.com", true));
        let resp = round_trip(&state, "google", "g", None).await;
        assert_eq!(location(&resp), "/g", "straight in: the account and its name exist");
        assert_eq!(viewer_of(&state, &resp).await.account.id, ann);
        // Then GitHub with it too: still ann's.
        fake.people.lock().unwrap().insert("c".into(), github_user(7, "ann-gh", 400, Some("ann@example.com")));
        let resp = round_trip(&state, "github", "c", None).await;
        assert_eq!(viewer_of(&state, &resp).await.account.id, ann);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM identities").await, 3, "email, google, github");
        // And the password still works.
        assert_eq!(db::password_login(&t.db, "ann@example.com").await.unwrap().map(|r| r.0), Some(ann));

        // The other way round: made with GitHub, then a sign-up by email with
        // the same address makes no second account (the owner gets a mail).
        fake.people.lock().unwrap().insert("d".into(), github_user(8, "dan", 400, Some("dan@example.com")));
        round_trip(&state, "github", "d", None).await;
        let again = db::create_password_account(&t.db, "DAN@example.com", "dan2", &hash, None).await.unwrap();
        assert!(matches!(again, db::NewAccount::EmailTaken), "{again:?}");
        // Google with dan's address finds the GitHub-made account.
        fake.people.lock().unwrap().insert("e".into(), google_user("g-8", "dan@example.com", true));
        let dan = viewer_of(&state, &round_trip(&state, "google", "e", None).await).await;
        assert_eq!(dan.account.username, "dan");
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 2);
        // A password for dan's own GitHub address on his own account: fine.
        // Ann's address on dan's account: no - it is ann's.
        assert!(db::add_password_login(&t.db, dan.account.id, "dan@example.com", &hash).await.unwrap());
        assert!(!db::add_password_login(&t.db, ann, "dan@example.com", &hash).await.unwrap());
    }

    #[tokio::test]
    async fn an_unconfirmed_sign_up_cannot_lie_in_wait_for_the_owner() {
        let (t, state, fake) = setup().await;
        // A stranger signs up with ann's address and a password they know,
        // and never confirms it.
        let hash = crate::credentials::hash_password("stranger's password").await.unwrap();
        db::create_password_account(&t.db, "ann@example.com", "lurker", &hash, None).await.unwrap();

        // Ann signs in with Google, which vouches for the address.
        fake.people.lock().unwrap().insert("g".into(), google_user("g-1", "ann@example.com", true));
        let resp = round_trip(&state, "google", "g", None).await;
        assert_eq!(location(&resp), "/welcome", "a fresh account of her own");
        let ann = viewer_of(&state, &resp).await;
        assert_ne!(ann.account.username, "lurker");
        // The stranger's password is gone, and their empty account with it.
        assert_eq!(db::password_login(&t.db, "ann@example.com").await.unwrap(), None);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn google_needs_a_verified_email() {
        let (t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("u".into(), google_user("g-2", "bob@example.com", false));
        assert_eq!(round_trip(&state, "google", "u", None).await.status(), StatusCode::FORBIDDEN);
        fake.people.lock().unwrap().insert("v".into(), google_user("g-2", "bob@example.com", true));
        let resp = round_trip(&state, "google", "v", None).await;
        assert_eq!(location(&resp), "/welcome");
        assert_eq!(viewer_of(&state, &resp).await.account.username, "bob");
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn a_state_works_once_and_only_for_its_own_provider() {
        let (_t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("c".into(), github_user(5, "five", 400, None));
        fake.people.lock().unwrap().insert("g".into(), google_user("g-5", "five@example.com", true));

        let (st, _) = begin(&state, "github", None).await;
        // Brought to Google's callback instead: refused, and spent.
        assert_eq!(back(&state, "google", "g", &st, None).await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(back(&state, "github", "c", &st, None).await.status(), StatusCode::BAD_REQUEST);

        let (st, _) = begin(&state, "github", None).await;
        assert_eq!(location(&back(&state, "github", "c", &st, None).await), "/welcome");
        assert_eq!(back(&state, "github", "c", &st, None).await.status(), StatusCode::BAD_REQUEST, "no replay");

        // Made up.
        assert_eq!(back(&state, "github", "c", "forged", None).await.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_provider_that_fails_gets_a_page_not_a_crash() {
        let (t, state, fake) = setup().await;
        *fake.broken.lock().unwrap() = true;
        assert_eq!(round_trip(&state, "github", "x", None).await.status(), StatusCode::BAD_GATEWAY);
        *fake.broken.lock().unwrap() = false;
        // A code the provider does not know.
        assert_eq!(round_trip(&state, "github", "unknown", None).await.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 0);
    }

    #[tokio::test]
    async fn a_provider_without_keys_is_not_offered() {
        let (_t, mut state, _) = setup().await;
        let mut cfg = (*state.cfg).clone();
        cfg.github_client_secret = None;
        state.cfg = Arc::new(cfg);
        let resp = login(State(state.clone()), Path("github".into())).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = login(State(state.clone()), Path("myspace".into())).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn linking_adds_a_login_to_the_account_that_asked() {
        let (t, state, fake) = setup().await;
        stranger(&t.db, "ann", "mine").await;
        let ann = db::viewer_for_session(&t.db, "mine").await.unwrap().unwrap().account.id;
        fake.people.lock().unwrap().insert("g".into(), google_user("g-ann", "ann@example.com", true));

        let resp = round_trip(&state, "google", "g", Some("mine")).await;
        assert_eq!(location(&resp), "/account?msg=linked");
        // Signing in with Google now lands on that same account.
        let resp = round_trip(&state, "google", "g", None).await;
        assert_eq!(viewer_of(&state, &resp).await.account.id, ann);
        // Linking it again changes nothing.
        let resp = round_trip(&state, "google", "g", Some("mine")).await;
        assert_eq!(location(&resp), "/account?msg=already_linked");

        // Someone else cannot take it.
        stranger(&t.db, "bob", "theirs").await;
        let resp = round_trip(&state, "google", "g", Some("theirs")).await;
        assert_eq!(location(&resp), "/account?msg=linked_elsewhere");
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM identities WHERE provider = 'google'").await, 1);
    }

    #[tokio::test]
    async fn a_link_can_only_be_finished_by_the_session_that_started_it() {
        let (t, state, fake) = setup().await;
        stranger(&t.db, "attacker", "evil").await;
        stranger(&t.db, "victim", "victim").await;
        fake.people.lock().unwrap().insert("v".into(), github_user(77, "victim-gh", 400, None));

        // The attacker starts linking, then gets the victim's browser to
        // come back with the victim's GitHub code.
        let (st, _) = begin(&state, "github", Some("evil")).await;
        let resp = back(&state, "github", "v", &st, Some("victim")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp = {
            let (st, _) = begin(&state, "github", Some("evil")).await;
            back(&state, "github", "v", &st, None).await
        };
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM identities WHERE provider = 'github'").await, 0);
    }

    #[tokio::test]
    async fn linking_needs_a_signed_in_account() {
        let (_t, state, _) = setup().await;
        let resp = link(State(state.clone()), CookieJar::new(), Path("github".into())).await.unwrap();
        assert_eq!(location(&resp), "/login");
    }

    #[tokio::test]
    async fn a_student_of_this_campus_plays_both_pools() {
        let (t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("i".into(), intra_user(1001, "megardes", 53));
        let (_, url) = begin(&state, "42", None).await;
        assert_eq!(url.query_pairs().find(|(k, _)| k == "redirect_uri").unwrap().1, "http://test/auth/callback");

        let resp = round_trip(&state, "42", "i", None).await;
        assert_eq!(location(&resp), "/");
        let v = viewer_of(&state, &resp).await;
        assert_eq!(v.intra.unwrap().login, "megardes");
        assert_eq!(v.account.intra_id, Some(1001));
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM users WHERE id = 1001").await, 1);
    }

    #[tokio::test]
    async fn a_student_of_another_campus_plays_the_global_pool_only() {
        let (t, mut state, fake) = setup().await;
        fake.people.lock().unwrap().insert("p".into(), intra_user(2002, "parisien", 1));

        let resp = round_trip(&state, "42", "p", None).await;
        assert_eq!(location(&resp), "/welcome");
        let v = viewer_of(&state, &resp).await;
        assert!(v.intra.is_none(), "no 42 pool");
        assert!(v.account.verified);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM users").await, 0, "the 42 pool never hears of them");
        let account = v.account.id;

        // Signing in again finds the same account.
        assert_eq!(viewer_of(&state, &round_trip(&state, "42", "p", None).await).await.account.id, account);

        // Should their campus be let in later, that same account starts
        // playing the 42 pool too.
        let mut cfg = (*state.cfg).clone();
        cfg.allowed_campus_ids = vec![53, 1];
        state.cfg = Arc::new(cfg);
        let resp = round_trip(&state, "42", "p", None).await;
        let v = viewer_of(&state, &resp).await;
        assert_eq!(v.account.id, account);
        assert_eq!(v.intra.map(|u| u.id), Some(2002));
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn linking_42_to_an_account_brings_the_42_pool_with_it() {
        let (t, state, fake) = setup().await;
        stranger(&t.db, "ann", "mine").await;
        fake.people.lock().unwrap().insert("i".into(), intra_user(1001, "megardes", 53));

        let resp = round_trip(&state, "42", "i", Some("mine")).await;
        assert_eq!(location(&resp), "/account?msg=linked");
        let v = db::viewer_for_session(&t.db, "mine").await.unwrap().unwrap();
        assert_eq!(v.intra.map(|u| u.login), Some("megardes".into()));
        assert!(v.account.verified, "42 vouches for them");

        // And signing in with 42 later lands on it.
        let resp = round_trip(&state, "42", "i", None).await;
        assert_eq!(viewer_of(&state, &resp).await.account.id, v.account.id);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn a_banned_account_cannot_sign_in_through_a_provider() {
        let (t, state, fake) = setup().await;
        fake.people.lock().unwrap().insert("c".into(), github_user(9, "nine", 400, None));
        fake.people.lock().unwrap().insert("i".into(), intra_user(1001, "megardes", 53));
        round_trip(&state, "github", "c", None).await;
        round_trip(&state, "42", "i", None).await;
        sqlx::query("UPDATE accounts SET banned_at = 'x'").execute(&t.db).await.unwrap();
        for (p, c) in [("github", "c"), ("42", "i")] {
            let resp = round_trip(&state, p, c, None).await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{p}");
            assert_eq!(set_cookie(&resp, auth::SESSION_COOKIE), None);
        }
    }

    #[tokio::test]
    async fn a_provider_refusal_is_shown_escaped() {
        let (_t, state, _) = setup().await;
        let (st, _) = begin(&state, "google", None).await;
        let q = Query(CallbackQuery {
            code: None,
            state: Some(st),
            error: Some("access_denied".into()),
            error_description: Some("<script>x</script>".into()),
        });
        let resp = google_callback(State(state.clone()), CookieJar::new(), ClientIp(None), q).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let html = body(resp).await;
        assert!(html.contains("google refused the sign-in"));
        assert!(!html.contains("<script>x"));
    }

    #[test]
    fn base64url_matches_the_rfc_vectors() {
        // RFC 4648 section 10, without padding, in the URL-safe alphabet.
        for (input, want) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64url(input.as_bytes()), want, "{input:?}");
        }
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn the_pkce_challenge_matches_rfc_7636() {
        // Appendix B of RFC 7636.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = pkce_verifier();
        assert!((43..=128).contains(&v.len()));
    }
}
