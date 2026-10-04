//! Local HTTP API. Every client (GUI, CLI, MCP) goes through it.
//!
//! Bound to loopback, requires `Authorization: Bearer <token>` (token in
//! `$PLONIX_HOME/api-token`) and a loopback `Host` header, so that web pages
//! cannot drive it through the browser (CSRF / DNS rebinding).
//!
//! AI agents use a second token, `$PLONIX_HOME/agent-token`, and can only
//! call the routes their mode allows (see [`crate::access`]).
//!
//! The same address also serves the web UI (see [`crate::ui`]): its static
//! files need no token, and the page obtains one through a launch code.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::access::{self, AgentActivity, AgentMode, AgentSettings, Caller, Group, SharedAgentSettings};
use crate::ask::{self, AskError, AskRequest};
use crate::browser;
use crate::codec;
use crate::engine::{Engine, ReplayRequest, SendError, SendRequest};
use crate::model::{Exchange, NewFinding, SEVERITIES};
use crate::paths::Home;
use crate::project::Project;
use crate::query;
use crate::settings::{self, Level};
use crate::scope::Decision;
use crate::ui::{self, LaunchCodes};

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    token: String,
    agent_token: String,
    agents: Arc<AgentActivity>,
    agent_settings: Arc<SharedAgentSettings>,
    api_addr: SocketAddr,
    launch_codes: Arc<LaunchCodes>,
    home: Home,
}

impl AppState {
    fn proxy_addr(&self) -> String {
        self.engine.proxy_addr().map(|a| a.to_string()).unwrap_or_default()
    }
}

/// Bearer tokens the API accepts.
pub struct Tokens {
    /// The user's own clients: full access.
    pub user: String,
    /// AI agents: limited to [`AgentMode::current`].
    pub agent: String,
}

pub fn router(engine: Arc<Engine>, tokens: Tokens, api_addr: SocketAddr, home: Home) -> Router {
    let state = AppState {
        engine,
        token: tokens.user,
        agent_token: tokens.agent,
        agents: Arc::default(),
        agent_settings: Arc::new(SharedAgentSettings::new(&home)),
        api_addr,
        launch_codes: Arc::default(),
        home,
    };
    let project_id = state.engine.project_ref.get().map(|p| p.id.clone()).unwrap_or_default();
    Router::new()
        .route("/", get(move || ui::index(project_id.clone())))
        .route("/ui/app.js", get(ui::app_js))
        .route("/ui/settings.js", get(ui::settings_js))
        .route("/ui/app.css", get(ui::app_css))
        .route("/ui/icon.svg", get(ui::icon))
        .route("/ui/session", post(ui_session))
        .route("/api/ui/launch", post(ui_launch))
        .route("/api/status", get(status))
        .route("/api/traffic", get(traffic))
        .route("/api/traffic/facets", get(facets))
        .route("/api/traffic/{id}", get(exchange))
        .route("/api/traffic/{id}/insights", get(insights))
        .route("/api/views/{view}", get(view_state).put(set_view_state))
        .route("/api/hosts", get(hosts))
        .route("/api/hosts/{host}/endpoints", get(endpoints))
        .route("/api/tech", get(tech_all))
        .route("/api/tech/{host}", get(tech_host))
        .route("/api/rules", get(rule_packs))
        .route("/api/scope", get(scope))
        .route("/api/scope/accept", post(accept))
        .route("/api/scope/reject", post(reject))
        .route("/api/scope/remove", post(remove))
        .route("/api/browser/open", post(open_browser))
        .route("/api/send", post(send))
        .route("/api/replay", post(replay))
        .route("/api/findings", get(findings).post(add_finding))
        .route("/api/settings", get(get_settings))
        .route("/api/settings/{section}", put(put_settings))
        .route("/api/storage", get(storage))
        .route("/api/storage/prune", post(prune))
        .route("/api/sessions", get(sessions))
        .route("/api/scan/catalog", get(scan_catalog))
        .route("/api/scan/suggest/{host}", get(scan_suggest))
        .route("/api/scan", post(scan_run))
        .route("/api/agents", get(agents))
        .route("/api/agents/settings", get(agent_settings).put(put_agent_settings))
        .route("/api/agents/ask", post(agent_ask))
        .route("/api/agents/launch", post(agent_launch))
        .route("/api/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Largest request body the API accepts.
const MAX_BODY: usize = 64 * 1024 * 1024;

async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    // Read the whole body before answering, even for refusals and handlers
    // that ignore it: closing a connection with unread bytes resets it, and
    // the client can lose the response it was reading.
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "the request body is too large or was cut off"),
    };
    let req = Request::from_parts(parts, axum::body::Body::from(body));
    let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or("");
    let host_ok = host.rsplit_once(':').is_some_and(|(h, p)| {
        p.parse::<u16>().ok() == Some(s.api_addr.port()) && matches!(h, "127.0.0.1" | "localhost" | "[::1]")
    });
    if !host_ok {
        return err(StatusCode::FORBIDDEN, "bad_host", "requests must target the loopback API address");
    }
    // The UI's static files and the launch-code exchange carry no token.
    if !req.uri().path().starts_with("/api/") {
        return next.run(req).await;
    }
    let auth = req.headers().get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
    let given = auth.strip_prefix("Bearer ").map(|t| t.trim().as_bytes()).unwrap_or_default();
    let caller = if !given.is_empty() && constant_eq(given, s.token.as_bytes()) {
        Caller::User
    } else if !given.is_empty() && constant_eq(given, s.agent_token.as_bytes()) {
        Caller::Agent
    } else {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "missing or wrong API token (see $PLONIX_HOME/api-token)");
    };
    if caller == Caller::Agent {
        let mode = AgentMode::current();
        let (method, path) = (req.method().as_str().to_string(), req.uri().path().to_string());
        let checked = access::check(mode, &s.agent_settings.get(), &method, &path);
        s.agents.record(&initiator(req.headers()), &method, &path, checked.is_err());
        if let Err(refusal) = checked {
            return err(StatusCode::FORBIDDEN, refusal.code(), refusal.message());
        }
    }
    let mut req = req;
    req.extensions_mut().insert(caller);
    next.run(req).await
}

type MaybeCaller = Option<axum::Extension<Caller>>;

/// Whether this request comes from an agent that may only see in-scope hosts.
fn agent_in_scope_only(s: &AppState, caller: &MaybeCaller) -> bool {
    caller.as_ref().is_some_and(|c| c.0 == Caller::Agent) && s.agent_settings.get().in_scope_only()
}

fn outside_agent_data() -> Response {
    err(
        StatusCode::FORBIDDEN,
        "outside_agent_data",
        "this host is not in scope, and the user lets agents see in-scope traffic only (Settings › AI agents)",
    )
}

pub(crate) fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn err(status: StatusCode, code: &str, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "code": code }))).into_response()
}

pub(crate) fn internal(e: anyhow::Error) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal", &format!("{e:#}"))
}

fn initiator(h: &HeaderMap) -> String {
    h.get("x-plonix-client").and_then(|v| v.to_str().ok()).unwrap_or("api").chars().take(32).collect()
}

/// Issues a one-time code that opens the web UI already signed in.
async fn ui_launch(State(s): State<AppState>) -> Response {
    match s.launch_codes.issue() {
        Ok(code) => Json(json!({
            "url": format!("http://{}/#code={code}", s.api_addr),
            "code": code,
            "expires_in": ui::CODE_TTL.as_secs(),
        }))
        .into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct SessionBody {
    code: String,
}

/// Trades a launch code for the API token. Only the UI's own origin may ask:
/// the JSON body forces a CORS preflight that other origins cannot pass, and
/// a present `Origin` must be this address.
async fn ui_session(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<SessionBody>) -> Response {
    if let Some(origin) = headers.get("origin").and_then(|o| o.to_str().ok()) {
        let host = origin.strip_prefix("http://").unwrap_or("");
        let ok = host.rsplit_once(':').is_some_and(|(h, p)| {
            p.parse::<u16>().ok() == Some(s.api_addr.port()) && matches!(h, "127.0.0.1" | "localhost" | "[::1]")
        });
        if !ok {
            return err(StatusCode::FORBIDDEN, "bad_origin", "the UI session must be requested by the Plonix UI");
        }
    }
    if s.launch_codes.redeem(b.code.trim()) {
        Json(json!({ "token": s.token })).into_response()
    } else {
        err(
            StatusCode::UNAUTHORIZED,
            "bad_code",
            "this link has expired or was already used; run `plonix ui` to open Plonix again",
        )
    }
}

async fn status(State(s): State<AppState>) -> Response {
    let rules = s.engine.rules();
    let pending = s.engine.store.suggestions(&rules).map(|v| v.len()).unwrap_or(0);
    let project = s.engine.project_ref.get();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "project": s.engine.project,
        "project_id": project.map(|p| p.id.clone()),
        "project_dir": project.map(|p| p.dir.clone()),
        "proxy": s.proxy_addr(),
        "api": s.api_addr.to_string(),
        "pid": std::process::id(),
        "started_at": s.engine.started_at,
        "exchanges": s.engine.store.count().unwrap_or(0),
        "scope_rules": rules.rules.len(),
        "pending_suggestions": pending,
        "ca_fingerprint": s.engine.ca.fingerprint(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct TrafficParams {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_limit() -> usize {
    100
}

async fn traffic(State(s): State<AppState>, caller: MaybeCaller, Query(p): Query<TrafficParams>) -> Response {
    let q = if agent_in_scope_only(&s, &caller) { format!("{} scope:in", p.q) } else { p.q };
    let q = match query::Query::parse(&q) {
        Ok(q) => q,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_query", &e.to_string()),
    };
    match s.engine.store.search(&q, &s.engine.rules(), p.limit.min(5000), p.offset) {
        Ok((items, total)) => Json(json!({ "total": total, "items": items })).into_response(),
        Err(e) => internal(e),
    }
}

/// Full exchange plus decoded text bodies for display.
#[derive(Serialize)]
pub struct ExchangeView {
    #[serde(flatten)]
    pub exchange: Exchange,
    pub url: String,
    pub in_scope: bool,
    pub req_text: Option<String>,
    pub resp_text: Option<String>,
}

pub fn view(ex: Exchange, in_scope: bool) -> ExchangeView {
    ExchangeView {
        url: ex.url(),
        in_scope,
        req_text: codec::body_text(&ex.req_headers, &ex.req_body),
        resp_text: codec::body_text(&ex.resp_headers, &ex.resp_body),
        exchange: ex,
    }
}

async fn exchange(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    match s.engine.store.get_exchange(id) {
        Ok(Some(ex)) => {
            let in_scope = s.engine.rules().in_scope(&ex.host);
            if !in_scope && agent_in_scope_only(&s, &caller) {
                return outside_agent_data();
            }
            Json(view(ex, in_scope)).into_response()
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} not found")),
        Err(e) => internal(e),
    }
}

/// What stands out in one exchange: tokens to decode, personal data, secrets.
async fn insights(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    if agent_in_scope_only(&s, &caller)
        && let Ok(Some(ex)) = s.engine.store.get_exchange(id)
        && !s.engine.rules().in_scope(&ex.host)
    {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    let found = tokio::task::spawn_blocking(move || {
        engine.store.get_exchange(id).map(|ex| ex.map(|ex| crate::insight::analyze(&ex, crate::insight::detectors())))
    })
    .await;
    match found {
        Ok(Ok(Some(list))) => Json(list).into_response(),
        Ok(Ok(None)) => err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} not found")),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// UI state saved with the project, such as a view's include/exclude filters,
/// so it survives reloads and is the same in every window.
async fn view_state(State(s): State<AppState>, Path(view): Path<String>) -> Response {
    if !valid_view(&view) {
        return err(StatusCode::BAD_REQUEST, "bad_request", "unknown view name");
    }
    match s.engine.store.view_state(&view) {
        Ok(state) => Json(state.unwrap_or_else(|| json!({}))).into_response(),
        Err(e) => internal(e),
    }
}

async fn set_view_state(State(s): State<AppState>, Path(view): Path<String>, Json(state): Json<Value>) -> Response {
    if !valid_view(&view) {
        return err(StatusCode::BAD_REQUEST, "bad_request", "unknown view name");
    }
    if !state.is_object() || state.to_string().len() > 64 * 1024 {
        return err(StatusCode::BAD_REQUEST, "bad_request", "state must be a JSON object under 64 KB");
    }
    match s.engine.store.set_view_state(&view, &state) {
        Ok(()) => Json(state).into_response(),
        Err(e) => internal(e),
    }
}

fn valid_view(view: &str) -> bool {
    !view.is_empty() && view.len() <= 40 && view.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn hosts(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    match s.engine.store.hosts(&s.engine.rules()) {
        Ok(mut h) => {
            if agent_in_scope_only(&s, &caller) {
                h.retain(|h| h.scope == Decision::Accepted);
            }
            Json(h).into_response()
        }
        Err(e) => internal(e),
    }
}

async fn facets(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.store.facets(&engine.rules())).await {
        Ok(Ok(f)) => Json(f).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn endpoints(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    match s.engine.store.endpoints(&host) {
        Ok(e) => Json(e).into_response(),
        Err(e) => internal(e),
    }
}

async fn tech_all(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.detect_all()).await {
        Ok(Ok(mut hosts)) => {
            if agent_in_scope_only(&s, &caller) {
                let rules = s.engine.rules();
                hosts.retain(|h| rules.in_scope(&h.host));
            }
            Json(hosts).into_response()
        }
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn tech_host(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    let h = host.clone();
    match tokio::task::spawn_blocking(move || engine.detect_host(&h)).await {
        Ok(Ok(tech)) => Json(json!({ "host": host.to_ascii_lowercase(), "tech": tech })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn scan_catalog(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.scan_catalog().describe()).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => internal(e.into()),
    }
}

async fn scan_suggest(State(s): State<AppState>, Path(host): Path<String>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.scan_suggest(&host)).await {
        Ok(Ok(suggestion)) => Json(suggestion).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn scan_run(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<crate::scan::ScanRequest>) -> Response {
    match s.engine.scan(req, &initiator(&headers)).await {
        Ok(report) => Json(report).into_response(),
        Err(e @ SendError::OutOfScope { .. }) => err(StatusCode::FORBIDDEN, "out_of_scope", &e.to_string()),
        Err(SendError::Other(e)) => internal(e),
        Err(e) => err(StatusCode::BAD_REQUEST, "bad_request", &e.to_string()),
    }
}

async fn rule_packs(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.detection_rules()).await {
        Ok(r) => Json(json!({ "packs": r.packs, "rules": r.detector.rules.len(), "problems": r.problems })).into_response(),
        Err(e) => internal(e.into()),
    }
}

async fn scope(State(s): State<AppState>) -> Response {
    let rules = s.engine.rules();
    match s.engine.store.suggestions(&rules) {
        Ok(sug) => Json(json!({ "rules": rules.rules, "suggestions": sug })).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct DomainBody {
    domain: String,
    #[serde(default)]
    include_subdomains: bool,
    #[serde(default)]
    note: String,
}

async fn decide(s: AppState, b: DomainBody, d: Decision) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.decide(&b.domain, d, b.include_subdomains, &b.note)).await {
        Ok(Ok(rule)) => Json(rule).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_domain", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

async fn accept(State(s): State<AppState>, Json(b): Json<DomainBody>) -> Response {
    decide(s, b, Decision::Accepted).await
}

async fn reject(State(s): State<AppState>, Json(b): Json<DomainBody>) -> Response {
    decide(s, b, Decision::Rejected).await
}

async fn remove(State(s): State<AppState>, Json(b): Json<DomainBody>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.remove_rule(&b.domain)).await {
        Ok(Ok(removed)) => Json(json!({ "removed": removed })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct OpenBody {
    target: String,
    /// Accept the target's domain (and subdomains) into scope first.
    #[serde(default = "yes")]
    scope: bool,
}

fn yes() -> bool {
    true
}

/// Opens the capture browser at a target: an isolated browser profile that
/// routes through this engine's proxy and trusts its CA.
async fn open_browser(State(s): State<AppState>, Json(b): Json<OpenBody>) -> Response {
    let target = match browser::parse_target(&b.target) {
        Ok(t) => t,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_target", &format!("{e:#}")),
    };
    let mut rule = Value::Null;
    if b.scope {
        let engine = s.engine.clone();
        let host = target.host.clone();
        match tokio::task::spawn_blocking(move || engine.decide(&host, Decision::Accepted, true, "")).await {
            Ok(Ok(r)) => rule = json!(r),
            Ok(Err(e)) => return err(StatusCode::BAD_REQUEST, "bad_target", &format!("{e:#}")),
            Err(e) => return internal(e.into()),
        }
    }
    let Some(found) = browser::detect() else {
        return err(
            StatusCode::NOT_FOUND,
            "no_browser",
            &format!(
                "no browser found to launch. Install Google Chrome, Brave, Edge or Firefox, or set any browser's HTTP and HTTPS proxy to {}",
                s.proxy_addr()
            ),
        );
    };
    let profile = browser::profile_dir(&s.home, s.engine.project_ref.get().map(|p| p.dir.as_path()));
    let launched = browser::launch(&profile, &found, &s.proxy_addr(), &s.engine.ca.spki_sha256(), &target.url);
    match launched {
        Ok(_) => Json(json!({
            "url": target.url,
            "host": target.host,
            "browser": found.name,
            "needs_trust": found.kind == browser::Kind::Firefox,
            "scope": rule,
        }))
        .into_response(),
        Err(e) => internal(e),
    }
}

fn send_result(s: &AppState, r: Result<Exchange, SendError>) -> Response {
    match r {
        Ok(ex) => {
            let in_scope = s.engine.rules().in_scope(&ex.host);
            Json(view(ex, in_scope)).into_response()
        }
        Err(e @ SendError::OutOfScope { .. }) => err(StatusCode::FORBIDDEN, "out_of_scope", &e.to_string()),
        Err(e @ SendError::BadRequest(_)) => err(StatusCode::BAD_REQUEST, "bad_request", &e.to_string()),
        Err(e @ SendError::NotFound(_)) => err(StatusCode::NOT_FOUND, "not_found", &e.to_string()),
        Err(SendError::Other(e)) => internal(e),
    }
}

async fn send(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<SendRequest>) -> Response {
    let r = s.engine.send(req, &initiator(&headers)).await;
    send_result(&s, r)
}

async fn replay(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<ReplayRequest>) -> Response {
    let r = s.engine.replay(req, &initiator(&headers)).await;
    send_result(&s, r)
}

async fn findings(State(s): State<AppState>) -> Response {
    match s.engine.store.findings() {
        Ok(f) => Json(f).into_response(),
        Err(e) => internal(e),
    }
}

async fn add_finding(State(s): State<AppState>, headers: HeaderMap, Json(f): Json<NewFinding>) -> Response {
    if f.title.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "bad_request", "title is required");
    }
    if !SEVERITIES.contains(&f.severity.as_str()) {
        return err(StatusCode::BAD_REQUEST, "bad_request", &format!("severity must be one of {}", SEVERITIES.join(", ")));
    }
    match s.engine.store.add_finding(&f, &initiator(&headers)) {
        Ok(f) => (StatusCode::CREATED, Json(f)).into_response(),
        Err(e) => internal(e),
    }
}

/// The agent access policy and which agents have connected.
async fn agents(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    let mode = AgentMode::current();
    let settings = s.agent_settings.get();
    let mut v = json!({
        "mode": mode,
        "enabled": settings.enabled,
        "data": settings.data,
        "capabilities": access::effective(mode, &settings),
        "not_allowed": access::not_allowed(mode),
        "connect": {
            "command": "plonix connect claude",
            "server": { "command": "plonix", "args": ["mcp"] },
        },
    });
    // Only the user sees who else is connected.
    if caller.is_some_and(|c| c.0 == Caller::User) {
        v["clients"] = json!(s.agents.clients());
    }
    Json(v).into_response()
}

async fn agent_settings(State(s): State<AppState>) -> Response {
    let settings = s.agent_settings.get();
    let groups: Vec<Value> = Group::SWITCHABLE.iter().map(|(g, label)| json!({ "group": g, "label": label, "on": settings.group_on(*g) })).collect();
    Json(json!({ "settings": settings, "groups": groups, "budgets": AgentSettings::BUDGETS })).into_response()
}

/// Changes agent access. Agents cannot reach this route (it is in no mode's
/// capabilities), so only the user changes what agents may see.
async fn put_agent_settings(State(s): State<AppState>, Json(new): Json<AgentSettings>) -> Response {
    if let Err(e) = s.agent_settings.set(new) {
        return internal(e);
    }
    agent_settings(State(s)).await
}

/// Builds the context for "Ask Claude Code" about one request, finding or host.
async fn agent_ask(State(s): State<AppState>, Json(req): Json<AskRequest>) -> Response {
    let settings = s.agent_settings.get();
    if !settings.enabled {
        return err(StatusCode::FORBIDDEN, "agents_disabled", "agent access is turned off in Settings › AI agents");
    }
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || ask::build(&engine, &req, &settings)).await {
        Ok(Ok(bundle)) => Json(bundle).into_response(),
        Ok(Err(AskError::NotFound(what))) => err(StatusCode::NOT_FOUND, "not_found", &format!("{what} not found")),
        Ok(Err(AskError::Other(e))) => internal(e),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct LaunchBody {
    prompt: String,
}

/// Opens Claude Code in Terminal with the prompt the user reviewed.
async fn agent_launch(State(s): State<AppState>, Json(b): Json<LaunchBody>) -> Response {
    if b.prompt.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "bad_request", "the prompt is empty");
    }
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || ask::launch_in_terminal(&home, &b.prompt)).await {
        Ok(Ok(_)) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) if format!("{e:#}").starts_with("unsupported") => err(StatusCode::NOT_IMPLEMENTED, "unsupported", &format!("{e:#}")),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn shutdown(State(s): State<AppState>) -> Response {
    s.engine.request_shutdown();
    Json(json!({ "ok": true })).into_response()
}

fn this_project(s: &AppState) -> Option<Project> {
    s.engine.project_ref.get().and_then(|p| Project::load(&p.dir).ok())
}

/// Every settings section with its values: global ones, and this project's.
async fn get_settings(State(s): State<AppState>) -> Response {
    let project = this_project(&s);
    let mut v = settings::describe(&s.home, project.as_ref().map(|p| &p.file.settings));
    v["project"] = json!(project.map(|p| json!({ "id": p.id(), "name": p.name(), "dir": p.dir })));
    Json(v).into_response()
}

#[derive(Deserialize)]
struct SettingsBody {
    values: Value,
}

/// Saves one section. Proxy changes apply to the running session at once.
async fn put_settings(State(s): State<AppState>, Path(id): Path<String>, Json(b): Json<SettingsBody>) -> Response {
    let Some(section) = settings::section(&id) else {
        return err(StatusCode::NOT_FOUND, "not_found", &format!("no settings section '{id}'"));
    };
    let mut project = this_project(&s);
    let current = match (section.level, &project) {
        (Level::Global, _) => settings::global(&s.home, &id),
        (Level::Project, Some(p)) => p.settings(&id),
        (Level::Project, None) => return err(StatusCode::CONFLICT, "no_project", "this engine has no project folder to save settings in"),
    };
    let values = match section.check(&b.values, &current) {
        Ok(v) => v,
        Err(problems) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "some settings need fixing", "code": "bad_settings", "problems": problems })),
            )
                .into_response();
        }
    };
    if id == settings::PROXY {
        // Apply first: a listen address that cannot be bound is not saved.
        let p = settings::ProxySettings::from_values(&values);
        if let Err(e) = s.engine.apply_proxy_settings(&p).await {
            let problems = [settings::Problem::new("listen_port", format!("{e:#}"))];
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("{e:#}"), "code": "bad_settings", "problems": problems })))
                .into_response();
        }
        crate::session::refresh(&s.home, &s.engine, s.api_addr);
    }
    let saved = match (section.level, project.as_mut()) {
        (Level::Global, _) => settings::save_global(&s.home, &id, &values),
        (Level::Project, Some(p)) => p.save_settings(&id, values.clone()),
        (Level::Project, None) => unreachable!(),
    };
    if let Err(e) = saved {
        return internal(e);
    }
    Json(json!({ "section": id, "values": values, "applies": section.applies, "proxy": s.proxy_addr() })).into_response()
}

/// How much traffic is out of scope, and the storage policy.
async fn storage(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    let stats = match tokio::task::spawn_blocking(move || engine.storage_stats()).await {
        Ok(Ok(st)) => st,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e.into()),
    };
    let project = this_project(&s);
    let policy = project.as_ref().map(|p| settings::StorageSettings::from_values(&p.settings(settings::STORAGE)));
    Json(json!({
        "stats": stats,
        "keep_only_in_scope": policy.is_some_and(|p| p.keep_only_in_scope),
        "last_prune": project.and_then(|p| p.file.last_prune),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct PruneBody {
    /// Must be true: deleting traffic is never a side effect of a stray request.
    #[serde(default)]
    confirm: bool,
}

/// Deletes out-of-scope traffic now.
async fn prune(State(s): State<AppState>, Json(b): Json<PruneBody>) -> Response {
    if !b.confirm {
        return err(StatusCode::BAD_REQUEST, "bad_request", "send {\"confirm\": true} to delete out-of-scope traffic");
    }
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.prune_out_of_scope()).await {
        Ok(Ok(report)) => {
            if let Some(mut p) = this_project(&s) {
                let _ = p.update(|f| f.last_prune = Some(report.clone()));
            }
            Json(report).into_response()
        }
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// Every running session (this one included), so a window can switch.
async fn sessions(State(s): State<AppState>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || crate::session::running(&home)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => internal(e.into()),
    }
}
