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
//!
//! Each feature's routes and handlers live in their own file (`traffic.rs`,
//! `market.rs`, ...); this one keeps the shared state, the token guard and
//! the error helpers.

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

use crate::access::{self, AgentActivity, AgentMode, Caller, SharedAgentSettings};
use crate::assistant::Conversations;
use crate::codec;
use crate::dialogs;
use crate::engine::{Engine, SendError};
use crate::intercept;
use crate::model::Exchange;
use crate::paths::Home;
use crate::chats;
use crate::project::Project;
use crate::proposal::Proposals;
use crate::ui::{self, LaunchCodes};

mod agents;
mod bench;
mod browser;
mod callbacks;
mod findings;
mod market;
mod programs;
mod proxy;
mod scans;
mod scope;
mod settings;
mod traffic;

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    token: String,
    agent_token: String,
    agents: Arc<AgentActivity>,
    agent_settings: Arc<SharedAgentSettings>,
    conversations: Arc<Conversations>,
    /// The background watcher that fills the Agents inbox.
    watch: Arc<crate::watch::Watch>,
    /// Serializes changes to the saved chats, which are read, changed and written whole.
    chats_lock: Arc<tokio::sync::Mutex<()>>,
    /// Edits agents suggested for Bench drafts, waiting for the user.
    proposals: Arc<Proposals>,
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
    let conversations: Arc<Conversations> = Arc::default();
    let watch = crate::watch::Watch::new(engine.clone(), home.clone(), conversations.clone());
    let state = AppState {
        engine,
        token: tokens.user,
        agent_token: tokens.agent,
        agents: Arc::default(),
        agent_settings: Arc::new(SharedAgentSettings::new(&home)),
        conversations,
        watch,
        chats_lock: Arc::default(),
        proposals: Arc::default(),
        api_addr,
        launch_codes: Arc::default(),
        home,
    };
    // The watcher looks only while agent access is on.
    if tokio::runtime::Handle::try_current().is_ok() {
        let settings = state.agent_settings.clone();
        tokio::spawn(state.watch.clone().run(move || settings.get().enabled));
    }
    // A turn still marked running was cut off when the last engine stopped.
    if let Err(e) = chats::settle(&state.engine.store) {
        tracing::warn!("the saved conversations could not be read: {e:#}");
    }
    let project_id = state.engine.project_ref.get().map(|p| p.id.clone()).unwrap_or_default();
    Router::new()
        .route("/", get(move || ui::index(project_id.clone())))
        .route("/ui/{*file}", get(ui::file))
        .route("/ui/guide/{file}", get(ui::guide_shot))
        .route("/ui/session", post(ui_session))
        .route("/api/ui/launch", post(ui_launch))
        .route("/api/status", get(status))
        .route("/api/usage", post(usage_screen))
        .route("/api/shutdown", post(shutdown))
        .merge(agents::routes())
        .merge(bench::routes())
        .merge(browser::routes())
        .merge(callbacks::routes())
        .merge(findings::routes())
        .merge(market::routes())
        .merge(programs::routes())
        .merge(proxy::routes())
        .merge(scans::routes())
        .merge(scope::routes())
        .merge(settings::routes())
        .merge(traffic::routes())
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        // `guard` already caps bodies at MAX_BODY; the extractors' own 2 MB default would refuse bigger HAR uploads.
        .layer(axum::extract::DefaultBodyLimit::disable())
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
        let query = req.uri().query().unwrap_or("").to_string();
        s.agents.record(&initiator(req.headers()), &method, &path, &query, checked.is_err());
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
    h.get("x-plonix-client").and_then(|v| v.to_str().ok()).unwrap_or("api").chars().take(64).collect()
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

/// The built-in tools switched on for this window. A project that already
/// follows a program keeps its Programs screen, so the rules it follows stay
/// in view even where the Programs tool was never installed.
fn tools_on(s: &AppState) -> std::collections::BTreeSet<String> {
    let mut on = crate::tool::ToolLibrary::new(&s.home).enabled_features();
    if s.engine.program().is_some() {
        on.insert("programs".into());
    }
    on
}

/// Each extension switched on, with how it runs: `scan`, `enumerate` or
/// `probe` for one that drives a program, `sandbox` for one with its own code.
fn extensions_on(s: &AppState) -> std::collections::BTreeMap<String, &'static str> {
    use crate::extension::Runner;
    use crate::program::Kind;
    s.engine
        .extensions()
        .extensions
        .iter()
        .map(|e| {
            let how = match &e.runner {
                Runner::Wasm(_) => "sandbox",
                Runner::Program(p) => match crate::program::get(p).map(|p| p.kind) {
                    Some(Kind::Enumerate) => "enumerate",
                    Some(Kind::Probe) => "probe",
                    _ => "scan",
                },
            };
            (e.name.clone(), how)
        })
        .collect()
}

async fn status(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    let rules = s.engine.rules();
    let pending = s.engine.store.suggestions(&rules).map(|v| v.len()).unwrap_or(0);
    let project = s.engine.project_ref.get();
    let mut v = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "project": s.engine.project,
        "project_id": project.map(|p| p.id.clone()),
        "project_dir": project.map(|p| p.dir.clone()),
        "demo": this_project(&s).is_some_and(|p| p.file.demo),
        "proxy": s.proxy_addr(),
        "api": s.api_addr.to_string(),
        "pid": std::process::id(),
        "started_at": s.engine.started_at,
        "exchanges": s.engine.store.count().unwrap_or(0),
        "scope_rules": rules.rules.len(),
        "pending_suggestions": pending,
        // Unread notes and leads in the Agents inbox, for the sidebar badge.
        "agent_inbox_unread": s.watch.unread(),
        "ca_fingerprint": s.engine.ca.fingerprint(),
        // Which built-in tools the Market has switched on (see crate::tool),
        // so the window shows the Access check tab and the Bench user
        // switcher only once they are installed.
        "tools": tools_on(&s),
        // The newest callback's number, so the sidebar can count new ones.
        "callbacks": s.engine.callbacks.latest(),
        // Extensions switched on, by what running one does, so screens offer
        // the ones that fit them (Find subdomains on Scope, Probe on a request).
        "extensions": extensions_on(&s),
    });
    // The window watches this to know when the held queue changes. Agents
    // learn nothing about Intercept.
    if is_user(&caller) {
        let i = &s.engine.intercept;
        v["intercept"] = json!({ "on": i.is_on(), "held": i.held(), "seq": i.seq() });
        // The app's own Open and Save dialogs, for HAR files.
        v["native_dialogs"] = json!(dialogs::get().is_some());
    }
    Json(v).into_response()
}

fn is_user(caller: &MaybeCaller) -> bool {
    caller.as_ref().is_some_and(|c| c.0 == Caller::User)
}

/// Intercept is the user's alone: agents are refused by the access check
/// already (these routes are in no mode's capabilities), and again here.
fn user_only(caller: &MaybeCaller) -> Option<Response> {
    (!is_user(caller)).then(|| err(StatusCode::FORBIDDEN, access::Refusal::NotAllowed.code(), access::Refusal::NotAllowed.message()))
}

fn bad_settings(problems: Vec<crate::settings::Problem>) -> Response {
    let msg = problems.iter().map(|p| format!("{}: {}", p.field, p.message)).collect::<Vec<_>>().join("; ");
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg, "code": "bad_settings", "problems": problems }))).into_response()
}

fn no_dialogs() -> Response {
    err(StatusCode::NOT_IMPLEMENTED, "no_dialogs", "file dialogs are only available in the Plonix app; download or upload the file instead")
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

/// How a refused or failed send reads over the API.
fn send_error(e: SendError) -> Response {
    let (status, code) = match &e {
        SendError::OutOfScope { .. } => (StatusCode::FORBIDDEN, "out_of_scope"),
        SendError::NotAllowed(_) => (StatusCode::FORBIDDEN, "program_rules"),
        SendError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
        SendError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        SendError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    match e {
        SendError::Other(e) => internal(e),
        e => err(status, code, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct UsageBody {
    event: String,
}

/// Counts a screen the window opened, for anonymous usage statistics (see
/// [`crate::usage`]). Only screen names are taken here.
async fn usage_screen(caller: MaybeCaller, Json(b): Json<UsageBody>) -> Response {
    if is_user(&caller) && b.event.starts_with("screen_") {
        crate::usage::record(&b.event);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn shutdown(State(s): State<AppState>) -> Response {
    s.engine.request_shutdown();
    Json(json!({ "ok": true })).into_response()
}

fn this_project(s: &AppState) -> Option<Project> {
    s.engine.project_ref.get().and_then(|p| Project::load(&p.dir).ok())
}
