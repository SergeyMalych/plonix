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
use crate::crawl;
use crate::scan;
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

    /// The scan catalog in effect: the built-in detectors and tactics. Installed
    /// scan packs are merged here once pack pinning is wired (see
    /// `docs/scanning.md`).
    pub fn scan_catalog(&self) -> scan::Catalog {
        scan::builtin_catalog()
    }

    /// Fingerprints a host from its captured traffic and suggests a scan
    /// profile. Read-only: sends nothing, so it works for any host.
    pub fn scan_suggest(&self, host: &str) -> Result<scan::ScanSuggestion> {
        let host = scope::normalize_host(host);
        let tech = self.detect_host(&host)?;
        let exchanges = self.store.exchanges_for_host(&host, detect::HOST_SAMPLE)?;
        Ok(self.scan_catalog().suggest(&tech, &exchanges))
    }

    /// Runs an active scan against one accepted host. Every request goes through
    /// `send`, so the scan can only ever reach a host in accepted scope, and
    /// each request is recorded like any replay. Findings are recorded against
    /// the existing Findings store. Never fires on its own — a person starts it.
    pub async fn scan(&self, req: scan::ScanRequest, initiator: &str) -> Result<scan::ScanReport, SendError> {
        let host = scope::normalize_host(&req.host);
        // Scope is enforced again on every send below; this is the early, clear
        // refusal so a scan never even begins against an un-accepted host.
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }

        let catalog = self.scan_catalog();
        let tech = self.detect_host(&host).map_err(SendError::Other)?;
        let exchanges = self.store.exchanges_for_host(&host, detect::HOST_SAMPLE).map_err(SendError::Other)?;
        let signals = catalog.signals(&tech, &exchanges);
        let active: std::collections::BTreeSet<String> = signals.iter().map(|s| s.signal.clone()).collect();

        // Which tactics to run: an explicit list, or the applicable ones from
        // the profile (intrusive only when asked).
        let selected = catalog.select(&active);
        let chosen: Vec<&scan::Tactic> = catalog
            .tactics
            .iter()
            .filter(|t| selected.iter().any(|s| s.id == t.def.id))
            .filter(|t| {
                if req.tactics.is_empty() {
                    req.include_intrusive || t.def.intrusiveness.default_on()
                } else {
                    req.tactics.iter().any(|id| id == &t.def.id)
                }
            })
            .collect();

        let endpoints = self.store.endpoints(&host).map_err(SendError::Other)?;
        // Build the authority from captured traffic so a non-default port is
        // kept; fall back to https:443 for a host with nothing captured yet.
        let (scheme, port) = exchanges.first().map(|e| (e.scheme.clone(), e.port)).unwrap_or_else(|| ("https".into(), 443));
        let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
        let authority = if default_port { host.clone() } else { format!("{host}:{port}") };
        let budget = req.max_requests.unwrap_or(scan::DEFAULT_REQUEST_BUDGET);

        let mut report = scan::ScanReport { host: host.clone(), signals, tactics_run: vec![], requests_sent: 0, findings: vec![], notes: vec![] };
        let mut budget_hit = false;

        for t in &chosen {
            // Fixed-path tactics plan once per host; injecting tactics plan
            // against each discovered endpoint.
            let targets: Vec<Option<scan::ScanTarget>> = if t.def.check.path.is_some() {
                vec![None]
            } else {
                endpoints.iter().map(|e| Some(scan::ScanTarget { method: e.method.clone(), path: e.path.clone() })).collect()
            };
            let mut ran = false;
            'targets: for target in &targets {
                for planned in t.plan(&scheme, &authority, target.as_ref()) {
                    if report.requests_sent >= budget {
                        budget_hit = true;
                        break 'targets;
                    }
                    ran = true;
                    report.requests_sent += 1;
                    let sent = self
                        .send(
                            SendRequest { method: planned.method.clone(), url: planned.url.clone(), headers: planned.headers.clone(), body: None, body_base64: None },
                            initiator,
                        )
                        .await;
                    let ex = match sent {
                        Ok(ex) => ex,
                        // A transport error on one request should not abort the
                        // whole scan; note it and move on.
                        Err(SendError::OutOfScope { .. }) => continue,
                        Err(e) => {
                            report.notes.push(format!("request failed: {}", e));
                            continue;
                        }
                    };
                    if let Some(mut draft) = t.evaluate(&planned, ex.status, &ex.resp_headers, &ex.resp_body) {
                        draft.exchange_id = ex.id;
                        let f = self
                            .store
                            .add_finding(
                                &crate::model::NewFinding {
                                    title: draft.title.clone(),
                                    severity: severity_str(draft.severity).to_string(),
                                    description: draft.description.clone(),
                                    exchange_ids: vec![ex.id],
                                },
                                initiator,
                            )
                            .map_err(SendError::Other)?;
                        report.findings.push(scan::ScanFindingRef { id: f.id, title: f.title, severity: draft.severity });
                    }
                }
            }
            if ran {
                report.tactics_run.push(t.def.id.clone());
            }
            if budget_hit {
                break;
            }
        }
        if budget_hit {
            report.notes.push(format!("request budget of {budget} reached; some tactics may not have run"));
        }
        Ok(report)
    }

    /// Crawls an accepted host: fetches in-scope pages through `send`, follows
    /// the same-host links it finds, and records what it sees. GET only; it
    /// never submits a form and never leaves accepted scope.
    pub async fn crawl(&self, req: crawl::CrawlRequest, initiator: &str) -> Result<crawl::CrawlReport, SendError> {
        let host = scope::normalize_host(&req.host);
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }

        let exchanges = self.store.exchanges_for_host(&host, 50).map_err(SendError::Other)?;
        let (scheme, port) = exchanges.first().map(|e| (e.scheme.clone(), e.port)).unwrap_or_else(|| ("https".into(), 443));
        let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
        let authority = if default_port { host.clone() } else { format!("{host}:{port}") };

        let max_pages = req.max_pages.unwrap_or(crawl::DEFAULT_MAX_PAGES).min(crawl::MAX_PAGES_CEIL);
        let max_depth = req.max_depth.unwrap_or(crawl::DEFAULT_MAX_DEPTH);

        let mut report = crawl::CrawlReport { host: host.clone(), pages_fetched: 0, urls_found: 0, forms: vec![], notes: vec![] };
        if req.browser {
            report.notes.push("the browser crawl mode is not available yet; ran a plain crawl".into());
        }

        // Seed with the start path and any already-discovered endpoints.
        let start = req.start.as_deref().unwrap_or("/");
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
        let enqueue = |url: String, depth: usize, seen: &mut std::collections::BTreeSet<String>, queue: &mut std::collections::VecDeque<(String, usize)>| {
            let key = crawl::dedup_key(&url);
            if seen.insert(key) {
                queue.push_back((url, depth));
            }
        };
        enqueue(format!("{scheme}://{authority}{}", if start.starts_with('/') { start.to_string() } else { format!("/{start}") }), 0, &mut seen, &mut queue);
        for e in self.store.endpoints(&host).map_err(SendError::Other)?.into_iter().filter(|e| e.method.eq_ignore_ascii_case("GET")) {
            enqueue(format!("{scheme}://{authority}{}", e.path), 0, &mut seen, &mut queue);
        }
        report.urls_found = seen.len();

        let mut budget_hit = false;
        while let Some((url, depth)) = queue.pop_front() {
            if report.pages_fetched >= max_pages {
                budget_hit = true;
                break;
            }
            report.pages_fetched += 1;
            let ex = match self.send(SendRequest { method: "GET".into(), url: url.clone(), ..Default::default() }, initiator).await {
                Ok(ex) => ex,
                Err(SendError::OutOfScope { .. }) => continue,
                Err(e) => {
                    report.notes.push(format!("fetch failed: {e}"));
                    continue;
                }
            };
            let is_html = crate::model::header(&ex.resp_headers, "content-type").is_some_and(|c| c.to_ascii_lowercase().contains("html"));
            if !is_html {
                continue;
            }
            let Some(body) = crate::codec::body_text(&ex.resp_headers, &ex.resp_body) else { continue };
            for form in crawl::extract_forms(&body) {
                let action = if form.action.is_empty() {
                    url.clone()
                } else {
                    crawl::resolve_same_host(&scheme, &authority, &ex.path, &form.action).unwrap_or(form.action.clone())
                };
                let resolved = crawl::Form { action, ..form };
                if !report.forms.contains(&resolved) {
                    report.forms.push(resolved);
                }
            }
            if depth < max_depth {
                for raw in crawl::extract_links(&body) {
                    if let Some(next) = crawl::resolve_same_host(&scheme, &authority, &ex.path, &raw) {
                        enqueue(next, depth + 1, &mut seen, &mut queue);
                    }
                }
            }
            report.urls_found = seen.len();
        }
        if budget_hit {
            report.notes.push(format!("page budget of {max_pages} reached; more pages remain uncrawled"));
        }
        Ok(report)
    }
}

fn severity_str(s: scan::Severity) -> &'static str {
    match s {
        scan::Severity::Info => "info",
        scan::Severity::Low => "low",
        scan::Severity::Medium => "medium",
        scan::Severity::High => "high",
        scan::Severity::Critical => "critical",
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
