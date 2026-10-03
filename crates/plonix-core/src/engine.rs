//! The engine ties the proxy, store, scope and upstream client together.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use crate::ca::CertAuthority;
use crate::detect::{self, Detection, Detector, HostTech};
use crate::model::{Exchange, Headers, Source, now_ms};
use crate::paths::{EngineInfo, Home};
use crate::project::PruneReport;
use crate::rulepack::{Library, PackInfo};
use crate::scope::{self, Decision, Rule, ScopeRules};
use crate::settings::ProxySettings;
use crate::store::Store;
use crate::upstream::{OutboundRequest, Upstream, UpstreamOptions, host_matches};

pub struct Engine {
    pub project: String,
    pub store: Store,
    pub ca: Arc<CertAuthority>,
    upstream: RwLock<Arc<Upstream>>,
    rules: RwLock<ScopeRules>,
    pub started_at: i64,
    /// Wakes everything waiting for the engine to stop. Use
    /// [`Engine::request_shutdown`] and [`Engine::stopped`].
    pub shutdown: Notify,
    stopping: AtomicBool,
    proxy: Mutex<ProxyListener>,
    interception: RwLock<Interception>,
    /// The project folder this engine records into, when it has one.
    pub project_ref: OnceLock<ProjectRef>,
    /// Captured exchanges are recorded in arrival order by one worker, so a
    /// response is always analyzed before requests that follow it.
    recorder: mpsc::UnboundedSender<Exchange>,
    recorder_rx: Mutex<Option<mpsc::UnboundedReceiver<Exchange>>>,
    detection: Mutex<DetectionState>,
    /// The listen address last asked for in the settings.
    applied_listen: Mutex<Option<SocketAddr>>,
    overrides: Mutex<UpstreamOptions>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageStats {
    pub total: i64,
    pub out_of_scope: i64,
    pub in_scope_rules: usize,
}

#[derive(Default)]
struct ProxyListener {
    addr: Option<SocketAddr>,
    task: Option<JoinHandle<()>>,
}

/// Which HTTPS tunnels are decrypted.
#[derive(Debug, Clone)]
struct Interception {
    decrypt: bool,
    passthrough: Vec<String>,
}

/// Identifies the project an engine serves.
#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub id: String,
    pub dir: std::path::PathBuf,
}

/// Detection rules currently in effect. Reloaded when installed packs change
/// (`plonix rules add/remove` while the engine runs).
#[derive(Default)]
struct DetectionState {
    library: Option<Library>,
    loaded_stamp: Option<Option<std::time::SystemTime>>,
    rules: Arc<LoadedRules>,
}

#[derive(Debug, Default)]
pub struct LoadedRules {
    pub detector: Detector,
    pub packs: Vec<PackInfo>,
    /// Packs that were skipped, and why.
    pub problems: Vec<String>,
}

/// An active request sent by the engine on behalf of a user or an agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendRequest {
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Headers,
    /// UTF-8 body. Ignored when `body_base64` is set.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_base64: Option<String>,
}

fn get() -> String {
    "GET".into()
}

/// Re-sends a captured exchange, optionally modified.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplayRequest {
    pub id: i64,
    #[serde(default)]
    pub method: Option<String>,
    /// New path and query, e.g. `/api/users/2?debug=1`.
    #[serde(default)]
    pub target: Option<String>,
    /// Headers to add or replace (case-insensitive on name).
    #[serde(default)]
    pub set_headers: Headers,
    #[serde(default)]
    pub remove_headers: Vec<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("{host} is not in scope ({decision}). Active requests are only sent to accepted hosts; accept it with `plonix scope accept {host}`")]
    OutOfScope { host: String, decision: &'static str },
    #[error("{0}")]
    BadRequest(String),
    #[error("exchange {0} not found")]
    NotFound(i64),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Engine {
    pub fn new(project: &str, store: Store, ca: Arc<CertAuthority>, upstream: Upstream) -> Result<Arc<Self>> {
        let rules = store.rules()?;
        let (recorder, rx) = mpsc::unbounded_channel();
        Ok(Arc::new(Self {
            recorder,
            recorder_rx: Mutex::new(Some(rx)),
            project: project.to_string(),
            store,
            ca,
            upstream: RwLock::new(Arc::new(upstream)),
            rules: RwLock::new(rules),
            started_at: now_ms(),
            shutdown: Notify::new(),
            stopping: AtomicBool::new(false),
            proxy: Mutex::default(),
            interception: RwLock::new(Interception { decrypt: true, passthrough: vec![] }),
            project_ref: OnceLock::new(),
            detection: Mutex::new(DetectionState::default()),
            applied_listen: Mutex::new(None),
            overrides: Mutex::default(),
        }))
    }

    /// The client used for outbound requests.
    pub fn upstream(&self) -> Arc<Upstream> {
        self.upstream.read().unwrap().clone()
    }

    pub fn set_upstream(&self, upstream: Upstream) {
        *self.upstream.write().unwrap() = Arc::new(upstream);
    }

    /// Whether HTTPS to `host` is decrypted (and recorded) or tunneled as is.
    pub fn decrypts(&self, host: &str) -> bool {
        let i = self.interception.read().unwrap();
        i.decrypt && !host_matches(host, &i.passthrough)
    }

    /// Where the proxy listens, once it is bound.
    pub fn proxy_addr(&self) -> Option<SocketAddr> {
        self.proxy.lock().unwrap().addr
    }

    /// Binds the proxy listener and serves on it, replacing the current one.
    /// Connections already open keep working. With `fallback`, a taken port
    /// moves to the next free one.
    pub async fn bind_proxy(self: &Arc<Self>, addr: SocketAddr, fallback: bool) -> Result<SocketAddr> {
        if self.proxy_addr() == Some(addr) {
            return Ok(addr);
        }
        let listener = bind_listener(addr, fallback, self.proxy_addr()).await.with_context(|| format!("binding the proxy to {addr}"))?;
        let bound = listener.local_addr()?;
        let task = tokio::spawn(crate::proxy::serve(listener, self.clone()));
        let mut p = self.proxy.lock().unwrap();
        if let Some(old) = p.task.replace(task) {
            old.abort();
        }
        p.addr = Some(bound);
        Ok(bound)
    }

    /// Applies proxy settings: the upstream client, HTTPS interception and,
    /// if they changed, the listen address. Returns the proxy's address.
    pub async fn apply_proxy_settings(self: &Arc<Self>, p: &ProxySettings) -> Result<SocketAddr> {
        let mut options = UpstreamOptions::from_settings(p)?;
        {
            let extra = self.overrides.lock().unwrap();
            options.insecure |= extra.insecure;
            options.extra_roots.extend(extra.extra_roots.iter().cloned());
        }
        let upstream = Upstream::with_options(options)?;
        let addr = SocketAddr::new(p.listen_host, p.listen_port);
        let current = self.proxy_addr();
        // A fallback port stays put as long as the setting does not change.
        let keep = current.is_some_and(|c| c.ip() == addr.ip() && (c.port() == addr.port() || (p.port_fallback && p.listen_port != 0)))
            && self.applied_listen() == Some(addr);
        let bound = if keep { current.unwrap() } else { self.bind_proxy(addr, p.port_fallback).await? };
        *self.applied_listen.lock().unwrap() = Some(addr);
        self.set_upstream(upstream);
        *self.interception.write().unwrap() = Interception { decrypt: p.intercept_tls, passthrough: p.passthrough_hosts.clone() };
        Ok(bound)
    }

    /// Upstream options that apply on top of the settings for this run
    /// (`--insecure-upstream`, extra trusted roots).
    pub fn set_overrides(&self, o: UpstreamOptions) {
        *self.overrides.lock().unwrap() = o;
    }

    fn applied_listen(&self) -> Option<SocketAddr> {
        *self.applied_listen.lock().unwrap()
    }

    /// Asks the engine to stop: the API stops serving and waiters wake up.
    pub fn request_shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(task) = self.proxy.lock().unwrap().task.take() {
            task.abort();
        }
        self.shutdown.notify_waiters();
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Resolves once [`Engine::request_shutdown`] has been called.
    pub async fn stopped(&self) {
        loop {
            let notified = self.shutdown.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_stopping() {
                return;
            }
            notified.await;
        }
    }

    /// Traffic that "keep only in-scope traffic" would delete right now.
    pub fn storage_stats(&self) -> Result<StorageStats> {
        let rules = self.rules();
        let out_hosts: Vec<String> = self.store.distinct_hosts()?.into_iter().filter(|h| !rules.in_scope(h)).collect();
        let keep = self.finding_exchange_ids()?;
        Ok(StorageStats {
            total: self.store.count()?,
            out_of_scope: self.store.count_for_hosts(&out_hosts, &keep)?,
            in_scope_rules: rules.rules.iter().filter(|r| r.decision == Decision::Accepted).count(),
        })
    }

    fn finding_exchange_ids(&self) -> Result<BTreeSet<i64>> {
        Ok(self.store.findings()?.into_iter().flat_map(|f| f.exchange_ids).collect())
    }

    /// Deletes traffic to hosts that are not in scope (keeping requests that
    /// findings point to) and compacts the database so it is gone from disk.
    /// Does nothing while no host is in scope.
    pub fn prune_out_of_scope(&self) -> Result<PruneReport> {
        let rules = self.rules();
        let at = now_ms();
        if !rules.rules.iter().any(|r| r.decision == Decision::Accepted) {
            let kept = self.store.count()?;
            return Ok(PruneReport { at, removed: 0, kept, skipped: "nothing is in scope yet, so nothing was deleted".into() });
        }
        let out_hosts: Vec<String> = self.store.distinct_hosts()?.into_iter().filter(|h| !rules.in_scope(h)).collect();
        let keep = self.finding_exchange_ids()?;
        let removed = self.store.delete_for_hosts(&out_hosts, &keep)?;
        if removed > 0 {
            self.rescan()?;
        }
        self.store.compact()?;
        Ok(PruneReport { at, removed, kept: self.store.count()?, skipped: String::new() })
    }

    pub fn rules(&self) -> ScopeRules {
        self.rules.read().unwrap().clone()
    }

    /// Loads installed rule packs from this library (built-in packs are
    /// always loaded).
    pub fn set_rule_library(&self, library: Library) {
        let mut d = self.detection.lock().unwrap();
        d.library = Some(library);
        d.loaded_stamp = None;
    }

    /// The detection rules in effect, reloading them if packs changed.
    pub fn detection_rules(&self) -> Arc<LoadedRules> {
        let mut d = self.detection.lock().unwrap();
        let stamp = d.library.as_ref().and_then(Library::stamp);
        if d.loaded_stamp != Some(stamp) {
            let loaded = match &d.library {
                Some(lib) => lib.load(),
                None => Library::at(std::path::Path::new("/nonexistent")).load(),
            };
            for p in &loaded.problems {
                tracing::warn!("detection rules: {p}");
            }
            d.rules = Arc::new(LoadedRules {
                detector: loaded.detector(),
                packs: loaded.packs.into_iter().map(|(_, info)| info).collect(),
                problems: loaded.problems,
            });
            d.loaded_stamp = Some(stamp);
        }
        d.rules.clone()
    }

    /// Technologies detected on one host. Runs over already-captured
    /// traffic, so newly installed rules apply to everything seen so far.
    pub fn detect_host(&self, host: &str) -> Result<Vec<Detection>> {
        let rules = self.detection_rules();
        let exchanges = self.store.exchanges_for_host(host, detect::HOST_SAMPLE)?;
        Ok(rules.detector.detect(&exchanges))
    }

    /// Technologies on every host, busiest host first.
    pub fn detect_all(&self) -> Result<Vec<HostTech>> {
        let hosts = self.store.hosts(&self.rules())?;
        hosts.into_iter().take(1000).map(|h| Ok(HostTech { tech: self.detect_host(&h.host)?, host: h.host })).collect()
    }

    /// Queues a captured exchange for recording (see [`Engine::start_recorder`]).
    pub fn enqueue(self: &Arc<Self>, ex: Exchange) {
        if let Err(mpsc::error::SendError(ex)) = self.recorder.send(ex) {
            let engine = self.clone();
            tokio::task::spawn_blocking(move || engine.record(ex));
        }
    }

    /// Starts the worker that drains the recording queue. Idempotent.
    pub fn start_recorder(self: &Arc<Self>) {
        let Some(mut rx) = self.recorder_rx.lock().unwrap().take() else { return };
        let engine = self.clone();
        std::thread::Builder::new()
            .name("plonix-recorder".into())
            .spawn(move || {
                while let Some(ex) = rx.blocking_recv() {
                    if let Err(e) = engine.record(ex) {
                        tracing::error!("failed to record exchange: {e:#}");
                    }
                }
            })
            .expect("spawn recorder thread");
    }

    /// Stores an exchange and feeds it to the scope analyzer.
    pub fn record(&self, ex: Exchange) -> Result<i64> {
        let id = self.store.insert_exchange(&ex)?;
        self.analyze(&ex, id, &self.rules())?;
        Ok(id)
    }

    fn analyze(&self, ex: &Exchange, id: i64, rules: &ScopeRules) -> Result<()> {
        let analysis = scope::analyze(ex, rules, &|h| self.store.token_owner(h));
        if !analysis.tokens.is_empty() {
            self.store.add_tokens(&analysis.tokens, &scope::normalize_host(&ex.host))?;
        }
        for ev in &analysis.evidence {
            self.store.add_evidence(ev, id, ex.ts)?;
        }
        Ok(())
    }

    /// Re-runs the analyzer over all stored traffic. Called when scope
    /// changes, because a newly accepted host turns its traffic into evidence.
    pub fn rescan(&self) -> Result<()> {
        let rules = self.rules();
        self.store.clear_analysis()?;
        // Pass 1 learns session tokens from in-scope traffic, so that token
        // reuse is detected regardless of the order requests were made in.
        let mut last = 0;
        loop {
            let batch = self.store.exchanges_after(last, 500)?;
            let Some(tail) = batch.last() else { break };
            last = tail.id;
            for ex in batch.iter().filter(|e| rules.in_scope(&e.host)) {
                let tokens = scope::session_tokens(ex, true);
                self.store.add_tokens(&tokens, &scope::normalize_host(&ex.host))?;
            }
        }
        let mut last = 0;
        loop {
            let batch = self.store.exchanges_after(last, 500)?;
            let Some(tail) = batch.last() else { break };
            last = tail.id;
            for ex in &batch {
                self.analyze(ex, ex.id, &rules)?;
            }
        }
        Ok(())
    }

    /// Accepts or rejects a domain. `*.example.com` means example.com and all subdomains.
    pub fn decide(&self, domain: &str, decision: Decision, include_subdomains: bool, note: &str) -> Result<Rule> {
        let (pattern, subs) = match domain.trim().strip_prefix("*.") {
            Some(base) => (scope::normalize_host(base), true),
            None => (scope::normalize_host(domain), include_subdomains),
        };
        anyhow::ensure!(!pattern.is_empty(), "empty domain");
        let rule = Rule { pattern, include_subdomains: subs, decision, created_at: now_ms(), note: note.to_string() };
        self.store.put_rule(&rule)?;
        *self.rules.write().unwrap() = self.store.rules()?;
        if decision == Decision::Accepted {
            self.rescan()?;
        }
        Ok(rule)
    }

    pub fn remove_rule(&self, domain: &str) -> Result<bool> {
        let pattern = scope::normalize_host(domain.trim().trim_start_matches("*."));
        let removed = self.store.delete_rule(&pattern)?;
        *self.rules.write().unwrap() = self.store.rules()?;
        self.rescan()?;
        Ok(removed)
    }

    /// Sends an active request. Refused unless the target host is accepted.
    pub async fn send(&self, req: SendRequest, initiator: &str) -> Result<Exchange, SendError> {
        let url = req.url.trim();
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| SendError::BadRequest(format!("absolute URL required, got '{url}'")))?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(SendError::BadRequest(format!("unsupported scheme '{scheme}'")));
        }
        let (authority, target) = match rest.find(['/', '?']) {
            Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        let host = scope::normalize_host(authority);
        let port = authority
            .rsplit_once(':')
            .filter(|(h, _)| !h.contains(':') || h.ends_with(']'))
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(if scheme == "https" { 443 } else { 80 });

        // The single choke point for active traffic: scope is enforced here.
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }

        let body: Vec<u8> = match (&req.body_base64, &req.body) {
            (Some(b64), _) => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| SendError::BadRequest(format!("body_base64: {e}")))?,
            (None, Some(s)) => s.clone().into_bytes(),
            (None, None) => vec![],
        };
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let method = req.method.to_ascii_uppercase();
        let started = Instant::now();
        let mut ex = Exchange {
            ts: now_ms(),
            scheme: scheme.clone(),
            host: host.clone(),
            port,
            method: method.clone(),
            path,
            query,
            req_headers: req.headers.clone(),
            req_body: body.clone(),
            source: Some(Source::Replay),
            initiator: Some(initiator.to_string()),
            ..Default::default()
        };
        let result = self
            .upstream()
            .send(OutboundRequest { scheme, host, port, method, target, headers: req.headers, body: Bytes::from(body), extra_headers: vec![] })
            .await;
        ex.duration_ms = started.elapsed().as_millis() as i64;
        match result {
            Ok(up) => {
                ex.status = Some(up.status);
                ex.resp_headers = up.headers;
                ex.resp_body = up.body.to_vec();
                ex.tls_sans = up.tls_sans;
            }
            Err(e) => ex.error = Some(format!("{e:#}")),
        }
        let id = self.record(ex.clone())?;
        ex.id = id;
        Ok(ex)
    }

    /// Replays a stored exchange with optional modifications (same scope rules as `send`).
    pub async fn replay(&self, req: ReplayRequest, initiator: &str) -> Result<Exchange, SendError> {
        let orig = self.store.get_exchange(req.id)?.ok_or(SendError::NotFound(req.id))?;
        let mut headers = orig.req_headers.clone();
        headers.retain(|(k, _)| !req.remove_headers.iter().any(|r| r.eq_ignore_ascii_case(k)));
        for (k, v) in &req.set_headers {
            match headers.iter_mut().find(|(hk, _)| hk.eq_ignore_ascii_case(k)) {
                Some(slot) => slot.1 = v.clone(),
                None => headers.push((k.clone(), v.clone())),
            }
        }
        let target = req.target.clone().unwrap_or_else(|| {
            if orig.query.is_empty() { orig.path.clone() } else { format!("{}?{}", orig.path, orig.query) }
        });
        let url = Exchange { path: target, query: String::new(), ..orig.clone() }.url();
        let (body, body_base64) = match req.body {
            Some(b) => (Some(b), None),
            None => (None, Some(base64::engine::general_purpose::STANDARD.encode(&orig.req_body))),
        };
        self.send(
            SendRequest { method: req.method.clone().unwrap_or(orig.method.clone()), url, headers, body, body_base64 },
            initiator,
        )
        .await
    }
}

/// Configuration for running an engine process.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub home: Home,
    pub project: String,
    pub proxy_addr: SocketAddr,
    /// When true and `proxy_addr`'s port is taken, use the next free port.
    pub proxy_port_fallback: bool,
    pub api_addr: SocketAddr,
    pub insecure_upstream: bool,
}

/// A started engine: listeners are bound, servers are running.
pub struct Running {
    pub engine: Arc<Engine>,
    pub proxy_addr: SocketAddr,
    pub api_addr: SocketAddr,
    pub token: String,
}

/// Starts an engine for a project by name, the way earlier versions did.
/// Opening a [`crate::session`] is the full version: a project folder,
/// its settings and a lock.
pub async fn start(config: &EngineConfig) -> Result<Running> {
    config.home.ensure()?;
    let ca = Arc::new(CertAuthority::load_or_create(&config.home)?);
    let project = crate::project::resolve(&config.home, &config.project)?;
    let store = Store::open(&project.db_path())?;
    let upstream = Upstream::new(config.insecure_upstream, vec![])?;
    let engine = Engine::new(project.name(), store, ca, upstream)?;
    engine.set_rule_library(Library::new(&config.home));
    start_with(engine, config).await
}

pub async fn start_with(engine: Arc<Engine>, config: &EngineConfig) -> Result<Running> {
    let token = config.home.load_or_create_token()?;
    engine.start_recorder();
    let api = TcpListener::bind(config.api_addr).await.with_context(|| format!("binding API to {}", config.api_addr))?;
    let proxy_addr = engine.bind_proxy(config.proxy_addr, config.proxy_port_fallback).await?;
    let api_addr = api.local_addr()?;
    let router = crate::api::router(engine.clone(), token.clone(), api_addr, config.home.clone());
    let shutdown_engine = engine.clone();
    tokio::spawn(async move {
        let _ = axum::serve(api, router).with_graceful_shutdown(async move { shutdown_engine.stopped().await }).await;
    });
    Ok(Running { engine, proxy_addr, api_addr, token })
}

/// Binds `addr`. With `fallback`, a taken port moves to the next free one
/// (up to 20 above it), then to any free port. `current` is the address
/// being replaced, which counts as free.
async fn bind_listener(addr: SocketAddr, fallback: bool, current: Option<SocketAddr>) -> std::io::Result<TcpListener> {
    let first = TcpListener::bind(addr).await;
    let err = match first {
        Ok(l) => return Ok(l),
        Err(e) if fallback && e.kind() == std::io::ErrorKind::AddrInUse => e,
        Err(e) => return Err(e),
    };
    if addr.port() != 0 {
        for port in addr.port().saturating_add(1)..=addr.port().saturating_add(20) {
            let a = SocketAddr::new(addr.ip(), port);
            if Some(a) == current {
                continue;
            }
            if let Ok(l) = TcpListener::bind(a).await {
                return Ok(l);
            }
        }
    }
    TcpListener::bind(SocketAddr::new(addr.ip(), 0)).await.map_err(|_| err)
}

/// The announcement clients use to find an engine.
pub fn info(engine: &Engine, api_addr: SocketAddr) -> EngineInfo {
    let project = engine.project_ref.get();
    EngineInfo {
        pid: std::process::id(),
        api: format!("http://{api_addr}"),
        proxy: engine.proxy_addr().map(|a| a.to_string()).unwrap_or_default(),
        project: engine.project.clone(),
        started_at: engine.started_at,
        project_id: project.map(|p| p.id.clone()).unwrap_or_default(),
        project_dir: project.map(|p| p.dir.clone()),
    }
}
