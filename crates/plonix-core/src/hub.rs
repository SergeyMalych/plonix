//! The Start screen: lists projects, creates them and opens them.
//!
//! The hub is a small loopback server, like an engine's API, that serves
//! the Start screen and hosts the sessions it opens. Plonix.app runs one
//! in its own process; `plonix launcher` runs one in the background for
//! the web version. Each project it opens is a [`Session`] with its own
//! engine, so several projects run at the same time without sharing ports,
//! databases or state.
//!
//! Projects already open elsewhere (another app window, `plonix start`)
//! are not opened twice: the hub signs the window in to the engine that
//! already serves them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify, broadcast};

use crate::api::{constant_eq, err, internal};
use crate::paths::{EngineInfo, Home, write_atomic};
use crate::project::{self, Project};
use crate::session::{self, OpenOptions, Session};
use crate::settings::{self, Level};
use crate::ui::{self, LaunchCodes};

pub const DEFAULT_HUB_PORT: u16 = 8070;

/// What happened to a project the hub knows about.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HubEvent {
    Opened { project_id: String, api: String },
    Closed { project_id: String },
}

pub struct Hub {
    pub home: Home,
    pub addr: SocketAddr,
    token: String,
    launch_codes: LaunchCodes,
    hosted: Mutex<HashMap<String, Session>>,
    pub events: broadcast::Sender<HubEvent>,
    stop: Notify,
    /// Options for every session this hub opens (tests trust extra roots).
    pub session_options: std::sync::Mutex<OpenOptions>,
}

/// Announced in `$PLONIX_HOME/hub.json` while a hub runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    pub pid: u32,
    pub url: String,
}

/// Starts a hub on the preferred port (or any free one) and serves it in
/// the background.
pub async fn start(home: &Home, port: Option<u16>) -> Result<Arc<Hub>> {
    home.ensure()?;
    crate::usage::init(home, true);
    let token = home.load_or_create_token()?;
    let mut listener = None;
    for p in [port, Some(DEFAULT_HUB_PORT)].into_iter().flatten() {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", p)).await {
            listener = Some(l);
            break;
        }
    }
    let listener = match listener {
        Some(l) => l,
        None => TcpListener::bind(("127.0.0.1", 0)).await.context("binding the Start screen")?,
    };
    let addr = listener.local_addr()?;
    let hub = Arc::new(Hub {
        home: home.clone(),
        addr,
        token,
        launch_codes: LaunchCodes::default(),
        hosted: Mutex::default(),
        events: broadcast::channel(64).0,
        stop: Notify::new(),
        session_options: Default::default(),
    });
    let router = router(hub.clone());
    let stopping = hub.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).with_graceful_shutdown(async move { stopping.stop.notified().await }).await;
    });
    Ok(hub)
}

impl Hub {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A one-time address that opens the Start screen signed in.
    pub fn launch_url(&self) -> Result<String> {
        Ok(format!("{}/#code={}", self.url(), self.launch_codes.issue()?))
    }

    /// Announces this hub in `hub.json`, for `plonix launcher`.
    pub fn announce(&self) -> Result<()> {
        let info = HubInfo { pid: std::process::id(), url: self.url() };
        write_atomic(&self.home.hub_file(), &serde_json::to_vec_pretty(&info)?)
    }

    /// Opens a project and returns a one-time address for its window. A
    /// project already open, here or in another process, is not opened again.
    pub async fn open(self: &Arc<Self>, project_id: &str) -> Result<Opened> {
        let entry = project::find(&self.home, project_id).ok_or_else(|| anyhow!("no project with id {project_id}"))?;
        let mut hosted = self.hosted.lock().await;
        if let Some(s) = hosted.get(project_id) {
            let url = engine_launch_url(&s.info().api, &self.token).await?;
            return Ok(Opened { url, info: s.info(), started: false });
        }
        if let Some(info) = session::find(&self.home, project_id) {
            let url = engine_launch_url(&info.api, &self.token).await?;
            return Ok(Opened { url, info, started: false });
        }
        let project = Project::load(&entry.path)?;
        let options = self.session_options.lock().unwrap().clone();
        let session = session::open(&self.home, project, options).await?;
        let info = session.info();
        let engine = session.engine.clone();
        hosted.insert(project_id.to_string(), session);
        drop(hosted);
        let _ = self.events.send(HubEvent::Opened { project_id: project_id.to_string(), api: info.api.clone() });

        // Close the session when its engine is told to stop, from anywhere.
        let hub = self.clone();
        let id = project_id.to_string();
        tokio::spawn(async move {
            engine.stopped().await;
            hub.finish(&id).await;
        });
        let url = engine_launch_url(&info.api, &self.token).await?;
        Ok(Opened { url, info, started: true })
    }

    /// Closes a project hosted by this hub, or asks the process serving it to.
    pub async fn close(&self, project_id: &str) -> Result<bool> {
        let hosted = self.hosted.lock().await.get(project_id).map(|s| s.engine.clone());
        if let Some(engine) = hosted {
            engine.request_shutdown();
            // `finish` runs from the watcher; wait for it so callers see the result.
            for _ in 0..200 {
                if !self.hosted.lock().await.contains_key(project_id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            return Ok(true);
        }
        match session::find(&self.home, project_id) {
            Some(info) => {
                engine_call(&info.api, &self.token, "POST", "/api/shutdown", json!({})).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn finish(&self, project_id: &str) {
        let Some(session) = self.hosted.lock().await.remove(project_id) else { return };
        match tokio::task::spawn_blocking(move || session.close()).await {
            Ok(Err(e)) => tracing::error!("closing project {project_id}: {e:#}"),
            Err(e) => tracing::error!("closing project {project_id}: {e}"),
            Ok(Ok(_)) => {}
        }
        let _ = self.events.send(HubEvent::Closed { project_id: project_id.to_string() });
    }

    /// Closes every hosted session and stops serving the Start screen.
    pub async fn shutdown(&self) {
        let ids: Vec<String> = self.hosted.lock().await.keys().cloned().collect();
        for id in ids {
            let _ = self.close(&id).await;
        }
        self.stop.notify_waiters();
        crate::usage::flush();
        if self.home.read_hub().is_some_and(|h| h.pid == std::process::id() && h.url == self.url()) {
            let _ = std::fs::remove_file(self.home.hub_file());
        }
    }

    /// Projects hosted by this hub.
    pub async fn hosted(&self) -> Vec<EngineInfo> {
        self.hosted.lock().await.values().map(Session::info).collect()
    }

    /// The hosted session serving an API address, if any.
    pub async fn project_for_api(&self, api: &str) -> Option<EngineInfo> {
        self.hosted.lock().await.values().map(Session::info).find(|i| i.api == api)
    }

    pub async fn stopped(&self) {
        self.stop.notified().await
    }
}

pub struct Opened {
    pub url: String,
    pub info: EngineInfo,
    pub started: bool,
}

impl Home {
    pub fn read_hub(&self) -> Option<HubInfo> {
        serde_json::from_slice(&std::fs::read(self.hub_file()).ok()?).ok()
    }
}

/// Calls an engine's API with the user's token.
async fn engine_call(api: &str, token: &str, method: &str, path: &str, body: Value) -> Result<Value> {
    let url = format!("{api}{path}");
    let auth = format!("Bearer {token}");
    let method = method.to_string();
    tokio::task::spawn_blocking(move || {
        let req = ureq::request(&method, &url).set("Authorization", &auth).set("X-Plonix-Client", "hub").timeout(Duration::from_secs(10));
        match req.send_json(body) {
            Ok(r) => Ok(r.into_json()?),
            Err(ureq::Error::Status(_, r)) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                Err(anyhow::Error::from(ApiFailure(v)))
            }
            Err(e) => Err(anyhow!("the project's engine did not answer: {e}")),
        }
    })
    .await?
}

/// An error an engine answered with, passed on as is.
#[derive(Debug)]
struct ApiFailure(Value);
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0["error"].as_str().unwrap_or("request failed"))
    }
}
impl std::error::Error for ApiFailure {}

async fn engine_launch_url(api: &str, token: &str) -> Result<String> {
    let v = engine_call(api, token, "POST", "/api/ui/launch", json!({})).await?;
    v["url"].as_str().map(String::from).context("the engine did not return a window address")
}

// ---- HTTP ------------------------------------------------------------------

fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/", get(ui::launcher))
        .route("/ui/{*file}", get(ui::file))
        .route("/ui/session", post(ui_session))
        .route("/api/ui/launch", post(ui_launch))
        .route("/api/ui/style", get(ui_style).put(put_ui_style))
        .route("/api/hub", get(about))
        .route("/api/terms", get(terms).post(accept_terms))
        .route("/api/starter", get(starter).post(install_starter))
        .route("/api/projects", get(projects).post(create))
        .route("/api/projects/add", post(add_existing))
        .route("/api/projects/demo", post(demo))
        .route("/api/projects/{id}/open", post(open))
        .route("/api/projects/{id}/close", post(close))
        .route("/api/projects/{id}/forget", post(forget))
        .route("/api/projects/{id}/reveal", post(reveal))
        .route("/api/projects/{id}/settings", get(project_settings))
        .route("/api/projects/{id}/settings/{section}", put(put_project_settings))
        .route("/api/settings", get(global_settings))
        .route("/api/settings/{section}", put(put_global_settings))
        .route("/api/pick-folder", post(pick_folder))
        .route("/api/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(hub.clone(), guard))
        .with_state(hub)
}

fn loopback(hostport: &str, port: u16) -> bool {
    hostport.rsplit_once(':').is_some_and(|(h, p)| p.parse::<u16>().ok() == Some(port) && matches!(h, "127.0.0.1" | "localhost" | "[::1]"))
}

async fn guard(State(hub): State<Arc<Hub>>, req: Request, next: Next) -> Response {
    // Read the body first, so refusals never close a connection with unread
    // bytes (which resets it and can lose the response).
    let (parts, body) = req.into_parts();
    let Ok(body) = axum::body::to_bytes(body, 1024 * 1024).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "the request body is too large or was cut off");
    };
    let req = Request::from_parts(parts, axum::body::Body::from(body));
    let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or("");
    if !loopback(host, hub.addr.port()) {
        return err(StatusCode::FORBIDDEN, "bad_host", "requests must target the loopback Start screen address");
    }
    if !req.uri().path().starts_with("/api/") {
        return next.run(req).await;
    }
    let auth = req.headers().get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
    if !auth.strip_prefix("Bearer ").is_some_and(|t| constant_eq(t.trim().as_bytes(), hub.token.as_bytes())) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "missing or wrong API token (see $PLONIX_HOME/api-token)");
    }
    next.run(req).await
}

async fn ui_launch(State(hub): State<Arc<Hub>>) -> Response {
    match hub.launch_url() {
        Ok(url) => Json(json!({ "url": url })).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct SessionBody {
    code: String,
}

async fn ui_session(State(hub): State<Arc<Hub>>, headers: HeaderMap, Json(b): Json<SessionBody>) -> Response {
    if let Some(origin) = headers.get("origin").and_then(|o| o.to_str().ok())
        && !loopback(origin.strip_prefix("http://").unwrap_or(""), hub.addr.port())
    {
        return err(StatusCode::FORBIDDEN, "bad_origin", "the session must be requested by the Plonix Start screen");
    }
    if hub.launch_codes.redeem(b.code.trim()) {
        Json(json!({ "token": hub.token })).into_response()
    } else {
        err(StatusCode::UNAUTHORIZED, "bad_code", "this link has expired or was already used; open Plonix again")
    }
}

async fn about(State(hub): State<Arc<Hub>>) -> Response {
    let interface = settings::InterfaceSettings::load(&hub.home);
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "projects_dir": hub.home.default_projects_dir(),
        "home_dir": crate::paths::user_home(),
        "can_pick_folder": folder_picker().is_some(),
        "open_in_browser": interface.open_in_browser,
    }))
    .into_response()
}

/// The license and terms, and whether they still need accepting.
async fn terms(State(hub): State<Arc<Hub>>) -> Response {
    Json(json!({
        "accepted": crate::terms::accepted(&hub.home),
        "version": crate::terms::VERSION,
        "terms": crate::terms::TERMS,
        "license": crate::terms::LICENSE,
        "terms_url": crate::terms::TERMS_URL,
        "privacy_url": crate::terms::PRIVACY_URL,
        "share_usage": crate::usage::sharing(&hub.home),
        "usage_disabled_by_env": crate::usage::disabled_by_env(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct AcceptBody {
    accept: bool,
    #[serde(default)]
    share_usage: bool,
}

/// Records the first-launch answer: the terms, and whether to share usage statistics.
async fn accept_terms(State(hub): State<Arc<Hub>>, Json(b): Json<AcceptBody>) -> Response {
    if !b.accept {
        return err(StatusCode::BAD_REQUEST, "bad_request", "accept the license and terms to continue");
    }
    let r = crate::usage::set_sharing(&hub.home, b.share_usage).and_then(|_| crate::terms::accept(&hub.home));
    match r {
        Ok(()) => {
            crate::usage::flush();
            Json(json!({ "accepted": true, "share_usage": crate::usage::sharing(&hub.home) })).into_response()
        }
        Err(e) => internal(e),
    }
}

/// The kinds of work, and the one picked.
async fn starter(State(hub): State<Arc<Hub>>) -> Response {
    let picked = crate::profile::current(&hub.home, None).map(|p| p.id.clone());
    Json(json!({ "profile": picked, "profiles": crate::profile::summaries() })).into_response()
}

#[derive(Deserialize)]
struct StarterBody {
    profile: String,
}

/// Saves the kind of work and installs its starter set (extensions wait in
/// the Market, where the user sees what each one may do first).
async fn install_starter(State(hub): State<Arc<Hub>>, Json(b): Json<StarterBody>) -> Response {
    let home = hub.home.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value> {
        crate::profile::set_global(&home, &b.profile)?;
        let Some(p) = crate::profile::get(&b.profile) else { return Ok(json!({ "profile": "" })) };
        let cat = crate::market::open_cached(&home, false)?;
        let r = crate::profile::install_starter(&crate::market::Market::new(&home), &cat, p);
        Ok(json!({ "profile": p.id, "title": p.title, "result": r }))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "starter_failed", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

/// Known projects, with the session serving each one, if any.
async fn projects(State(hub): State<Arc<Hub>>) -> Response {
    let home = hub.home.clone();
    let hosted: Vec<String> = hub.hosted.lock().await.keys().cloned().collect();
    let list = tokio::task::spawn_blocking(move || {
        let running = session::running(&home);
        project::listings(&home)
            .into_iter()
            .map(|l| {
                let s = running.iter().find(|i| i.project_id == l.entry.id);
                let here = hosted.contains(&l.entry.id);
                let mut v = json!(l);
                v["session"] = json!(s.map(|i| json!({ "api": i.api, "proxy": i.proxy, "pid": i.pid, "started_at": i.started_at, "here": here })));
                v
            })
            .collect::<Vec<_>>()
    })
    .await;
    match list {
        Ok(l) => Json(l).into_response(),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    /// The folder the project folder goes in (default: the projects folder).
    #[serde(default)]
    location: Option<String>,
}

async fn create(State(hub): State<Arc<Hub>>, Json(b): Json<CreateBody>) -> Response {
    let parent = match b.location.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => project::expand_tilde(FsPath::new(l)),
        None => hub.home.default_projects_dir(),
    };
    if !parent.is_absolute() {
        return err(StatusCode::BAD_REQUEST, "bad_request", "the location must be a full path, such as ~/Plonix");
    }
    let dir = parent.join(project::slug(&b.name));
    let home = hub.home.clone();
    let r = tokio::task::spawn_blocking(move || -> Result<Project> {
        let p = Project::create(&dir, &b.name)?;
        project::remember(&home, &p, false)?;
        Ok(p)
    })
    .await;
    match r {
        Ok(Ok(p)) => (StatusCode::CREATED, Json(json!({ "id": p.id(), "name": p.name(), "path": p.dir }))).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct AddBody {
    path: String,
}

/// Adds an existing project folder (copied from elsewhere, or forgotten).
async fn add_existing(State(hub): State<Arc<Hub>>, Json(b): Json<AddBody>) -> Response {
    let dir = project::expand_tilde(FsPath::new(b.path.trim()));
    match Project::load(&dir).and_then(|p| project::remember(&hub.home, &p, false).map(|_| p)) {
        Ok(p) => Json(json!({ "id": p.id(), "name": p.name(), "path": p.dir })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
    }
}

#[derive(Deserialize, Default)]
struct DemoBody {
    /// Replace the demo with a fresh copy.
    #[serde(default)]
    fresh: bool,
}

/// The demo project, created on first use (or made afresh). Opening it is a
/// separate step, like for any project.
async fn demo(State(hub): State<Arc<Hub>>, body: Option<Json<DemoBody>>) -> Response {
    let fresh = body.map(|Json(b)| b.fresh).unwrap_or_default();
    let home = hub.home.clone();
    match tokio::task::spawn_blocking(move || crate::demo::ensure(&home, fresh)).await {
        Ok(Ok(p)) => Json(json!({ "id": p.id(), "name": p.name(), "path": p.dir })).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, "cannot_create_demo", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

async fn open(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
    match hub.open(&id).await {
        Ok(o) => Json(json!({ "url": o.url, "api": o.info.api, "proxy": o.info.proxy, "project": o.info.project, "started": o.started }))
            .into_response(),
        Err(e) => err(StatusCode::CONFLICT, "cannot_open", &format!("{e:#}")),
    }
}

async fn close(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
    match hub.close(&id).await {
        Ok(closed) => Json(json!({ "closed": closed })).into_response(),
        Err(e) => internal(e),
    }
}

async fn forget(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
    if session::find(&hub.home, &id).is_some() {
        return err(StatusCode::CONFLICT, "project_open", "close the project before removing it from the list");
    }
    match project::forget(&hub.home, &id) {
        Ok(removed) => Json(json!({ "removed": removed })).into_response(),
        Err(e) => internal(e),
    }
}

/// Shows the project folder in the file manager.
async fn reveal(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
    let Some(entry) = project::find(&hub.home, &id) else {
        return err(StatusCode::NOT_FOUND, "not_found", "no such project");
    };
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    match crate::browser::spawn_detached(std::process::Command::new(opener).arg(&entry.path)) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => internal(e.into()),
    }
}

async fn project_settings(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Response {
    let Some(entry) = project::find(&hub.home, &id) else {
        return err(StatusCode::NOT_FOUND, "not_found", "no such project");
    };
    match Project::load(&entry.path) {
        Ok(p) => {
            let mut v = settings::describe(&hub.home, Some(&p.file.settings));
            v["project"] = json!({ "id": p.id(), "name": p.name(), "dir": p.dir, "last_prune": p.file.last_prune });
            Json(v).into_response()
        }
        Err(e) => err(StatusCode::NOT_FOUND, "not_found", &format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct ValuesBody {
    values: Value,
}

fn bad_settings(problems: Vec<settings::Problem>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": "some settings need fixing", "code": "bad_settings", "problems": problems })))
        .into_response()
}

/// Saves a project's section. If the project is open, its engine saves and
/// applies it, so a running proxy picks the change up at once.
async fn put_project_settings(State(hub): State<Arc<Hub>>, Path((id, section)): Path<(String, String)>, Json(b): Json<ValuesBody>) -> Response {
    let Some(entry) = project::find(&hub.home, &id) else {
        return err(StatusCode::NOT_FOUND, "not_found", "no such project");
    };
    // The project's name lives in its file, not in a settings section.
    if section == "general" {
        return rename(&hub, &entry.path, &b.values);
    }
    let Some(sec) = settings::section(&section) else {
        return err(StatusCode::NOT_FOUND, "not_found", &format!("no settings section '{section}'"));
    };
    if sec.level != Level::Project {
        return err(StatusCode::BAD_REQUEST, "bad_request", "that section applies to all projects; save it with PUT /api/settings/{section}");
    }
    if let Some(info) = session::find(&hub.home, &id) {
        return match engine_call(&info.api, &hub.token, "PUT", &format!("/api/settings/{section}"), json!({ "values": b.values })).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => match e.downcast_ref::<ApiFailure>() {
                Some(f) => (StatusCode::BAD_REQUEST, Json(f.0.clone())).into_response(),
                None => internal(e),
            },
        };
    }
    let mut p = match Project::load(&entry.path) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::NOT_FOUND, "not_found", &format!("{e:#}")),
    };
    match sec.check(&b.values, &p.settings(&section)) {
        Ok(values) => match p.save_settings(&section, values.clone()) {
            Ok(()) => Json(json!({ "section": section, "values": values, "applies": sec.applies })).into_response(),
            Err(e) => internal(e),
        },
        Err(problems) => bad_settings(problems),
    }
}

fn rename(hub: &Hub, dir: &FsPath, values: &Value) -> Response {
    let name = values["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return bad_settings(vec![settings::Problem::new("name", "a name of up to 80 characters")]);
    }
    let r = Project::load(dir).and_then(|mut p| {
        p.update(|f| f.name = name.clone())?;
        project::remember(&hub.home, &p, false)?;
        Ok(p)
    });
    match r {
        Ok(p) => Json(json!({ "section": "general", "values": { "name": p.name() } })).into_response(),
        Err(e) => internal(e),
    }
}

async fn ui_style(State(hub): State<Arc<Hub>>) -> Response {
    Json(settings::look(&hub.home)).into_response()
}

async fn put_ui_style(State(hub): State<Arc<Hub>>, Json(b): Json<Value>) -> Response {
    match settings::set_look(&hub.home, &b) {
        Ok(()) => Json(settings::look(&hub.home)).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, "bad_style", &e.to_string()),
    }
}

async fn global_settings(State(hub): State<Arc<Hub>>) -> Response {
    Json(settings::describe(&hub.home, None)).into_response()
}

async fn put_global_settings(State(hub): State<Arc<Hub>>, Path(section): Path<String>, Json(b): Json<ValuesBody>) -> Response {
    let Some(sec) = settings::section(&section).filter(|s| s.level == Level::Global) else {
        return err(StatusCode::NOT_FOUND, "not_found", &format!("no global settings section '{section}'"));
    };
    match sec.check(&b.values, &settings::global(&hub.home, &section)) {
        Ok(values) => match settings::save_global(&hub.home, &section, &values) {
            Ok(()) => Json(json!({ "section": section, "values": values, "applies": sec.applies })).into_response(),
            Err(e) => internal(e),
        },
        Err(problems) => bad_settings(problems),
    }
}

/// Asks the user for a folder with the system's own dialog.
async fn pick_folder(State(_hub): State<Arc<Hub>>) -> Response {
    let Some(cmd) = folder_picker() else {
        return err(StatusCode::NOT_IMPLEMENTED, "no_picker", "type the folder's path instead");
    };
    match tokio::task::spawn_blocking(move || run_picker(cmd)).await {
        Ok(Ok(path)) => Json(json!({ "path": path })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// The command that shows a folder dialog on this system, if there is one.
fn folder_picker() -> Option<Vec<String>> {
    if let Some(custom) = std::env::var_os("PLONIX_FOLDER_PICKER") {
        return Some(vec![custom.to_string_lossy().into_owned()]);
    }
    if cfg!(target_os = "macos") {
        return Some(vec![
            "osascript".into(),
            "-e".into(),
            "POSIX path of (choose folder with prompt \"Choose where the Plonix project folder goes\")".into(),
        ]);
    }
    if cfg!(windows) {
        return Some(vec![
            "powershell".into(),
            "-NoProfile".into(),
            "-Command".into(),
            "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
             $d.Description = 'Choose where the Plonix project folder goes'; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath } else { exit 1 }"
                .into(),
        ]);
    }
    let zenity = ["/usr/bin/zenity", "/usr/local/bin/zenity"].into_iter().find(|p| FsPath::new(p).exists())?;
    Some(vec![zenity.into(), "--file-selection".into(), "--directory".into(), "--title=Choose where the Plonix project folder goes".into()])
}

/// Runs the dialog. `None` when the user cancels.
fn run_picker(cmd: Vec<String>) -> Result<Option<PathBuf>> {
    let out = std::process::Command::new(&cmd[0]).args(&cmd[1..]).output().with_context(|| format!("running {}", cmd[0]))?;
    if !out.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        bail!("the folder dialog returned an unexpected path");
    }
    Ok(Some(path))
}

async fn shutdown(State(hub): State<Arc<Hub>>) -> Response {
    let h = hub.clone();
    tokio::spawn(async move { h.shutdown().await });
    Json(json!({ "ok": true })).into_response()
}
