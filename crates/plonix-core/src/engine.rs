//! The engine ties the proxy, store, scope and upstream client together.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::authcheck;
use crate::ca::CertAuthority;
use crate::clientcert::{CertInfo, ClientCerts, StoredCert};
use crate::har;
use crate::detect::{self, Detection, Detector, HostTech};
use crate::extension::{self, Capability, ExtensionLibrary, Loaded, LoadedSet, Runner};
use crate::insight::{Category as InsightCategory, Insight, Side as InsightSide};
use crate::sandbox;
use crate::model::{Exchange, Headers, Source, WsMessage, now_ms};
use crate::paths::{EngineInfo, Home};
use crate::filterpack::{FilterLibrary, FilterSet};
use crate::listpack::{ListLibrary, ListSet};
use crate::intercept::{InterceptOptions, Interceptor};
use crate::project::PruneReport;
use crate::replace::RuleSet;
use crate::rulepack::{Library, PackInfo, sha256_hex};
use crate::crawl;
use crate::exclude::{self, ExcludedDomain, Exclusions, Group, GroupStatus};
use crate::bounty;
use crate::runs;
use crate::scan;
use crate::scope::{self, Decision, Rule, ScopeRules};
use crate::settings::ProxySettings;
use crate::store::{Learned, Store};
use crate::upstream::{InboundResponse, OutboundRequest, Upstream, UpstreamOptions, host_matches};

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
    recorder: mpsc::UnboundedSender<Queued>,
    recorder_rx: Mutex<Option<mpsc::UnboundedReceiver<Queued>>>,
    detection: Mutex<DetectionState>,
    filters: Mutex<FilterState>,
    lists: Mutex<ListState>,
    /// The listen address last asked for in the settings.
    applied_listen: Mutex<Option<SocketAddr>>,
    overrides: Mutex<UpstreamOptions>,
    /// Bodies passing through the proxy are recorded up to this many bytes.
    body_limit: AtomicUsize,
    /// An optional stand-in for the network, consulted before a send goes out.
    /// The demo project installs one so its made-up hosts answer locally; no
    /// other project sets it, so real traffic always goes to the real network.
    responder: RwLock<Option<Responder>>,
    /// Requests and responses held in the proxy for the user (see [`crate::intercept`]).
    pub intercept: Interceptor,
    /// Match-and-replace rules in effect (see [`crate::replace`]); empty while switched off.
    replace: RwLock<Arc<RuleSet>>,
    replace_on: AtomicBool,
    /// Installed extensions in effect (see [`crate::extension`]).
    extensions: Mutex<ExtensionState>,
    /// Newly recorded exchanges, on their way to the extensions' worker.
    extension_feed: Mutex<Option<std::sync::mpsc::Sender<i64>>>,
    extension_limits: RwLock<sandbox::Limits>,
    /// The last problem running each program extension, so it is logged once.
    program_problems: Mutex<HashMap<String, String>>,
    /// Client certificates presented to servers (see [`crate::clientcert`]); empty while switched off.
    client_certs: RwLock<Arc<ClientCerts>>,
    client_certs_on: AtomicBool,
    /// The bug bounty or disclosure program this project follows, if any
    /// (see [`crate::bounty`]): its rules are enforced on every send.
    program: RwLock<Option<Arc<bounty::Guard>>>,
    /// Hosts handed out for out-of-band tests and the callbacks they got
    /// (see [`crate::callbacks`]). Idle until the user starts it.
    pub callbacks: crate::callbacks::Callbacks,
}

/// Where a project keeps the program it follows.
const PROGRAM_STATE: &str = "program";

/// One scope rule a program would add, change or remove.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeChange {
    pub pattern: String,
    pub include_subdomains: bool,
    pub decision: Decision,
    /// `add`, `update`, `same` or `remove`.
    pub change: &'static str,
}

/// What applying a program would change, for the user to review.
#[derive(Debug, Clone, Serialize)]
pub struct ProgramPreview {
    pub program: bounty::Program,
    pub scope: Vec<ScopeChange>,
    /// Assets Plonix cannot test as hosts (mobile apps, source code, `*.example.*`).
    pub not_scoped: Vec<bounty::Asset>,
    /// The project already follows a different program.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
}

/// A stand-in for the network: given an outbound request it may return a
/// canned response, or `None` to let the request go out as usual.
pub type Responder = Arc<dyn Fn(&OutboundRequest) -> Option<InboundResponse> + Send + Sync>;

/// How much of each body is recorded until the settings say otherwise.
pub const DEFAULT_BODY_LIMIT: usize = 10 * 1024 * 1024;

/// Most exchanges the recorder writes in one transaction. Filling the search
/// index one exchange per transaction costs several times more per exchange.
const RECORD_BATCH: usize = 128;

#[derive(Debug, Clone, Serialize)]
pub struct StorageStats {
    pub total: i64,
    pub out_of_scope: i64,
    pub in_scope_rules: usize,
}

/// An exchange waiting to be recorded, and who wants to know its id.
type Queued = (Exchange, Option<oneshot::Sender<i64>>);

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

/// Named Traffic filters in effect, reloaded when filter packs change.
#[derive(Default)]
struct FilterState {
    library: Option<FilterLibrary>,
    loaded_stamp: Option<Option<std::time::SystemTime>>,
    set: Arc<FilterSet>,
}

/// Payload lists in effect for the Bench, reloaded when list packs change.
#[derive(Default)]
struct ListState {
    library: Option<ListLibrary>,
    loaded_stamp: Option<Option<std::time::SystemTime>>,
    set: Arc<ListSet>,
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
    /// Send this request as a saved user: its headers replace the auth
    /// headers on the request before it goes out. The values stay in the
    /// project database and never reach the client. See [`crate::users`].
    #[serde(default)]
    pub as_user: Option<String>,
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
    /// The program this project follows does not allow it.
    #[error("{0}")]
    NotAllowed(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Engine {
    pub fn new(project: &str, store: Store, ca: Arc<CertAuthority>, upstream: Upstream) -> Result<Arc<Self>> {
        let rules = store.rules()?;
        let replace = RuleSet::new(&store.replace_rules()?);
        let guard = match store.view_state(PROGRAM_STATE)? {
            Some(v) => match serde_json::from_value::<bounty::Program>(v) {
                Ok(p) => Some(Arc::new(bounty::Guard::new(p))),
                Err(e) => {
                    tracing::warn!("the saved program could not be read: {e}");
                    None
                }
            },
            None => None,
        };
        let callbacks = crate::callbacks::Callbacks::load(&store);
        let (recorder, rx) = mpsc::unbounded_channel();
        let engine = Arc::new(Self {
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
            filters: Mutex::new(FilterState::default()),
            lists: Mutex::new(ListState::default()),
            applied_listen: Mutex::new(None),
            overrides: Mutex::default(),
            body_limit: AtomicUsize::new(DEFAULT_BODY_LIMIT),
            responder: RwLock::new(None),
            intercept: Interceptor::default(),
            replace: RwLock::new(replace),
            replace_on: AtomicBool::new(true),
            extensions: Mutex::default(),
            extension_feed: Mutex::new(None),
            extension_limits: RwLock::new(sandbox::Limits::default()),
            program_problems: Mutex::default(),
            client_certs: RwLock::default(),
            client_certs_on: AtomicBool::new(true),
            program: RwLock::new(guard),
            callbacks,
        });
        engine.reload_client_certs()?;
        Ok(engine)
    }

    /// Installs a stand-in for the network (see [`Responder`]). Only the demo
    /// project uses this.
    pub fn set_responder(&self, responder: Responder) {
        *self.responder.write().unwrap() = Some(responder);
    }

    fn responder(&self) -> Option<Responder> {
        self.responder.read().unwrap().clone()
    }

    /// How many bytes of each body the proxy records.
    pub fn body_limit(&self) -> usize {
        self.body_limit.load(Ordering::Relaxed)
    }

    pub fn set_body_limit(&self, bytes: usize) {
        self.body_limit.store(bytes.max(1), Ordering::Relaxed);
    }

    /// The client used for outbound requests.
    pub fn upstream(&self) -> Arc<Upstream> {
        self.upstream.read().unwrap().clone()
    }

    pub fn set_upstream(&self, upstream: Upstream) {
        upstream.set_client_certs(self.client_certs.read().unwrap().clone());
        *self.upstream.write().unwrap() = Arc::new(upstream);
    }

    /// Switches client certificates on or off (Settings › Client certificates).
    pub fn set_client_certs_on(&self, on: bool) -> Result<()> {
        self.client_certs_on.store(on, Ordering::Relaxed);
        self.reload_client_certs()
    }

    /// Reads the client certificates again after they changed. Ones that
    /// cannot be used are left out with a warning that names the host only.
    pub fn reload_client_certs(&self) -> Result<()> {
        let certs = if self.client_certs_on.load(Ordering::Relaxed) {
            let (certs, problems) = ClientCerts::load(&self.store.client_certs()?);
            for p in problems {
                tracing::warn!("{p}");
            }
            Arc::new(certs)
        } else {
            Arc::default()
        };
        *self.client_certs.write().unwrap() = certs.clone();
        self.upstream().set_client_certs(certs);
        Ok(())
    }

    /// The project's client certificates, described without their keys.
    pub fn client_cert_list(&self) -> Result<Vec<CertInfo>> {
        Ok(self.store.client_certs()?.iter().map(crate::clientcert::describe).collect())
    }

    /// Adds a client certificate and starts presenting it.
    pub fn add_client_cert(&self, cert: &StoredCert) -> Result<CertInfo> {
        let stored = self.store.add_client_cert(cert)?;
        self.reload_client_certs()?;
        Ok(crate::clientcert::describe(&stored))
    }

    pub fn remove_client_cert(&self, id: i64) -> Result<bool> {
        let removed = self.store.delete_client_cert(id)?;
        self.reload_client_certs()?;
        Ok(removed)
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
        let old = {
            let mut p = self.proxy.lock().unwrap();
            p.addr = Some(bound);
            p.task.replace(task)
        };
        if let Some(old) = old {
            // Wait until the old listener is dropped, so its port is free
            // when this returns. Connections it accepted carry on.
            old.abort();
            let _ = old.await;
        }
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
        self.set_body_limit((p.max_body_mb as usize).saturating_mul(1024 * 1024));
        *self.interception.write().unwrap() = Interception { decrypt: p.intercept_tls, passthrough: p.passthrough_hosts.clone() };
        Ok(bound)
    }

    /// The match-and-replace rules the proxy applies.
    pub fn replace_rules(&self) -> Arc<RuleSet> {
        self.replace.read().unwrap().clone()
    }

    /// Switches match and replace on or off (Settings › Match and replace).
    pub fn set_replace_on(&self, on: bool) -> Result<()> {
        self.replace_on.store(on, Ordering::Relaxed);
        self.reload_replace_rules()
    }

    /// Reads the rules again after they changed.
    pub fn reload_replace_rules(&self) -> Result<()> {
        let set = if self.replace_on.load(Ordering::Relaxed) { RuleSet::new(&self.store.replace_rules()?) } else { Arc::default() };
        *self.replace.write().unwrap() = set;
        Ok(())
    }

    /// Applies Intercept's options. A filter that does not parse is refused.
    pub fn set_intercept_options(&self, options: InterceptOptions) -> Result<()> {
        let filter = self.filters().parse(&options.filter).context("the Intercept filter")?;
        self.intercept.set_options(options, filter);
        Ok(())
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
        self.callbacks.stop();
        let _ = self.callbacks.persist(&self.store);
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

    // ---- program ---------------------------------------------------------

    /// The program this project follows, as enforced.
    pub fn program(&self) -> Option<Arc<bounty::Guard>> {
        self.program.read().unwrap().clone()
    }

    /// What applying `p` would change. Nothing is changed.
    pub fn program_preview(&self, p: bounty::Program) -> Result<ProgramPreview> {
        p.validate()?;
        let wanted = p.scope_rules(now_ms());
        let current = self.rules();
        let note = p.rule_note();
        let mut scope: Vec<ScopeChange> = wanted
            .iter()
            .map(|w| {
                let change = match current.rules.iter().find(|r| r.pattern == w.pattern) {
                    Some(r) if r.decision == w.decision && r.include_subdomains == w.include_subdomains => "same",
                    Some(_) => "update",
                    None => "add",
                };
                ScopeChange { pattern: w.pattern.clone(), include_subdomains: w.include_subdomains, decision: w.decision, change }
            })
            .collect();
        for r in current.rules.iter().filter(|r| r.note == note && !wanted.iter().any(|w| w.pattern == r.pattern)) {
            scope.push(ScopeChange { pattern: r.pattern.clone(), include_subdomains: r.include_subdomains, decision: r.decision, change: "remove" });
        }
        let not_scoped = p.assets.iter().filter(|a| !a.kind.testable() || bounty::scope_targets(&a.identifier, a.kind).is_empty()).cloned().collect();
        let replaces = self.program().map(|g| g.program.clone()).filter(|old| old.key() != p.key()).map(|old| old.name);
        Ok(ProgramPreview { program: p, scope, not_scoped, replaces })
    }

    /// Makes this project follow `p`: its scope rules replace the ones an
    /// earlier sync of it (or another program) added, and its rules of
    /// engagement apply to every send from now on.
    pub fn apply_program(&self, p: bounty::Program) -> Result<ProgramPreview> {
        let preview = self.program_preview(p.clone())?;
        if let Some(old) = self.program() {
            self.store.delete_rules_noted(&old.program.rule_note())?;
        }
        self.store.delete_rules_noted(&p.rule_note())?;
        self.store.put_rules(&p.scope_rules(now_ms()))?;
        *self.rules.write().unwrap() = self.store.rules()?;
        self.rescan()?;
        self.store.set_view_state(PROGRAM_STATE, &serde_json::to_value(&p)?)?;
        *self.program.write().unwrap() = Some(Arc::new(bounty::Guard::new(p)));
        Ok(preview)
    }

    /// Stops following the program. Its scope rules stay unless `remove_scope`.
    pub fn clear_program(&self, remove_scope: bool) -> Result<bool> {
        let Some(old) = self.program.write().unwrap().take() else { return Ok(false) };
        if remove_scope {
            self.store.delete_rules_noted(&old.program.rule_note())?;
            *self.rules.write().unwrap() = self.store.rules()?;
            self.rescan()?;
        }
        self.store.set_view_state(PROGRAM_STATE, &serde_json::Value::Null)?;
        Ok(true)
    }

    /// Refuses automated testing when the program in effect bans it.
    fn check_automation(&self, what: &str) -> Result<(), SendError> {
        match self.program() {
            Some(g) => g.check_automation(what).map_err(SendError::NotAllowed),
            None => Ok(()),
        }
    }

    /// Loads installed rule packs from this library (built-in packs are
    /// always loaded).
    /// Loads installed filter packs from this library (built-in packs are
    /// always loaded).
    pub fn set_filter_library(&self, library: FilterLibrary) {
        let mut f = self.filters.lock().unwrap();
        f.library = Some(library);
        f.loaded_stamp = None;
    }

    /// Named filters in effect (`is:name`), reloading them if packs changed.
    pub fn filters(&self) -> Arc<FilterSet> {
        let mut f = self.filters.lock().unwrap();
        let stamp = f.library.as_ref().and_then(FilterLibrary::stamp);
        if f.loaded_stamp != Some(stamp) {
            let set = match &f.library {
                Some(lib) => lib.load(),
                None => FilterLibrary::at(std::path::Path::new("/nonexistent")).load(),
            };
            for p in &set.problems {
                tracing::warn!("filters: {p}");
            }
            f.set = Arc::new(set);
            f.loaded_stamp = Some(stamp);
        }
        f.set.clone()
    }

    pub fn set_list_library(&self, library: ListLibrary) {
        let mut l = self.lists.lock().unwrap();
        l.library = Some(library);
        l.loaded_stamp = None;
    }

    /// The payload lists in effect, reloading them if installed packs changed.
    pub fn lists(&self) -> Arc<ListSet> {
        let mut l = self.lists.lock().unwrap();
        let stamp = l.library.as_ref().and_then(ListLibrary::stamp);
        if l.loaded_stamp != Some(stamp) {
            let set = match &l.library {
                Some(lib) => lib.load(),
                None => ListLibrary::at(std::path::Path::new("/nonexistent")).load(),
            };
            for p in &set.problems {
                tracing::warn!("lists: {p}");
            }
            l.set = Arc::new(set);
            l.loaded_stamp = Some(stamp);
        }
        l.set.clone()
    }

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
        self.queue(ex, None);
    }

    /// Like [`Engine::enqueue`], and tells the exchange's id once it is
    /// recorded (WebSocket messages are stored against their handshake).
    pub fn enqueue_for_id(self: &Arc<Self>, ex: Exchange) -> oneshot::Receiver<i64> {
        let (tx, rx) = oneshot::channel();
        self.queue(ex, Some(tx));
        rx
    }

    fn queue(self: &Arc<Self>, ex: Exchange, reply: Option<oneshot::Sender<i64>>) {
        if let Err(mpsc::error::SendError((ex, reply))) = self.recorder.send((ex, reply)) {
            let engine = self.clone();
            tokio::task::spawn_blocking(move || {
                if let (Ok(id), Some(reply)) = (engine.record(ex), reply) {
                    let _ = reply.send(id);
                }
            });
        }
    }

    /// Starts the worker that drains the recording queue. Idempotent.
    pub fn start_recorder(self: &Arc<Self>) {
        let Some(mut rx) = self.recorder_rx.lock().unwrap().take() else { return };
        let engine = self.clone();
        std::thread::Builder::new()
            .name("plonix-recorder".into())
            .spawn(move || {
                // Whatever queued up while the last batch was written goes
                // in the next one: a busy proxy is recorded in batches.
                while let Some(first) = rx.blocking_recv() {
                    let mut batch = vec![first];
                    while batch.len() < RECORD_BATCH {
                        match rx.try_recv() {
                            Ok(item) => batch.push(item),
                            Err(_) => break,
                        }
                    }
                    let (exchanges, replies): (Vec<Exchange>, Vec<_>) = batch.into_iter().unzip();
                    match engine.record_all(exchanges) {
                        Ok(ids) => {
                            for (id, reply) in ids.into_iter().zip(replies) {
                                if let Some(reply) = reply {
                                    let _ = reply.send(id);
                                }
                            }
                        }
                        Err(e) => tracing::error!("failed to record exchanges: {e:#}"),
                    }
                }
            })
            .expect("spawn recorder thread");
    }

    /// Imports a HAR file: each entry is stored and analyzed like captured
    /// traffic, with source `import`. Entries the project has already (same
    /// time, method, URL and status) are left out, so importing a file twice
    /// adds nothing. Bodies over the recording limit are kept in part.
    pub fn import_har(&self, reader: impl std::io::Read) -> Result<har::ImportReport> {
        let mut report = har::ImportReport::default();
        let limit = self.body_limit();
        let rules = self.rules();
        let mut n = 0;
        har::read_entries(reader, |entry| {
            n += 1;
            let (ex, messages) = match har::to_exchange(&entry, limit) {
                Ok(x) => x,
                Err(why) => {
                    report.skip(n, why);
                    return Ok(());
                }
            };
            if self.store.has_same_exchange(&ex)? {
                report.duplicates += 1;
                return Ok(());
            }
            let id = self.store.insert_exchange(&ex)?;
            analyze_into(&self.store, std::iter::once((&ex, id)), &rules)?;
            self.queue_for_extensions(id);
            if !messages.is_empty() {
                let messages: Vec<WsMessage> = messages.into_iter().map(|m| WsMessage { exchange_id: id, ..m }).collect();
                self.store.insert_ws_messages(&messages)?;
            }
            report.imported += 1;
            report.first_id.get_or_insert(id);
            report.last_id = Some(id);
            Ok(())
        })?;
        Ok(report)
    }

    /// The exchanges a HAR export holds: `ids` when given, else everything
    /// matching the Traffic search `q`, oldest first.
    pub fn har_selection(&self, q: &str, ids: &[i64]) -> Result<Vec<i64>> {
        if !ids.is_empty() {
            let mut ids = ids.to_vec();
            ids.sort_unstable();
            ids.dedup();
            return Ok(ids);
        }
        let query = self.filters().parse(q)?;
        self.store.search_ids(&query, &self.rules())
    }

    /// Writes the exchanges `ids` as a HAR file, one entry at a time.
    pub fn write_har(&self, ids: &[i64], out: impl std::io::Write) -> Result<usize> {
        let entries = ids.iter().filter_map(|&id| match self.store.get_exchange(id) {
            Ok(Some(ex)) => {
                let messages = if ex.status == Some(101) { self.store.ws_messages(id, 100_000, 0).map(|(m, _)| m) } else { Ok(vec![]) };
                Some(messages.map(|m| (ex, m)))
            }
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        });
        har::write(out, entries)
    }

    /// Stores an exchange and feeds it to the scope analyzer.
    pub fn record(&self, ex: Exchange) -> Result<i64> {
        Ok(self.record_all(vec![ex])?[0])
    }

    /// Stores exchanges in one transaction, then feeds them to the scope
    /// analyzer in order. Returns their ids.
    pub fn record_all(&self, exchanges: Vec<Exchange>) -> Result<Vec<i64>> {
        let ids = self.store.insert_exchanges(&exchanges)?;
        analyze_into(&self.store, exchanges.iter().zip(ids.iter().copied()), &self.rules())?;
        for &id in &ids {
            self.queue_for_extensions(id);
        }
        Ok(ids)
    }

    /// Re-runs the analyzer over all stored traffic. Called when scope
    /// changes, because a newly accepted host turns its traffic into evidence.
    pub fn rescan(&self) -> Result<()> {
        reanalyze(&self.store, &self.rules())
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
    pub async fn send(&self, mut req: SendRequest, initiator: &str) -> Result<Exchange, SendError> {
        // Sending as a saved user: its headers replace the auth headers the
        // draft carried. The values come from the project database, so they
        // are never round-tripped through the client.
        if let Some(uid) = req.as_user.take().filter(|u| !u.is_empty()) {
            let users = self.store.saved_users().map_err(SendError::Other)?;
            let Some(user) = users.into_iter().find(|u| u.id == uid) else {
                return Err(SendError::BadRequest(format!("there is no saved user '{uid}'")));
            };
            req.headers.retain(|(k, _)| !crate::users::is_auth_header(k));
            req.headers.extend(user.headers);
        }
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
        // The program's rules of engagement: the headers it asks for, at the rate it allows.
        if let Some(guard) = self.program() {
            guard.add_headers(&mut req.headers);
            guard.pace().await;
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
        let outbound = OutboundRequest { scheme, host, port, method, target, headers: req.headers, body: Bytes::from(body), extra_headers: vec![] };
        let result = match self.responder().and_then(|r| r(&outbound)) {
            Some(resp) => Ok(resp),
            None => self.upstream().send(outbound).await,
        };
        ex.duration_ms = started.elapsed().as_millis() as i64;
        match result {
            Ok(up) => {
                ex.status = Some(up.status);
                ex.resp_headers = up.headers;
                ex.resp_body = up.body.to_vec();
                ex.tls_sans = up.tls_sans;
                ex.http_version = up.version;
                ex.client_cert = up.client_cert;
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
        if orig.req_truncated && req.body.is_none() {
            return Err(SendError::BadRequest(format!(
                "only the first {} bytes of request {}'s body were recorded, so it cannot be sent again as it was; give the body to send",
                orig.req_body.len(),
                orig.id
            )));
        }
        let url = Exchange { path: target, query: String::new(), ..orig.clone() }.url();
        let (body, body_base64) = match req.body {
            Some(b) => (Some(b), None),
            None => (None, Some(base64::engine::general_purpose::STANDARD.encode(&orig.req_body))),
        };
        self.send(
            SendRequest { method: req.method.clone().unwrap_or(orig.method.clone()), url, headers, body, body_base64, as_user: None },
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

    /// Analyzes a host and returns a reviewable scan plan: the active signals
    /// and the gated tactics turned into concrete, justified `TestProposal`s
    /// over the host's discovered endpoints, grouped by OWASP category.
    /// Read-only — sends nothing — so it works for any host and is safe to
    /// expose to an advising agent, exactly like `scan_suggest`.
    pub fn scan_plan(&self, host: &str) -> Result<scan::ScanPlan> {
        let host = scope::normalize_host(host);
        let tech = self.detect_host(&host)?;
        let exchanges = self.store.exchanges_for_host(&host, detect::HOST_SAMPLE)?;
        let endpoints: Vec<scan::PlanEndpoint> = self
            .store
            .endpoints(&host)?
            .into_iter()
            .map(|e| scan::PlanEndpoint { method: e.method, path: e.path, params: e.params, sample_id: e.sample_id })
            .collect();
        Ok(self.scan_catalog().plan(&host, &tech, &exchanges, &endpoints))
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
        self.check_automation("scanning")?;
        let mut req = req;
        if self.program().is_some_and(|g| g.no_intrusive()) {
            req.include_intrusive = false;
        }
        let no_intrusive = self.program().is_some_and(|g| g.no_intrusive());

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
            .filter(|t| !(no_intrusive && t.def.intrusiveness == scan::Intrusiveness::Intrusive))
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
                            SendRequest { method: planned.method.clone(), url: planned.url.clone(), headers: planned.headers.clone(), body: None, body_base64: None, as_user: None },
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
    /// never submits a form and never leaves accepted scope. With `browser`,
    /// the walk runs in a headless browser instead (see [`crate::browser_crawl`]).
    pub async fn crawl(&self, req: crawl::CrawlRequest, initiator: &str) -> Result<crawl::CrawlReport, SendError> {
        let host = scope::normalize_host(&req.host);
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }
        self.check_automation("crawling")?;

        // The start may be a full URL, which also fixes the scheme and port;
        // otherwise they come from the host's captured traffic.
        let start = req.start.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("/");
        let (scheme, authority, start) = match crawl::split_url(start) {
            Some((s, auth, path)) => {
                if scope::normalize_host(auth) != host {
                    return Err(SendError::BadRequest(format!("the start URL {start} is not on {host}")));
                }
                (s.to_ascii_lowercase(), auth.to_string(), path.to_string())
            }
            None if start.contains("://") => return Err(SendError::BadRequest(format!("the start URL {start} must use http or https"))),
            None => {
                let exchanges = self.store.exchanges_for_host(&host, 50).map_err(SendError::Other)?;
                let (scheme, port) = exchanges.first().map(|e| (e.scheme.clone(), e.port)).unwrap_or_else(|| ("https".into(), 443));
                let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
                let authority = if default_port { host.clone() } else { format!("{host}:{port}") };
                (scheme, authority, if start.starts_with('/') { start.to_string() } else { format!("/{start}") })
            }
        };

        if req.browser {
            return crate::browser_crawl::run(self, &host, format!("{scheme}://{authority}{start}"), &req).await;
        }

        let max_pages = req.max_pages.unwrap_or(crawl::DEFAULT_MAX_PAGES).min(crawl::MAX_PAGES_CEIL);
        let max_depth = req.max_depth.unwrap_or(crawl::DEFAULT_MAX_DEPTH);

        let mut report = crawl::CrawlReport::new(&host);

        // Seed with the start path and any already-discovered endpoints.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
        let enqueue = |url: String, depth: usize, seen: &mut std::collections::BTreeSet<String>, queue: &mut std::collections::VecDeque<(String, usize)>| {
            let key = crawl::dedup_key(&url);
            if seen.insert(key) {
                queue.push_back((url, depth));
            }
        };
        enqueue(format!("{scheme}://{authority}{start}"), 0, &mut seen, &mut queue);
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
                report.add_form(resolved);
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

    /// Runs a set of payloads through the marked positions of a request. Every
    /// request it produces goes through [`Self::send`], so a run is bounded by
    /// scope exactly like a single Bench send: it can only reach accepted hosts,
    /// it is capped by a request budget, and every send is recorded. A person
    /// starts it; it never fires on its own.
    pub async fn run(&self, req: runs::RunRequest, initiator: &str) -> Result<runs::RunReport, SendError> {
        self.check_automation("Bench runs")?;
        let lists = self.lists();
        let plan = runs::plan(&req, &|id| lists.values_of(id)).map_err(SendError::BadRequest)?;
        let budget = req.max_requests.unwrap_or(runs::DEFAULT_REQUEST_BUDGET).min(runs::MAX_REQUEST_BUDGET);
        let delay = std::time::Duration::from_millis(req.delay_ms.unwrap_or(runs::DEFAULT_DELAY_MS).min(runs::MAX_DELAY_MS));
        let method = if req.method.trim().is_empty() { "GET".into() } else { req.method.to_ascii_uppercase() };

        let mut report = runs::RunReport { positions: plan.positions(), ..Default::default() };

        // Build the full list of requests to send: the baseline (if asked), then
        // one per value-assignment.
        let base = plan.base_values();
        let mut assignments: Vec<(bool, Vec<String>)> = Vec::new();
        if req.include_base {
            assignments.push((true, base.clone()));
        }
        for a in plan.assignments() {
            assignments.push((false, a));
        }
        report.planned = assignments.len();
        if assignments.len() > budget {
            assignments.truncate(budget);
            report.truncated = true;
        }

        let mut first = true;
        for (baseline, values) in assignments {
            // Pace the run; no pause before the very first request.
            if !first && !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            first = false;

            let (url, raw) = plan.render(&values);
            let (headers, body) = match runs::split_raw(&raw) {
                Ok(hb) => hb,
                Err(e) => return Err(SendError::BadRequest(e)),
            };
            let send = SendRequest {
                method: method.clone(),
                url,
                headers,
                body: if body.is_empty() { None } else { Some(body) },
                body_base64: None,
                as_user: None,
            };
            let n = report.rows.len() + 1;
            match self.send(send, initiator).await {
                Ok(ex) => report.rows.push(runs::RunRow {
                    n,
                    values: values.clone(),
                    exchange_id: ex.id,
                    status: ex.status,
                    length: ex.resp_body.len(),
                    duration_ms: ex.duration_ms,
                    error: ex.error,
                    baseline,
                }),
                // The first out-of-scope refusal stops the run with a clear
                // error, since every request targets the same host family.
                Err(e @ SendError::OutOfScope { .. }) => {
                    if report.rows.is_empty() {
                        return Err(e);
                    }
                    report.notes.push(e.to_string());
                    break;
                }
                Err(SendError::BadRequest(m)) => {
                    if report.rows.is_empty() {
                        return Err(SendError::BadRequest(m));
                    }
                    report.notes.push(format!("request {n}: {m}"));
                }
                Err(SendError::Other(e)) => return Err(SendError::Other(e)),
                Err(e) => report.notes.push(format!("request {n}: {e}")),
            }
        }
        report.requests_sent = report.rows.len();
        if report.truncated {
            report.notes.push(format!("request budget of {budget} reached; {} of {} requests were sent", report.requests_sent, report.planned));
        }
        Ok(report)
    }

    /// Replays each target request as every chosen identity and lines the
    /// responses up (see [`crate::authcheck`]). Every replay goes through the
    /// scope-gated send path, so a target outside accepted scope is refused,
    /// never sent.
    pub async fn access_check(&self, req: authcheck::AuthCheckRequest, initiator: &str) -> Result<authcheck::AuthCheckReport, SendError> {
        use crate::users::AUTH_HEADERS;

        let mut identities: Vec<authcheck::Identity> =
            req.users.iter().map(|u| authcheck::Identity { id: u.id.clone(), label: u.name.clone(), anon: false }).collect();
        if req.include_anon {
            identities.push(authcheck::Identity { id: "signed-out".into(), label: "Signed out".into(), anon: true });
        }
        if identities.is_empty() {
            return Err(SendError::BadRequest("choose at least one saved user, or keep the signed-out check on".into()));
        }
        let targets: Vec<i64> = req.targets.iter().take(authcheck::MAX_TARGETS).copied().collect();
        if targets.is_empty() {
            return Err(SendError::BadRequest("no requests were selected to check".into()));
        }

        let remove_auth: Vec<String> = AUTH_HEADERS.iter().map(|h| h.to_string()).collect();
        let delay = std::time::Duration::from_millis(req.delay_ms.unwrap_or(authcheck::DEFAULT_DELAY_MS).min(authcheck::MAX_DELAY_MS));
        let mut report = authcheck::AuthCheckReport { identities: identities.clone(), planned: targets.len() * identities.len(), ..Default::default() };

        let mut attempts = 0usize;
        let mut first = true;
        'targets: for tid in targets {
            let Some(orig) = self.store.get_exchange(tid).map_err(SendError::Other)? else { continue };
            let mut cells = Vec::with_capacity(identities.len());
            for ident in &identities {
                if attempts >= authcheck::MAX_REQUESTS {
                    report.truncated = true;
                    break 'targets;
                }
                attempts += 1;
                if !first && !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                first = false;

                // As a saved user: drop the known auth headers the capture
                // carried, then set this user's own. Signed out: drop them and
                // set nothing.
                let set_headers = if ident.anon {
                    Vec::new()
                } else {
                    req.users.iter().find(|u| u.id == ident.id).map(|u| u.headers.clone()).unwrap_or_default()
                };
                let replay = ReplayRequest {
                    id: tid,
                    method: None,
                    target: None,
                    set_headers,
                    remove_headers: remove_auth.clone(),
                    body: None,
                };
                let cell = match self.replay(replay, initiator).await {
                    Ok(ex) => {
                        report.sent += 1;
                        authcheck::Cell {
                            identity: ident.id.clone(),
                            status: ex.status,
                            len: ex.resp_size.unwrap_or(ex.resp_body.len() as i64),
                            ms: ex.duration_ms,
                            exchange_id: ex.id,
                            error: ex.error,
                            sig: sha256_hex(&ex.resp_body),
                        }
                    }
                    // Out of scope stops the whole check: every target shares a
                    // host family, so the first refusal means none can be sent.
                    Err(e @ SendError::OutOfScope { .. }) if report.sent == 0 => return Err(e),
                    Err(e) => authcheck::Cell {
                        identity: ident.id.clone(),
                        status: None,
                        len: 0,
                        ms: 0,
                        exchange_id: tid,
                        error: Some(e.to_string()),
                        sig: String::new(),
                    },
                };
                cells.push(cell);
            }
            let notes = authcheck::notes_for(&cells, &identities);
            report.rows.push(authcheck::TargetRow {
                target_id: tid,
                method: orig.method,
                host: orig.host,
                path: orig.path,
                cells,
                notes,
            });
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

// ---- extensions -------------------------------------------------------------------

/// Extensions in effect, reloaded when they are installed, removed, enabled
/// or disabled.
#[derive(Default)]
struct ExtensionState {
    library: Option<ExtensionLibrary>,
    loaded_stamp: Option<extension::Stamp>,
    set: Arc<LoadedSet>,
}

/// Exchanges handed to an extension in one call.
const EXTENSION_BATCH: usize = 10;

/// What running an extension over captured traffic did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExtensionRun {
    pub extension: String,
    /// Exchanges it was given.
    pub exchanges: usize,
    pub notes: usize,
    /// Findings it proposed that were not already there.
    pub proposed: usize,
    /// Lines it logged, capped.
    pub logs: Vec<String>,
    /// Set when it was stopped and switched off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// Set when a program extension could not finish, e.g. its program is
    /// not installed. It stays on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// The outcome of a parameter probe (see [`Engine::run_param_probe`]).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ParamProbeReport {
    pub target: String,
    /// The baseline request's exchange id.
    pub baseline_exchange: i64,
    /// Candidate parameters sent, not counting the baseline and calibration.
    pub sent: usize,
    /// Candidates whose request could not be sent.
    pub errors: usize,
    /// Parameter names that changed the response.
    pub influential: Vec<String>,
    /// Whether a finding was proposed (false when one was already there).
    pub proposed: bool,
}

/// A probe request: the target endpoint, optionally with one extra query
/// parameter `name=<marker>` appended. Only a parameter name and a benign
/// marker are added; the probe sends no payload of its own.
fn probe_request(url: &str, param: Option<&str>, marker: &str) -> SendRequest {
    let url = match param {
        Some(p) => format!("{url}{}{p}={marker}", if url.contains('?') { '&' } else { '?' }),
        None => url.to_string(),
    };
    SendRequest { method: "GET".into(), url, headers: vec![], body: None, body_base64: None, as_user: None }
}

impl Engine {
    /// Uses this library for extensions and starts feeding newly captured
    /// traffic to the enabled ones.
    pub fn set_extension_library(self: &Arc<Self>, library: ExtensionLibrary) {
        {
            let mut e = self.extensions.lock().unwrap();
            e.library = Some(library);
            e.loaded_stamp = None;
        }
        let mut feed = self.extension_feed.lock().unwrap();
        if feed.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<i64>();
        let engine = Arc::downgrade(self);
        let started = std::thread::Builder::new().name("plonix-extensions".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut ids = vec![first];
                while ids.len() < EXTENSION_BATCH * 5
                    && let Ok(id) = rx.try_recv()
                {
                    ids.push(id);
                }
                let Some(engine) = engine.upgrade() else { break };
                engine.feed_extensions(&ids);
            }
        });
        if started.is_ok() {
            *feed = Some(tx);
        }
    }

    /// The enabled extensions, reloading them if anything changed.
    pub fn extensions(&self) -> Arc<LoadedSet> {
        let mut e = self.extensions.lock().unwrap();
        let Some(stamp) = e.library.as_ref().map(ExtensionLibrary::stamp) else { return e.set.clone() };
        if e.loaded_stamp != Some(stamp) {
            let set = e.library.as_ref().map(ExtensionLibrary::load).unwrap_or_default();
            for p in &set.problems {
                tracing::warn!("extensions: {p}");
            }
            e.set = Arc::new(set);
            e.loaded_stamp = Some(stamp);
        }
        e.set.clone()
    }

    /// Runs one extension over exchanges it may see: in-scope ones, and the
    /// rest only with `read-out-of-scope`. A fault switches it off with the
    /// reason, and never reaches further than this call.
    fn run_extension(&self, ext: &Loaded, exchanges: &[Exchange]) -> Result<(sandbox::Output, usize), sandbox::Fault> {
        let Runner::Wasm(compiled) = &ext.runner else { return Ok((sandbox::Output::default(), 0)) };
        let rules = self.rules();
        let out_of_scope = ext.granted.contains(&Capability::ReadOutOfScope);
        let visible: Vec<(&Exchange, bool)> =
            exchanges.iter().map(|ex| (ex, rules.in_scope(&ex.host))).filter(|(_, in_scope)| *in_scope || out_of_scope).collect();
        if visible.is_empty() || !ext.granted.contains(&Capability::ReadTraffic) {
            return Ok((sandbox::Output::default(), 0));
        }
        let ids: Vec<i64> = visible.iter().map(|(ex, _)| ex.id).collect();
        let batch = sandbox::batch_json(&visible);
        match sandbox::analyze(compiled, &ext.granted, &batch, &ids, *self.extension_limits.read().unwrap()) {
            Ok(out) => {
                for line in &out.logs {
                    tracing::info!("extension {}: {line}", ext.name);
                }
                Ok((out, visible.len()))
            }
            Err(fault) => {
                let reason = format!("Plonix stopped it and switched it off: {fault}. Turn it back on once it is fixed.");
                tracing::warn!("extension {} {}: {reason}", ext.name, ext.version);
                let mut e = self.extensions.lock().unwrap();
                if let Some(lib) = &e.library
                    && let Err(err) = lib.disable(&ext.name, &reason)
                {
                    tracing::error!("extension {}: could not record that it is switched off: {err:#}", ext.name);
                }
                // Drop it now, whatever the file system's clock says.
                let rest = e.set.extensions.iter().filter(|x| x.name != ext.name).cloned().collect();
                e.set = Arc::new(LoadedSet { extensions: rest, problems: e.set.problems.clone() });
                Err(fault)
            }
        }
    }

    /// Stores what an extension proposed as open findings attributed to it,
    /// skipping ones it already proposed. Returns how many are new.
    fn store_proposals(&self, ext: &Loaded, proposals: &[sandbox::Proposal]) -> Result<usize> {
        if proposals.is_empty() {
            return Ok(0);
        }
        let by = extension_author(&ext.name);
        let existing: BTreeSet<String> = self.store.findings()?.into_iter().filter(|f| f.created_by == by).map(|f| f.title).collect();
        let mut added = 0;
        for p in proposals.iter().filter(|p| !existing.contains(&p.title)) {
            let note = format!("Proposed by the extension {} {}. Not confirmed: check it before you rely on it.", ext.name, ext.version);
            let description = if p.description.is_empty() { note } else { format!("{}\n\n{note}", p.description) };
            let f = crate::model::NewFinding { title: p.title.clone(), severity: p.severity.clone(), description, exchange_ids: p.exchange_ids.clone() };
            self.store.add_finding(&f, &by)?;
            added += 1;
        }
        Ok(added)
    }

    /// Hands newly recorded exchanges to every enabled extension.
    fn feed_extensions(&self, ids: &[i64]) {
        let set = self.extensions();
        if set.extensions.is_empty() {
            return;
        }
        let exchanges: Vec<Exchange> = ids.iter().filter_map(|id| self.store.get_exchange(*id).ok().flatten()).collect();
        for ext in &set.extensions {
            if let Runner::Program(program) = &ext.runner {
                if let Err(e) = self.run_program(ext, program, &exchanges) {
                    self.program_problem(ext, &e);
                }
                continue;
            }
            for chunk in exchanges.chunks(EXTENSION_BATCH) {
                match self.run_extension(ext, chunk) {
                    Ok((out, _)) => {
                        if let Err(e) = self.store_proposals(ext, &out.proposals) {
                            tracing::error!("extension {}: storing its findings: {e:#}", ext.name);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    /// Queues a recorded exchange for the enabled extensions.
    fn queue_for_extensions(&self, id: i64) {
        if let Some(tx) = self.extension_feed.lock().unwrap().as_ref() {
            let _ = tx.send(id);
        }
    }

    /// Runs one extension over everything captured so far (it may only see
    /// what its capabilities allow), storing the findings it proposes.
    pub fn run_extension_on_traffic(&self, name: &str) -> Result<ExtensionRun> {
        let set = self.extensions();
        let Some(ext) = set.extensions.iter().find(|e| e.name == name) else {
            anyhow::bail!("no enabled extension named `{}` (see `plonix extensions`)", crate::detect::clean(name, 64));
        };
        let mut run = ExtensionRun { extension: ext.name.clone(), ..Default::default() };
        let mut last = 0;
        if let Runner::Program(program) = &ext.runner {
            match crate::program::get(program).map(|p| p.kind) {
                Some(crate::program::Kind::Enumerate) => {
                    match self.discover_subdomains(&ext) {
                        Ok(found) => run.notes += found,
                        Err(e) => run.problem = Some(e),
                    }
                    return Ok(run);
                }
                Some(crate::program::Kind::Probe) => {
                    run.problem = Some(format!(
                        "{} probes one endpoint at a time. Run it on an in-scope request instead (`plonix extensions probe {} <url>`).",
                        ext.name, ext.name
                    ));
                    return Ok(run);
                }
                _ => {}
            }
            loop {
                let batch = self.store.exchanges_after(last, crate::program::BATCH)?;
                let Some(tail) = batch.last() else { break };
                last = tail.id;
                match self.run_program(ext, program, &batch) {
                    Ok((seen, hits)) => {
                        run.exchanges += seen;
                        run.notes += hits;
                    }
                    Err(e) => {
                        run.problem = Some(e);
                        break;
                    }
                }
            }
            return Ok(run);
        }
        loop {
            let batch = self.store.exchanges_after(last, EXTENSION_BATCH)?;
            let Some(tail) = batch.last() else { break };
            last = tail.id;
            match self.run_extension(ext, &batch) {
                Ok((out, seen)) => {
                    run.exchanges += seen;
                    run.notes += out.notes.len();
                    run.proposed += self.store_proposals(ext, &out.proposals)?;
                    run.logs.extend(out.logs.into_iter().take(50usize.saturating_sub(run.logs.len())));
                }
                Err(fault) => {
                    run.stopped = Some(format!("Stopped and switched off: {fault}."));
                    break;
                }
            }
        }
        Ok(run)
    }

    /// Notes from the enabled extensions on one exchange, shown in the Lens
    /// next to what Plonix spotted and labelled with the extension's name.
    /// Findings they would propose are shown, not stored.
    pub fn extension_insights(&self, ex: &Exchange) -> Vec<Insight> {
        let set = self.extensions();
        let mut out = vec![];
        let stored = self.store.extension_hits(ex.id).unwrap_or_default();
        for ext in &set.extensions {
            if let Runner::Program(_) = &ext.runner {
                match stored.iter().find(|(name, version, _)| *name == ext.name && *version == ext.version) {
                    Some((_, _, json)) => out.extend(program_insights(ext, json)),
                    // Not scanned yet: queue it, and its hits show next time.
                    None => self.queue_for_extensions(ex.id),
                }
                continue;
            }
            let Ok((result, _)) = self.run_extension(ext, std::slice::from_ref(ex)) else { continue };
            let from = format!("From the extension {} {}, not from Plonix", ext.name, ext.version);
            let notes = result.notes.into_iter().map(|n| (n.tag, n.text));
            let proposals = result.proposals.into_iter().map(|p| (format!("proposes: {}", p.severity), p.title));
            for (tag, text) in notes.chain(proposals).take(20) {
                out.push(Insight {
                    kind: format!("extension:{}", ext.name),
                    category: InsightCategory::Extension,
                    label: tag,
                    side: InsightSide::Response,
                    location: format!("extension {}", ext.name),
                    value: text,
                    decoded: None,
                    notes: vec![from.clone()],
                    count: 1,
                });
            }
        }
        out
    }

    /// Runs a program extension over the exchanges it may see that this
    /// version has not scanned yet, storing what it found. Returns how many
    /// exchanges it scanned and how many secrets it found.
    fn run_program(&self, ext: &Loaded, program: &str, exchanges: &[Exchange]) -> std::result::Result<(usize, usize), String> {
        if !ext.granted.contains(&Capability::ReadTraffic) || !ext.granted.contains(&Capability::RunProgram) {
            return Ok((0, 0));
        }
        let rules = self.rules();
        let out_of_scope = ext.granted.contains(&Capability::ReadOutOfScope);
        let visible: Vec<&Exchange> = exchanges.iter().filter(|ex| out_of_scope || rules.in_scope(&ex.host)).collect();
        let ids: Vec<i64> = visible.iter().map(|ex| ex.id).collect();
        let done = self.store.extension_scanned(&ext.name, &ext.version, &ids).map_err(|e| e.to_string())?;
        let todo: Vec<&Exchange> = visible.into_iter().filter(|ex| !done.contains(&ex.id)).collect();
        let mut hits = 0;
        for chunk in todo.chunks(crate::program::BATCH) {
            let found = crate::program::scan(program, chunk)?;
            hits += found.values().map(Vec::len).sum::<usize>();
            let rows: Vec<(i64, String)> = found.iter().map(|(id, h)| (*id, serde_json::to_string(h).unwrap_or_else(|_| "[]".into()))).collect();
            self.store.put_extension_hits(&ext.name, &ext.version, &rows).map_err(|e| e.to_string())?;
        }
        if !todo.is_empty() {
            self.program_problems.lock().unwrap().remove(&ext.name);
        }
        Ok((todo.len(), hits))
    }

    /// Runs the extension's enumeration tool over every accepted scope domain
    /// and records the subdomains it finds as scope *suggestions*. Returns how
    /// many it suggested. The tool reads public sources; Plonix sends nothing
    /// to the target and brings nothing into scope — each suggestion waits for
    /// the user's own accept/reject decision.
    fn discover_subdomains(&self, ext: &Loaded) -> std::result::Result<usize, String> {
        if !ext.granted.contains(&Capability::RunProgram) || !ext.granted.contains(&Capability::SuggestScope) {
            return Err(format!("{} needs permission to run its tool and to suggest scope", ext.name));
        }
        let Runner::Program(program) = &ext.runner else { return Ok(0) };
        let rules = self.rules();
        // The accepted domains to enumerate, deduplicated so a seed and one of
        // its own subdomains are not both enumerated.
        let mut domains: Vec<String> = rules
            .rules
            .iter()
            .filter(|r| r.decision == Decision::Accepted)
            .map(|r| crate::scope::normalize_host(&r.pattern))
            .filter(|h| !h.is_empty())
            .collect();
        domains.sort();
        domains.dedup();
        let all = domains.clone();
        domains.retain(|d| !all.iter().any(|other| other != d && crate::scope::is_subdomain_of(d, other)));
        if domains.is_empty() {
            return Err("no accepted scope domain to enumerate yet. Accept a domain in Scope first.".into());
        }
        let now = crate::model::now_ms();
        let mut suggested = 0;
        for domain in domains {
            let found = crate::program::enumerate(program, &domain)?;
            let evidence: Vec<crate::scope::NewEvidence> = found
                .into_iter()
                .filter(|h| rules.decide_domain(h) == Decision::Unknown && !crate::scope::is_noise(h))
                .map(|h| crate::scope::NewEvidence {
                    domain: h,
                    kind: crate::scope::EvidenceKind::Discovered,
                    via: domain.clone(),
                    detail: format!("subdomain enumeration of {domain} ({})", ext.name),
                })
                .collect();
            suggested += evidence.len();
            self.store.add_suggestions(&evidence, now).map_err(|e| e.to_string())?;
        }
        Ok(suggested)
    }

    /// Probes one in-scope endpoint for undocumented query parameters. Plonix
    /// sends every request itself through [`Engine::send`], so each is
    /// scope-enforced and recorded; the extension supplies only the candidate
    /// names, never a payload. Parameters that change the response become one
    /// unconfirmed finding for a person to review.
    pub async fn run_param_probe(&self, name: &str, target_url: &str) -> std::result::Result<ParamProbeReport, SendError> {
        let set = self.extensions();
        let ext = set
            .extensions
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| SendError::BadRequest(format!("no enabled extension named `{}` (see `plonix extensions`)", crate::detect::clean(name, 64))))?
            .clone();
        let is_probe = matches!(&ext.runner, Runner::Program(p) if crate::program::get(p).map(|p| p.kind) == Some(crate::program::Kind::Probe));
        if !is_probe {
            return Err(SendError::BadRequest(format!("{name} is not a parameter probe")));
        }
        if !ext.granted.contains(&Capability::ScopedRequests) || !ext.granted.contains(&Capability::RunProgram) {
            return Err(SendError::BadRequest(format!("{name} needs permission to send scoped requests and run its probe")));
        }
        let initiator = extension_author(name);
        const MARKER: &str = "plnxprobe7q";

        // Baseline, then a calibration probe with a name nothing should read,
        // to tell whether responses are stable enough to compare by length.
        let baseline = self.send(probe_request(target_url, None, MARKER), &initiator).await?;
        let calib = self.send(probe_request(target_url, Some("plnxcalib9z"), MARKER), &initiator).await.ok();
        let base_len = baseline.resp_body.len() as i64;
        let base_status = baseline.status;
        let calib_len = calib.as_ref().map(|c| c.resp_body.len() as i64);
        let length_reliable = calib.as_ref().map_or(true, |c| c.status == base_status && (c.resp_body.len() as i64 - base_len).abs() <= 8);

        let mut report = ParamProbeReport { target: target_url.to_string(), baseline_exchange: baseline.id, ..Default::default() };
        let mut influential: Vec<(String, i64)> = vec![];
        for cand in crate::program::PARAM_NAMES.iter().take(crate::program::MAX_PROBES) {
            let ex = match self.send(probe_request(target_url, Some(cand), MARKER), &initiator).await {
                Ok(ex) => ex,
                // Scope is the same host throughout: a refusal stops the probe.
                Err(e @ SendError::OutOfScope { .. }) => return Err(e),
                Err(_) => {
                    report.errors += 1;
                    continue;
                }
            };
            report.sent += 1;
            let len = ex.resp_body.len() as i64;
            let reflected = String::from_utf8_lossy(&ex.resp_body).contains(MARKER);
            let status_changed = ex.status != base_status;
            let len_changed = length_reliable && (len - base_len).abs() > 64 && Some(len) != calib_len;
            if reflected || status_changed || len_changed {
                influential.push((cand.to_string(), ex.id));
            }
        }
        report.influential = influential.iter().map(|(n, _)| n.clone()).collect();

        if !influential.is_empty() {
            let host = crate::scope::normalize_host(target_url);
            let names = report.influential.join(", ");
            let title = format!("Undocumented parameters on {host}");
            let mut ids = vec![baseline.id];
            ids.extend(influential.iter().map(|(_, id)| *id));
            let description = format!(
                "The parameter probe found query parameters that change this endpoint's response, so the application reads them although they are not documented here: {names}.\n\n\
                 Each was sent once as `name={MARKER}` through Plonix's scope-gated send path and compared with the baseline (exchange {}). Review whether any reach data or behaviour that should not be.\n\n\
                 Proposed by the extension {} {}. Not confirmed: check it before you rely on it.",
                baseline.id, ext.name, ext.version
            );
            let by = extension_author(&ext.name);
            let existing: BTreeSet<String> = self.store.findings()?.into_iter().filter(|f| f.created_by == by).map(|f| f.title).collect();
            if !existing.contains(&title) {
                let f = crate::model::NewFinding { title, severity: "info".into(), description, exchange_ids: ids };
                self.store.add_finding(&f, &by)?;
                report.proposed = true;
            }
        }
        Ok(report)
    }

    /// Logs why a program extension could not run, once until it changes.
    fn program_problem(&self, ext: &Loaded, problem: &str) {
        let mut seen = self.program_problems.lock().unwrap();
        if seen.get(&ext.name).map(String::as_str) != Some(problem) {
            tracing::warn!("extension {} {}: {problem}", ext.name, ext.version);
            seen.insert(ext.name.clone(), problem.to_string());
        }
    }

    /// The limits each extension call runs under.
    pub fn set_extension_limits(&self, limits: sandbox::Limits) {
        *self.extension_limits.write().unwrap() = limits;
    }
}

/// What a program extension found in one exchange, as Lens insights: exposed
/// secrets, labelled with the extension's name.
fn program_insights(ext: &Loaded, json: &str) -> Vec<Insight> {
    let hits: Vec<crate::program::Hit> = serde_json::from_str(json).unwrap_or_default();
    hits.into_iter()
        .map(|h| {
            let mut notes = vec![format!("Found by the extension {} {}. Not checked with the service it belongs to.", ext.name, ext.version)];
            notes.extend(h.notes);
            Insight {
                kind: format!("extension:{}", ext.name),
                category: InsightCategory::Secret,
                label: crate::program::label(&h.detector),
                side: h.side,
                location: h.location,
                value: h.value,
                decoded: None,
                notes,
                count: 1,
            }
        })
        .collect()
}

/// How findings an extension proposed are attributed.
pub fn extension_author(name: &str) -> String {
    format!("extension:{name}")
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
    pub agent_token: String,
}

/// Runs scope analysis over exchanges in order and stores what it learned
/// in one go. A token the batch itself teaches counts for the exchanges after
/// it, as if each had been written right away.
fn analyze_into<'a>(store: &Store, exchanges: impl Iterator<Item = (&'a Exchange, i64)>, rules: &ScopeRules) -> Result<()> {
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut learned = Vec::new();
    for (ex, id) in exchanges {
        let analysis = scope::analyze(ex, rules, &|h| store.token_owner(h).or_else(|| owners.get(h).cloned()));
        if analysis.tokens.is_empty() && analysis.evidence.is_empty() {
            continue;
        }
        let host = scope::normalize_host(&ex.host);
        for t in &analysis.tokens {
            owners.entry(t.clone()).or_insert_with(|| host.clone());
        }
        learned.push(Learned { exchange_id: id, ts: ex.ts, host, tokens: analysis.tokens, evidence: analysis.evidence });
    }
    store.add_analysis(&learned)
}

/// Rebuilds scope evidence for everything in `store` against `rules`.
pub fn reanalyze(store: &Store, rules: &ScopeRules) -> Result<()> {
    store.clear_analysis()?;
    // Pass 1 learns session tokens from in-scope traffic, so that token
    // reuse is detected regardless of the order requests were made in.
    let mut last = 0;
    loop {
        let batch = store.exchanges_after(last, 500)?;
        let Some(tail) = batch.last() else { break };
        last = tail.id;
        let tokens: Vec<Learned> = batch
            .iter()
            .filter(|e| rules.in_scope(&e.host))
            .map(|ex| Learned { host: scope::normalize_host(&ex.host), tokens: scope::session_tokens(ex, true), ..Default::default() })
            .collect();
        store.add_analysis(&tokens)?;
    }
    let mut last = 0;
    loop {
        let batch = store.exchanges_after(last, 500)?;
        let Some(tail) = batch.last() else { break };
        last = tail.id;
        analyze_into(store, batch.iter().map(|ex| (ex, ex.id)), rules)?;
    }
    Ok(())
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
    engine.set_filter_library(FilterLibrary::new(&config.home));
    engine.set_list_library(ListLibrary::new(&config.home));
    engine.set_extension_library(ExtensionLibrary::new(&config.home));
    if project.file.demo {
        engine.set_responder(crate::demo::responder());
    }
    start_with(engine, config).await
}

pub async fn start_with(engine: Arc<Engine>, config: &EngineConfig) -> Result<Running> {
    let token = config.home.load_or_create_token()?;
    let agent_token = config.home.load_or_create_agent_token()?;
    engine.start_recorder();
    let api = TcpListener::bind(config.api_addr).await.with_context(|| format!("binding API to {}", config.api_addr))?;
    let proxy_addr = engine.bind_proxy(config.proxy_addr, config.proxy_port_fallback).await?;
    let api_addr = api.local_addr()?;
    let router = crate::api::router(
        engine.clone(),
        crate::api::Tokens { user: token.clone(), agent: agent_token.clone() },
        api_addr,
        config.home.clone(),
    );
    let shutdown_engine = engine.clone();
    tokio::spawn(async move {
        let _ = axum::serve(api, router).with_graceful_shutdown(async move { shutdown_engine.stopped().await }).await;
    });
    Ok(Running { engine, proxy_addr, api_addr, token, agent_token })
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
