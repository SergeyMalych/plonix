//! Sessions: an open project, served by its own engine.
//!
//! Every open project gets an engine of its own: its own proxy port, API
//! port and database, so several projects can run side by side, in one
//! process (the app, the Start screen) or in several (`plonix start -p …`).
//! Nothing is shared between them except the certificate authority, which
//! the user trusts once, and the API token.
//!
//! A session holds the project folder's lock for as long as it is open, so
//! one project is never served twice. It announces itself in
//! `$PLONIX_HOME/sessions/<project id>.json`, and in `engine.json` as the
//! current session that commands without `-p` talk to.

use std::fs::File;
use std::net::{SocketAddr, TcpListener as StdListener};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;

use crate::ca::CertAuthority;
use crate::engine::{self, Engine, ProjectRef};
use crate::paths::{EngineInfo, Home, write_atomic};
use crate::project::{self, Project, PruneReport};
use crate::rulepack::Library;
use crate::settings::{self, ProxySettings, StorageSettings};
use crate::store::Store;
use crate::upstream::{Upstream, UpstreamOptions};

pub const DEFAULT_API_PORT: u16 = 8090;

/// Options that override the project's settings for one session.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub proxy_port: Option<u16>,
    pub api_port: Option<u16>,
    /// Accept invalid upstream certificates for this session.
    pub insecure_upstream: bool,
    /// Extra trusted roots (tests).
    pub extra_roots: Vec<rustls_pki_types::CertificateDer<'static>>,
}

pub struct Session {
    pub engine: Arc<Engine>,
    pub project: Project,
    pub api_addr: SocketAddr,
    pub token: String,
    home: Home,
    _lock: File,
}

/// Opens a project: takes its lock, starts its engine and announces it.
pub async fn open(home: &Home, mut project: Project, options: OpenOptions) -> Result<Session> {
    home.ensure()?;
    let lock = project.lock()?;
    let storage = StorageSettings::from_values(&project.settings(settings::STORAGE));
    let ca = Arc::new(CertAuthority::load_or_create(home)?);
    let store = Store::open(&project.db_path()).with_context(|| format!("opening {}", project.db_path().display()))?;
    let engine = Engine::new(project.name(), store, ca, Upstream::new(false, vec![])?)?;
    let _ = engine.project_ref.set(ProjectRef { id: project.id().to_string(), dir: project.dir.clone() });
    engine.set_rule_library(Library::new(home));

    // The last session did not close cleanly: finish its clean-up now.
    if storage.keep_only_in_scope && project.open_marker().exists() {
        let report = engine.prune_out_of_scope()?;
        tracing::info!("{}: removed {} out-of-scope exchanges left from an unfinished session", project.name(), report.removed);
        project.update(|f| f.last_prune = Some(report))?;
    }
    std::fs::write(project.open_marker(), std::process::id().to_string())?;

    let mut proxy = ProxySettings::from_values(&project.settings(settings::PROXY));
    if let Some(port) = options.proxy_port {
        proxy.listen_port = port;
    }
    engine.set_overrides(UpstreamOptions { insecure: options.insecure_upstream, extra_roots: options.extra_roots.clone(), ..Default::default() });
    let token = home.load_or_create_token()?;
    engine.start_recorder();
    let api = bind_api(options.api_port.or(project.file.last_api_port)).await?;
    let api_addr = api.local_addr()?;
    if let Err(e) = engine.apply_proxy_settings(&proxy).await {
        let _ = std::fs::remove_file(project.open_marker());
        return Err(e);
    }
    let router = crate::api::router(engine.clone(), token.clone(), api_addr, home.clone());
    let stopping = engine.clone();
    tokio::spawn(async move {
        let _ = axum::serve(api, router).with_graceful_shutdown(async move { stopping.stopped().await }).await;
    });

    project.update(|f| f.last_api_port = Some(api_addr.port()))?;
    project::remember(home, &project, true)?;
    let session = Session { engine, project, api_addr, token, home: home.clone(), _lock: lock };
    session.announce()?;
    tracing::info!("{}: proxy {}, API {}", session.project.name(), session.proxy_addr(), api_addr);
    Ok(session)
}

/// The API port: the preferred one when free, else any.
async fn bind_api(preferred: Option<u16>) -> Result<TcpListener> {
    if preferred == Some(0) {
        return TcpListener::bind(("127.0.0.1", 0)).await.context("binding the API");
    }
    let next = (DEFAULT_API_PORT..DEFAULT_API_PORT + 20).map(Some);
    for port in [preferred].into_iter().chain(next).flatten().filter(|p| *p != 0) {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)).await {
            return Ok(l);
        }
    }
    TcpListener::bind(("127.0.0.1", 0)).await.context("binding the API")
}

impl Session {
    pub fn proxy_addr(&self) -> SocketAddr {
        self.engine.proxy_addr().unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    pub fn info(&self) -> EngineInfo {
        engine::info(&self.engine, self.api_addr)
    }

    /// Writes this session's announcement and makes it the current session.
    pub fn announce(&self) -> Result<()> {
        let data = serde_json::to_vec_pretty(&self.info())?;
        write_atomic(&session_file(&self.home, self.project.id()), &data)?;
        write_atomic(&self.home.engine_file(), &data)
    }

    /// Re-applies the project's proxy settings after they changed.
    pub async fn reload_proxy_settings(&mut self) -> Result<SocketAddr> {
        let fresh = Project::load(&self.project.dir)?;
        self.project = fresh;
        let p = ProxySettings::from_values(&self.project.settings(settings::PROXY));
        let addr = self.engine.apply_proxy_settings(&p).await?;
        refresh(&self.home, &self.engine, self.api_addr);
        Ok(addr)
    }

    /// Resolves when the session is asked to stop (window closed, `plonix stop`, API shutdown).
    pub async fn stopped(&self) {
        self.engine.stopped().await
    }

    /// Stops the engine, applies "keep only in-scope traffic", withdraws the
    /// announcement and releases the project.
    pub fn close(mut self) -> Result<Option<PruneReport>> {
        self.engine.request_shutdown();
        let storage = StorageSettings::from_values(&Project::load(&self.project.dir).map(|p| p.settings(settings::STORAGE)).unwrap_or_default());
        let mut report = None;
        if storage.keep_only_in_scope {
            let r = self.engine.prune_out_of_scope()?;
            tracing::info!("{}: kept {} in-scope exchanges, removed {}", self.project.name(), r.kept, r.removed);
            self.project.update(|f| f.last_prune = Some(r.clone()))?;
            report = Some(r);
        }
        let _ = std::fs::remove_file(self.project.open_marker());
        withdraw(&self.home, self.project.id());
        Ok(report)
    }
}

pub fn session_file(home: &Home, project_id: &str) -> PathBuf {
    home.sessions_dir().join(format!("{project_id}.json"))
}

/// Rewrites an engine's announcement after its proxy moved.
pub fn refresh(home: &Home, engine: &Engine, api_addr: SocketAddr) {
    let info = engine::info(engine, api_addr);
    if info.project_id.is_empty() {
        return;
    }
    if let Ok(data) = serde_json::to_vec_pretty(&info) {
        let _ = write_atomic(&session_file(home, &info.project_id), &data);
        if home.read_engine_info().is_some_and(|i| i.project_id == info.project_id && i.pid == info.pid) {
            let _ = write_atomic(&home.engine_file(), &data);
        }
    }
}

/// Removes a session's announcement. If it was the current session,
/// another running one becomes current.
fn withdraw(home: &Home, project_id: &str) {
    let _ = std::fs::remove_file(session_file(home, project_id));
    let current = home.read_engine_info();
    if current.as_ref().is_none_or(|i| i.project_id == project_id || i.project_id.is_empty()) {
        match running(home).into_iter().max_by_key(|i| i.started_at) {
            Some(next) => {
                let _ = serde_json::to_vec_pretty(&next).map(|d| write_atomic(&home.engine_file(), &d));
            }
            None => {
                let _ = std::fs::remove_file(home.engine_file());
            }
        }
    }
}

/// Sessions that are running, in any process. Announcements left behind by
/// sessions that ended without closing are removed.
pub fn running(home: &Home) -> Vec<EngineInfo> {
    let Ok(dir) = std::fs::read_dir(home.sessions_dir()) else { return vec![] };
    let mut out = vec![];
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let info: Option<EngineInfo> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let alive = info.as_ref().and_then(|i| i.project_dir.as_ref()).and_then(|d| Project::load(d).ok()).is_some_and(|p| p.is_open());
        match info {
            Some(i) if alive => out.push(i),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out.sort_by_key(|i| i.started_at);
    out
}

/// The running session for a project, by id, name or folder.
pub fn find(home: &Home, selector: &str) -> Option<EngineInfo> {
    let all = running(home);
    let sel = selector.trim();
    let dir = project::expand_tilde(std::path::Path::new(sel));
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    all.iter()
        .find(|i| i.project_id == sel)
        .or_else(|| all.iter().find(|i| i.project == sel))
        .or_else(|| all.iter().find(|i| i.project.eq_ignore_ascii_case(sel) || project::slug(&i.project) == project::slug(sel)))
        .or_else(|| all.iter().find(|i| i.project_dir.as_deref() == Some(dir.as_path())))
        .cloned()
}

/// Whether a loopback port is free right now.
pub fn port_free(port: u16) -> bool {
    StdListener::bind(("127.0.0.1", port)).is_ok()
}
