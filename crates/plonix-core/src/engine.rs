//! The engine ties the proxy, store, scope and upstream client together.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};

use crate::ca::CertAuthority;
use crate::detect::{self, Detection, Detector, HostTech};
use crate::model::{Exchange, Headers, Source, now_ms};
use crate::paths::{EngineInfo, Home};
use crate::rulepack::{Library, PackInfo};
use crate::exclude::{self, ExcludedDomain, Exclusions, Group, GroupStatus};
use crate::scope::{self, Decision, Rule, ScopeRules};
use crate::store::Store;
use crate::upstream::{OutboundRequest, Upstream};

pub struct Engine {
    pub project: String,
    pub store: Store,
    pub ca: Arc<CertAuthority>,
    pub upstream: Upstream,
    rules: RwLock<ScopeRules>,
    pub started_at: i64,
    pub shutdown: Notify,
    /// Captured exchanges are recorded in arrival order by one worker, so a
    /// response is always analyzed before requests that follow it.
    recorder: mpsc::UnboundedSender<Exchange>,
    recorder_rx: Mutex<Option<mpsc::UnboundedReceiver<Exchange>>>,
    detection: Mutex<DetectionState>,
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
            upstream,
            rules: RwLock::new(rules),
            started_at: now_ms(),
            shutdown: Notify::new(),
            detection: Mutex::new(DetectionState::default()),
        }))
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
            // A rescan rebuilds evidence from scratch against the new rules,
            // which drops anything the rule now covers.
            self.rescan()?;
        } else {
            // A rejection keeps the rest of the evidence, so prune just the
            // domains this rule now decides out of the wait list.
            self.store.prune_decided(&self.rules())?;
        }
        Ok(rule)
    }

    // ---- exclusions ------------------------------------------------------

    /// The built-in groups followed by the user's custom groups.
    pub fn exclusion_groups(&self) -> Result<Vec<Group>> {
        let mut groups = exclude::builtin_groups();
        groups.extend(self.store.custom_groups()?);
        Ok(groups)
    }

    /// A snapshot of every group with each domain's current on/off state.
    pub fn exclusions(&self) -> Result<Exclusions> {
        let rules = self.rules();
        let groups = self
            .exclusion_groups()?
            .into_iter()
            .map(|g| {
                let state = exclude::group_state(&rules, &g);
                let domains = g.domains.iter().map(|d| ExcludedDomain { host: d.clone(), excluded: exclude::is_excluded(&rules, d) }).collect();
                GroupStatus { state, domains, id: g.id, name: g.name, description: g.description, builtin: g.builtin }
            })
            .collect();
        Ok(Exclusions { groups, asked: self.exclusions_asked()? })
    }

    /// Switches a whole group on (exclude every member) or off.
    pub fn set_group_excluded(&self, group_id: &str, on: bool) -> Result<()> {
        let group = self.exclusion_groups()?.into_iter().find(|g| g.id == group_id).context("unknown exclusion group")?;
        if on {
            let now = now_ms();
            let rules: Vec<Rule> = group.domains.iter().map(|d| exclude::exclusion_rule(d, group_id, now)).collect();
            self.store.put_rules(&rules)?;
        } else {
            self.store.delete_group_rules(group_id)?;
        }
        *self.rules.write().unwrap() = self.store.rules()?;
        self.store.prune_decided(&self.rules())?;
        Ok(())
    }

    /// Switches one member of a group on or off.
    pub fn set_domain_excluded(&self, group_id: &str, host: &str, on: bool) -> Result<()> {
        if on {
            self.store.put_rules(&[exclude::exclusion_rule(host, group_id, now_ms())])?;
        } else {
            self.store.delete_group_rule(&scope::normalize_host(host))?;
        }
        *self.rules.write().unwrap() = self.store.rules()?;
        self.store.prune_decided(&self.rules())?;
        Ok(())
    }

    /// Creates or replaces a custom group. Enabling it is a separate step.
    pub fn save_custom_group(&self, mut group: Group) -> Result<Group> {
        group.id = exclude::normalize_group_id(&group.id, &group.name);
        anyhow::ensure!(!group.id.is_empty(), "a group needs a name");
        anyhow::ensure!(!exclude::builtin_groups().iter().any(|g| g.id == group.id), "that name clashes with a built-in group");
        group.builtin = false;
        group.domains = group.domains.iter().map(|d| scope::normalize_host(d)).filter(|d| !d.is_empty()).collect();
        self.store.put_custom_group(&group, now_ms())?;
        Ok(group)
    }

    /// Deletes a custom group and any exclusions it owns.
    pub fn remove_custom_group(&self, id: &str) -> Result<()> {
        self.store.delete_group_rules(id)?;
        self.store.delete_custom_group(id)?;
        *self.rules.write().unwrap() = self.store.rules()?;
        Ok(())
    }

    /// Whether the one-time "exclude common domains?" prompt has been answered.
    pub fn exclusions_asked(&self) -> Result<bool> {
        Ok(self
            .store
            .view_state("exclusions")?
            .and_then(|v| v.get("asked").and_then(|a| a.as_bool()))
            .unwrap_or(false))
    }

    /// Records that the one-time prompt has been answered, so it is not shown again.
    pub fn mark_exclusions_asked(&self) -> Result<()> {
        self.store.set_view_state("exclusions", &serde_json::json!({ "asked": true }))?;
        Ok(())
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
            .upstream
            .send(OutboundRequest { scheme, host, port, method, target, headers: req.headers, body: Bytes::from(body) })
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
    /// When true and `proxy_addr`'s port is taken, fall back to a free port.
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
    pub agent_token: String,
}

/// Binds the proxy and the API and starts serving in the background.
pub async fn start(config: &EngineConfig) -> Result<Running> {
    config.home.ensure()?;
    let ca = Arc::new(CertAuthority::load_or_create(&config.home)?);
    let store = Store::open(&config.home.project_db(&config.project))?;
    let upstream = Upstream::new(config.insecure_upstream, vec![])?;
    let engine = Engine::new(&config.project, store, ca, upstream)?;
    engine.set_rule_library(Library::new(&config.home));
    start_with(engine, config).await
}

pub async fn start_with(engine: Arc<Engine>, config: &EngineConfig) -> Result<Running> {
    let token = config.home.load_or_create_token()?;
    let agent_token = config.home.load_or_create_agent_token()?;
    engine.start_recorder();
    let proxy = match TcpListener::bind(config.proxy_addr).await {
        Ok(l) => l,
        Err(e) if config.proxy_port_fallback && e.kind() == std::io::ErrorKind::AddrInUse => {
            let mut addr = config.proxy_addr;
            addr.set_port(0);
            TcpListener::bind(addr).await?
        }
        Err(e) => return Err(e).with_context(|| format!("binding proxy to {}", config.proxy_addr)),
    };
    let api = TcpListener::bind(config.api_addr).await.with_context(|| format!("binding API to {}", config.api_addr))?;
    let proxy_addr = proxy.local_addr()?;
    let api_addr = api.local_addr()?;
    tokio::spawn(crate::proxy::serve(proxy, engine.clone()));
    let router = crate::api::router(
        engine.clone(),
        crate::api::Tokens { user: token.clone(), agent: agent_token.clone() },
        api_addr,
        proxy_addr,
        config.home.clone(),
    );
    let shutdown_engine = engine.clone();
    tokio::spawn(async move {
        let _ = axum::serve(api, router)
            .with_graceful_shutdown(async move { shutdown_engine.shutdown.notified().await })
            .await;
    });
    Ok(Running { engine, proxy_addr, api_addr, token, agent_token })
}

/// Runs an engine in the foreground until shutdown is requested.
pub async fn run(config: EngineConfig) -> Result<()> {
    let running = start(&config).await?;
    let info = EngineInfo {
        pid: std::process::id(),
        api: format!("http://{}", running.api_addr),
        proxy: running.proxy_addr.to_string(),
        project: config.project.clone(),
        started_at: running.engine.started_at,
    };
    std::fs::write(config.home.engine_file(), serde_json::to_vec_pretty(&info)?)?;
    tracing::info!("proxy listening on {}, API on {}", running.proxy_addr, running.api_addr);
    println!("Plonix engine running: proxy {} · API {} · project {}", running.proxy_addr, running.api_addr, config.project);

    tokio::select! {
        _ = running.engine.shutdown.notified() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    // Only remove the file if it still describes this process.
    if config.home.read_engine_info().is_some_and(|i| i.pid == std::process::id()) {
        let _ = std::fs::remove_file(config.home.engine_file());
    }
    Ok(())
}
