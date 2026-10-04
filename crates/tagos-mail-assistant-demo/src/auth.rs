use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Form, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const CODE_TTL_SECS: i64 = 60;
pub const ACCESS_TTL_SECS: i64 = 3600;
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;

const CLIENTS_JSON: &str = include_str!("../demo/clients.json");

const ALLOWED_ORIGINS: &[&str] = &[
    "http://localhost:6274",
    "http://127.0.0.1:6274",
    "http://localhost:8787",
    "http://127.0.0.1:8787",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Off,
    Mcp,
    Alexa,
}

#[derive(Clone)]
pub struct AuthState {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    mode: AuthMode,
    base: String,
    clients: Vec<Client>,
    codes: HashMap<String, PendingCode>,
    access: HashMap<String, LiveToken>,
    refresh: HashMap<String, LiveToken>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

#[derive(Deserialize)]
struct ClientFile {
    clients: Vec<Client>,
}

#[derive(Clone, Deserialize)]
struct Client {
    client_id: String,
    redirect_uris: Vec<String>,
}

struct PendingCode {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    scope: String,
    expires_at: DateTime<Utc>,
    used: bool,
}

struct LiveToken {
    aud: String,
    scope: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct TokenInfo {
    pub scopes: Vec<String>,
}

#[derive(Debug)]
pub struct ServePlan {
    pub bind_host: String,
    pub base: String,
}

pub fn serve_plan(
    port: u16,
    public_bind: bool,
    login: bool,
    static_token: bool,
    base_override: Option<&str>,
    listen: Option<&str>,
) -> Result<ServePlan, &'static str> {
    if public_bind && listen.is_some() {
        return Err("--listen cannot be combined with --public");
    }
    if public_bind && !login {
        return Err("--public requires --auth-profile");
    }
    if static_token && !login {
        return Err("--demo-static-token requires --auth-profile");
    }
    let bind_host = if public_bind {
        "0.0.0.0".to_string()
    } else {
        listen.unwrap_or("127.0.0.1").to_string()
    };
    if static_token && !is_loopback(&bind_host) {
        return Err("--demo-static-token is loopback only");
    }
    let base = match base_override {
        Some(raw) => raw.trim_end_matches('/').to_string(),
        None if !public_bind => format!("http://127.0.0.1:{port}"),
        None => return Err("--public requires --base-url"),
    };
    Ok(ServePlan { bind_host, base })
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

impl AuthState {
    pub fn off() -> Self {
        Self::with_now(AuthMode::Off, "http://127.0.0.1", Arc::new(Utc::now))
    }

    pub fn open(mode: AuthMode, base: impl Into<String>) -> Self {
        Self::with_now(mode, base, Arc::new(Utc::now))
    }

    pub fn with_now(
        mode: AuthMode,
        base: impl Into<String>,
        now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        let file: ClientFile = serde_json::from_str(CLIENTS_JSON).expect("demo clients");
        Self {
            inner: Arc::new(Mutex::new(Inner {
                mode,
                base: base.into().trim_end_matches('/').to_string(),
                clients: file.clients,
                codes: HashMap::new(),
                access: HashMap::new(),
                refresh: HashMap::new(),
                now,
            })),
        }
    }

    pub fn mode(&self) -> AuthMode {
        self.lock().mode
    }

    pub fn base(&self) -> String {
        self.lock().base.clone()
    }

    pub fn login_enabled(&self) -> bool {
        self.mode() != AuthMode::Off
    }

    pub fn issue_static_token(&self) -> String {
        let mut inner = self.lock();
        let scope = "mail.read mail.draft".to_string();
        let aud = inner.base.clone();
        let expires_at = (inner.now)() + Duration::seconds(ACCESS_TTL_SECS);
        let raw = random_secret();
        inner.access.insert(
            digest(&raw),
            LiveToken {
                aud,
                scope,
                expires_at,
            },
        );
        raw
    }

    #[cfg(test)]
    fn stores_raw_secret(&self, raw: &str) -> bool {
        let inner = self.lock();
        inner.access.contains_key(raw)
            || inner.refresh.contains_key(raw)
            || inner.codes.contains_key(raw)
    }

    pub fn authenticate(&self, raw: &str) -> Result<TokenInfo, ()> {
        let inner = self.lock();
        let Some(token) = inner.access.get(&digest(raw)) else {
            return Err(());
        };
        if (inner.now)() >= token.expires_at || token.aud != inner.base {
            return Err(());
        }
        Ok(TokenInfo {
            scopes: token.scope.split_whitespace().map(str::to_string).collect(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("auth lock")
    }
}

pub fn required_scope(tool: &str) -> Option<&'static str> {
    match tool {
        "await_reply"
        | "resolve_followup"
        | "forget_followups"
        | "prepare_reply_draft"
        | "update_draft"
        | "delete_draft"
        | "demo_advance_clock"
        | "demo_deliver_reply"
        | "demo_reset" => Some("mail.draft"),
        "whoami" | "search_messages" | "list_people" | "list_contexts" | "get_thread"
        | "list_drafts" | "list_followups" | "check_followups" | "inbox_briefing" => {
            Some("mail.read")
        }
        _ => None,
    }
}

pub async fn prm(State(auth): State<AuthState>) -> Json<Value> {
    let base = auth.base();
    Json(json!({
        "resource": base,
        "authorization_servers": [format!("{base}/demo-oauth")],
        "scopes_supported": ["mail.read", "mail.draft"],
        "bearer_methods_supported": ["header"],
    }))
}

pub async fn as_meta(State(auth): State<AuthState>) -> Json<Value> {
    let base = auth.base();
    let issuer = format!("{base}/demo-oauth");
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "scopes_supported": ["mail.read", "mail.draft"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

#[derive(Deserialize)]
pub struct AuthorizeForm {
    client_id: String,
    redirect_uri: String,
    response_type: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
    resource: Option<String>,
    scope: Option<String>,
}

pub async fn authorize_get(
    State(auth): State<AuthState>,
    Query(form): Query<AuthorizeForm>,
) -> Response {
    authorize_page(&auth, &form)
}

pub async fn authorize_post(
    State(auth): State<AuthState>,
    Form(form): Form<AuthorizeForm>,
) -> Response {
    let checked = match check_authorize(&auth, &form) {
        Ok(checked) => checked,
        Err(fail) => return fail.into_response(),
    };
    let raw = random_secret();
    let mut inner = auth.lock();
    let expires_at = (inner.now)() + Duration::seconds(CODE_TTL_SECS);
    inner.codes.insert(
        digest(&raw),
        PendingCode {
            client_id: checked.client_id,
            redirect_uri: checked.redirect_uri.clone(),
            challenge: checked.challenge,
            resource: checked.resource,
            scope: checked.scope,
            expires_at,
            used: false,
        },
    );
    let issuer = format!("{}/demo-oauth", inner.base);
    let location = format!(
        "{}?code={}&state={}&iss={}",
        checked.redirect_uri,
        percent_encode(&raw),
        percent_encode(&checked.state),
        percent_encode(&issuer),
    );
    Redirect::to(&location).into_response()
}

pub async fn token(State(auth): State<AuthState>, Form(form): Form<TokenForm>) -> Response {
    match form.grant_type.as_str() {
        "authorization_code" => code_grant(&auth, &form),
        "refresh_token" => refresh_grant(&auth, &form),
        _ => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
    }
}

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    code_verifier: String,
    #[serde(default)]
    resource: String,
    #[serde(default)]
    refresh_token: String,
}

pub async fn guard(State(auth): State<AuthState>, request: Request<Body>, next: Next) -> Response {
    if !request.uri().path().starts_with("/mcp") {
        return next.run(request).await;
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let origin = origin.to_str().unwrap_or("");
        if !ALLOWED_ORIGINS.contains(&origin) {
            return StatusCode::FORBIDDEN.with_json(json!({"error": "forbidden_origin"}));
        }
    }
    if query_has_token(request.uri().query().unwrap_or("")) {
        return unauthorized(&auth);
    }
    if !auth.login_enabled() {
        return next.run(request).await;
    }
    let raw = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if raw.is_empty() {
        return unauthorized(&auth);
    }
    let info = match auth.authenticate(raw) {
        Ok(info) => info,
        Err(()) => return unauthorized(&auth),
    };
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1_048_576)
        .await
        .unwrap_or_default();
    if let Some(tool) = tool_name(&bytes) {
        if let Some(scope) = required_scope(&tool) {
            if !info.scopes.iter().any(|have| have == scope) {
                return StatusCode::FORBIDDEN.with_json(json!({"error": "insufficient_scope"}));
            }
        }
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

struct CheckedAuth {
    client_id: String,
    redirect_uri: String,
    state: String,
    challenge: String,
    resource: String,
    scope: String,
}

fn authorize_page(auth: &AuthState, form: &AuthorizeForm) -> Response {
    let checked = match check_authorize(auth, form) {
        Ok(checked) => checked,
        Err(fail) => return fail.into_response(),
    };
    let html = format!(
        "<!doctype html><html><body><form method=\"post\" action=\"/demo-oauth/authorize\">\
         <input type=\"hidden\" name=\"client_id\" value=\"{client}\">\
         <input type=\"hidden\" name=\"redirect_uri\" value=\"{redirect}\">\
         <input type=\"hidden\" name=\"response_type\" value=\"code\">\
         <input type=\"hidden\" name=\"state\" value=\"{state}\">\
         <input type=\"hidden\" name=\"code_challenge\" value=\"{challenge}\">\
         <input type=\"hidden\" name=\"code_challenge_method\" value=\"S256\">\
         <input type=\"hidden\" name=\"resource\" value=\"{resource}\">\
         <input type=\"hidden\" name=\"scope\" value=\"{scope}\">\
         <button type=\"submit\">Link Tagos Mail (demo)</button></form></body></html>",
        client = escape(&checked.client_id),
        redirect = escape(&checked.redirect_uri),
        state = escape(&checked.state),
        challenge = escape(&checked.challenge),
        resource = escape(&checked.resource),
        scope = escape(&checked.scope),
    );
    Html(html).into_response()
}

fn check_authorize(auth: &AuthState, form: &AuthorizeForm) -> Result<CheckedAuth, AuthFail> {
    let inner = auth.lock();
    if form.resource.as_deref() != Some(inner.base.as_str()) {
        return Err(AuthFail::InvalidRequest);
    }
    if form.response_type != "code"
        || form.state.is_empty()
        || form.code_challenge.is_empty()
        || form.code_challenge_method != "S256"
    {
        return Err(AuthFail::InvalidRequest);
    }
    let Some(client) = inner
        .clients
        .iter()
        .find(|client| client.client_id == form.client_id)
    else {
        return Err(AuthFail::InvalidRequest);
    };
    if !client
        .redirect_uris
        .iter()
        .any(|uri| uri == &form.redirect_uri)
    {
        return Err(AuthFail::InvalidRequest);
    }
    let scope = normalize_scope(form.scope.as_deref())?;
    Ok(CheckedAuth {
        client_id: form.client_id.clone(),
        redirect_uri: form.redirect_uri.clone(),
        state: form.state.clone(),
        challenge: form.code_challenge.clone(),
        resource: inner.base.clone(),
        scope,
    })
}

fn code_grant(auth: &AuthState, form: &TokenForm) -> Response {
    let mut inner = auth.lock();
    if form.resource != inner.base || form.client_id.is_empty() || form.redirect_uri.is_empty() {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let key = digest(&form.code);
    let now = (inner.now)();
    let aud = inner.base.clone();
    let scope = {
        let Some(pending) = inner.codes.get_mut(&key) else {
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
        };
        if pending.used || now >= pending.expires_at {
            pending.used = true;
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
        }
        if pending.client_id != form.client_id
            || pending.redirect_uri != form.redirect_uri
            || pending.resource != form.resource
            || s256(&form.code_verifier) != pending.challenge
        {
            pending.used = true;
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
        }
        pending.used = true;
        pending.scope.clone()
    };
    issue_pair(&mut inner, aud, scope)
}

fn refresh_grant(auth: &AuthState, form: &TokenForm) -> Response {
    let mut inner = auth.lock();
    if !form.resource.is_empty() && form.resource != inner.base {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let key = digest(&form.refresh_token);
    let Some(current) = inner.refresh.remove(&key) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    if (inner.now)() >= current.expires_at || current.aud != inner.base {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    issue_pair(&mut inner, current.aud, current.scope)
}

fn issue_pair(inner: &mut Inner, aud: String, scope: String) -> Response {
    let now = (inner.now)();
    let access = random_secret();
    let refresh = random_secret();
    inner.access.insert(
        digest(&access),
        LiveToken {
            aud: aud.clone(),
            scope: scope.clone(),
            expires_at: now + Duration::seconds(ACCESS_TTL_SECS),
        },
    );
    inner.refresh.insert(
        digest(&refresh),
        LiveToken {
            aud,
            scope: scope.clone(),
            expires_at: now + Duration::seconds(REFRESH_TTL_SECS),
        },
    );
    (
        StatusCode::OK,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TTL_SECS,
            "refresh_token": refresh,
            "scope": scope,
        })
        .to_string(),
    )
        .into_response()
}

fn normalize_scope(raw: Option<&str>) -> Result<String, AuthFail> {
    let raw = raw.unwrap_or("mail.read mail.draft");
    let mut scopes = Vec::new();
    for scope in raw.split_whitespace() {
        if scope != "mail.read" && scope != "mail.draft" {
            return Err(AuthFail::InvalidScope);
        }
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    if scopes.is_empty() {
        return Err(AuthFail::InvalidScope);
    }
    Ok(scopes.join(" "))
}

enum AuthFail {
    InvalidRequest,
    InvalidScope,
}

impl AuthFail {
    fn into_response(self) -> Response {
        let code = match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidScope => "invalid_scope",
        };
        oauth_error(StatusCode::BAD_REQUEST, code)
    }
}

fn unauthorized(auth: &AuthState) -> Response {
    let inner = auth.lock();
    let mut response = Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::CONTENT_TYPE, "application/json");
    if inner.mode == AuthMode::Mcp {
        response = response.header(
            header::WWW_AUTHENTICATE,
            format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"mail.read mail.draft\"",
                inner.base
            ),
        );
    }
    response
        .body(Body::from(r#"{"error":"unauthenticated"}"#))
        .unwrap_or_else(|_| StatusCode::UNAUTHORIZED.into_response())
}

fn oauth_error(status: StatusCode, code: &str) -> Response {
    status.with_json(json!({"error": code}))
}

fn query_has_token(query: &str) -> bool {
    query.split('&').any(|pair| {
        let key = pair.split('=').next().unwrap_or("");
        key == "access_token" || key == "token"
    })
}

fn tool_name(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    if value.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    value
        .pointer("/params/name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn digest(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn percent_encode(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

trait WithJson {
    fn with_json(self, value: Value) -> Response;
}

impl WithJson for StatusCode {
    fn with_json(self, value: Value) -> Response {
        (
            self,
            [(header::CONTENT_TYPE, "application/json")],
            value.to_string(),
        )
            .into_response()
    }
}

#[cfg(test)]
fn issue_access(auth: &AuthState, aud: &str, scope: &str) -> String {
    let mut inner = auth.lock();
    let expires_at = (inner.now)() + Duration::seconds(ACCESS_TTL_SECS);
    let raw = random_secret();
    inner.access.insert(
        digest(&raw),
        LiveToken {
            aud: aud.to_string(),
            scope: scope.to_string(),
            expires_at,
        },
    );
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::sync::Mutex as StdMutex;
    use tower::ServiceExt;

    fn clock(start: DateTime<Utc>) -> (AuthState, Arc<StdMutex<DateTime<Utc>>>) {
        let slot = Arc::new(StdMutex::new(start));
        let now_slot = slot.clone();
        let auth = AuthState::with_now(
            AuthMode::Mcp,
            "http://127.0.0.1:8787",
            Arc::new(move || *now_slot.lock().expect("clock")),
        );
        (auth, slot)
    }

    fn router(auth: AuthState) -> axum::Router {
        crate::mcp::app_router_with(crate::mcp::DemoMcp::ephemeral(), auth)
    }

    async fn body_of(response: Response) -> (StatusCode, String, Option<String>) {
        let status = response.status();
        let www = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned(), www)
    }

    fn challenge(verifier: &str) -> String {
        s256(verifier)
    }

    fn authorize_query(resource: Option<&str>, scope: &str) -> String {
        let mut url = format!(
            "/demo-oauth/authorize?client_id=demo-inspector&redirect_uri={}&response_type=code&state=state-1&code_challenge={}&code_challenge_method=S256&scope={}",
            percent_encode("http://127.0.0.1:6276/oauth/callback"),
            percent_encode(&challenge("verifier-0123456789abcdef")),
            percent_encode(scope),
        );
        if let Some(resource) = resource {
            url.push_str("&resource=");
            url.push_str(&percent_encode(resource));
        }
        url
    }

    #[test]
    fn public_bind_requires_demo_login() {
        let err = serve_plan(8787, true, false, false, None, None).unwrap_err();
        assert_eq!(err, "--public requires --auth-profile");
    }

    #[test]
    fn container_listen_does_not_require_login() {
        let plan = serve_plan(8787, false, false, false, None, Some("0.0.0.0")).unwrap();
        assert_eq!(plan.bind_host, "0.0.0.0");
    }

    #[test]
    fn static_token_refuses_a_public_bind() {
        let err =
            serve_plan(8787, true, true, true, Some("http://demo.example"), None).unwrap_err();
        assert_eq!(err, "--demo-static-token is loopback only");
    }

    #[test]
    fn stored_credentials_are_sha256_not_the_raw_secret() {
        let (auth, _) = clock(Utc::now());
        let token = auth.issue_static_token();
        assert!(!auth.stores_raw_secret(&token));
        assert!(auth.authenticate(&token).is_ok());
    }

    #[test]
    fn every_tool_has_a_scope() {
        for name in crate::mcp::tool_names() {
            assert!(required_scope(&name).is_some(), "{name}");
        }
    }

    #[tokio::test]
    async fn authorization_server_metadata_advertises_s256() {
        let (auth, _) = clock(Utc::now());
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .uri("/.well-known/oauth-authorization-server/demo-oauth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"S256\""));
        assert!(body.contains("authorization_response_iss_parameter_supported"));
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["authorization_response_iss_parameter_supported"],
            true
        );
    }

    #[tokio::test]
    async fn authorize_without_resource_is_rejected() {
        let (auth, _) = clock(Utc::now());
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .uri(authorize_query(None, "mail.read"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_request"), "{body}");
    }

    #[tokio::test]
    async fn wrong_verifier_is_rejected() {
        let (auth, _) = clock(Utc::now());
        let code = consent(&auth, "mail.read mail.draft").await;
        let response = exchange(&auth, &code, "not-the-verifier").await;
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_grant"), "{body}");
    }

    #[tokio::test]
    async fn reused_authorization_code_is_rejected() {
        let (auth, _) = clock(Utc::now());
        let code = consent(&auth, "mail.read").await;
        let first = exchange(&auth, &code, "verifier-0123456789abcdef").await;
        assert_eq!(first.status(), StatusCode::OK);
        let second = exchange(&auth, &code, "verifier-0123456789abcdef").await;
        let (status, body, _) = body_of(second).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_grant"), "{body}");
    }

    #[tokio::test]
    async fn refresh_token_is_in_every_token_response() {
        let (auth, _) = clock(Utc::now());
        let code = consent(&auth, "mail.read mail.draft").await;
        let issued = exchange(&auth, &code, "verifier-0123456789abcdef").await;
        let (status, body, _) = body_of(issued).await;
        assert_eq!(status, StatusCode::OK);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        let refresh = parsed["refresh_token"].as_str().unwrap().to_string();
        assert!(parsed["access_token"].as_str().unwrap().len() > 20);
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/demo-oauth/token")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "grant_type=refresh_token&refresh_token={refresh}&client_id=demo-inspector"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::OK);
        let rotated: Value = serde_json::from_str(&body).unwrap();
        assert!(rotated["refresh_token"].as_str().unwrap() != refresh);
        assert!(rotated["access_token"].is_string());
    }

    #[tokio::test]
    async fn authorization_code_expires_after_sixty_seconds() {
        let start = Utc::now();
        let (auth, slot) = clock(start);
        let code = consent(&auth, "mail.read").await;
        *slot.lock().unwrap() = start + Duration::seconds(CODE_TTL_SECS);
        let response = exchange(&auth, &code, "verifier-0123456789abcdef").await;
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_grant"), "{body}");
    }

    #[tokio::test]
    async fn mcp_profile_unauthorized_includes_www_authenticate() {
        let (status, _, www) = unauthenticated(AuthMode::Mcp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let header = www.expect("WWW-Authenticate");
        assert!(header.contains("resource_metadata="));
        assert!(header.contains("mail.read mail.draft"));
    }

    #[tokio::test]
    async fn alexa_profile_unauthorized_omits_www_authenticate() {
        let (status, body, www) = unauthenticated(AuthMode::Alexa).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(www.is_none(), "{www:?}");
        assert!(body.contains("unauthenticated"));
    }

    #[tokio::test]
    async fn read_token_cannot_prepare_a_draft() {
        let (auth, _) = clock(Utc::now());
        let code = consent(&auth, "mail.read").await;
        let issued = exchange(&auth, &code, "verifier-0123456789abcdef").await;
        let (_, body, _) = body_of(issued).await;
        let token = serde_json::from_str::<Value>(&body).unwrap()["access_token"]
            .as_str()
            .unwrap()
            .to_string();
        let response = call_tool(&auth, &token, "prepare_reply_draft", "{}").await;
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body.contains("insufficient_scope"), "{body}");
        let briefing = call_tool(&auth, &token, "inbox_briefing", "{}").await;
        let (brief_status, brief_body, _) = body_of(briefing).await;
        assert_eq!(brief_status, StatusCode::OK, "{brief_body}");
    }

    #[tokio::test]
    async fn bearer_in_query_is_unauthorized() {
        let (auth, _) = clock(Utc::now());
        let token = auth.issue_static_token();
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/mcp?access_token={token}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_for_another_audience_is_unauthorized() {
        let (auth, _) = clock(Utc::now());
        let token = issue_access(&auth, "http://other.example", "mail.read mail.draft");
        let response = call_tool(&auth, &token, "inbox_briefing", "{}").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn foreign_origin_is_forbidden() {
        let (auth, _) = clock(Utc::now());
        let token = auth.issue_static_token();
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::ORIGIN, "https://evil.example")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn authenticated_get_mcp_is_not_allowed() {
        let (auth, _) = clock(Utc::now());
        let token = auth.issue_static_token();
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/mcp")
                    .header(header::HOST, "127.0.0.1:8787")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, body, _) = body_of(response).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body}");
    }

    async fn unauthenticated(mode: AuthMode) -> (StatusCode, String, Option<String>) {
        let auth = AuthState::open(mode, "http://127.0.0.1:8787");
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        body_of(response).await
    }

    async fn consent(auth: &AuthState, scope: &str) -> String {
        let page = router(auth.clone())
            .oneshot(
                Request::builder()
                    .uri(authorize_query(Some("http://127.0.0.1:8787"), scope))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, html, _) = body_of(page).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Link Tagos Mail (demo)"), "{html}");
        let posted = router(auth.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/demo-oauth/authorize")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "client_id=demo-inspector&redirect_uri={}&response_type=code&state=state-1&code_challenge={}&code_challenge_method=S256&resource={}&scope={}",
                        percent_encode("http://127.0.0.1:6276/oauth/callback"),
                        percent_encode(&challenge("verifier-0123456789abcdef")),
                        percent_encode("http://127.0.0.1:8787"),
                        percent_encode(scope),
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(posted.status(), StatusCode::SEE_OTHER);
        let location = posted
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        percent_decode(code)
    }

    async fn exchange(auth: &AuthState, code: &str, verifier: &str) -> Response {
        router(auth.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/demo-oauth/token")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "grant_type=authorization_code&code={}&redirect_uri={}&client_id=demo-inspector&code_verifier={}&resource={}",
                        percent_encode(code),
                        percent_encode("http://127.0.0.1:6276/oauth/callback"),
                        percent_encode(verifier),
                        percent_encode("http://127.0.0.1:8787"),
                    )))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn call_tool(auth: &AuthState, token: &str, name: &str, arguments: &str) -> Response {
        router(auth.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::HOST, "127.0.0.1:8787")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(format!(
                        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[allow(dead_code)]
    fn percent_decode(raw: &str) -> String {
        let bytes = raw.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }
}
