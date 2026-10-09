//! The engine ties the proxy, store, scope and upstream client together.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::ca::CertAuthority;
use crate::clientcert::{CertInfo, ClientCerts, StoredCert};
use crate::har;
use crate::detect::{self, Detection, Detector, HostTech};
use crate::sandbox;
use crate::model::{Exchange, Headers, Source, WsMessage, now_ms};
use crate::paths::EngineInfo;
use crate::detectorpack::{DetectorLibrary, DetectorSet};
use crate::filterpack::{FilterLibrary, FilterSet};
use crate::listpack::{ListLibrary, ListSet};
use crate::intercept::{InterceptOptions, Interceptor};
use crate::project::PruneReport;
use crate::replace::RuleSet;
use crate::rulepack::{Library, PackInfo, sha256_hex};
use crate::exclude::{self, ExcludedDomain, Exclusions, Group, GroupStatus};
use crate::bounty;
use crate::scope::{self, Decision, Rule, ScopeRules};
use crate::settings::ProxySettings;
use crate::store::{Learned, Store};
use crate::upstream::{InboundResponse, OutboundRequest, Upstream, UpstreamOptions, host_matches};

mod automation;
mod extensions;
mod send;

pub use extensions::{ExtensionRun, ParamProbeReport, extension_author};
use extensions::ExtensionState;

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
    /// Proxy ports of saved users' own browser windows, by user id.
    user_proxies: tokio::sync::Mutex<HashMap<String, SocketAddr>>,
    interception: RwLock<Interception>,
    /// The project folder this engine records into, when it has one.
    pub project_ref: OnceLock<ProjectRef>,
    /// Captured exchanges are recorded in arrival order by one worker, so a
    /// response is always analyzed before requests that follow it.
    recorder: mpsc::UnboundedSender<Queued>,
    recorder_rx: Mutex<Option<mpsc::UnboundedReceiver<Queued>>>,
    detection: Mutex<Reloading<Library, LoadedRules>>,
    filters: Mutex<Reloading<FilterLibrary, FilterSet>>,
    detectors: Mutex<Reloading<DetectorLibrary, DetectorSet>>,
    lists: Mutex<Reloading<ListLibrary, ListSet>>,
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

/// What a pack library holds, loaded again when its files change: named
/// Traffic filters, detector packs (Mind Reader suggestions), Bench payload
/// lists and detection rules (`plonix rules add/remove` while the engine runs).
struct Reloading<L, T> {
    library: Option<L>,
    loaded_stamp: Option<Option<std::time::SystemTime>>,
    value: Arc<T>,
}

impl<L, T: Default> Default for Reloading<L, T> {
    fn default() -> Self {
        Self { library: None, loaded_stamp: None, value: Arc::default() }
    }
}

impl<L, T> Reloading<L, T> {
    fn set_library(&mut self, library: L) {
        self.library = Some(library);
        self.loaded_stamp = None;
    }

    /// The loaded value, loading it again if the library's files changed
    /// since. `load` gets no library when none was set.
    fn get(&mut self, stamp: impl Fn(&L) -> Option<std::time::SystemTime>, load: impl FnOnce(Option<&L>) -> T) -> Arc<T> {
        let now = self.library.as_ref().and_then(stamp);
        if self.loaded_stamp != Some(now) {
            self.value = Arc::new(load(self.library.as_ref()));
            self.loaded_stamp = Some(now);
        }
        self.value.clone()
    }
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
            user_proxies: tokio::sync::Mutex::default(),
            interception: RwLock::new(Interception { decrypt: true, passthrough: vec![] }),
            project_ref: OnceLock::new(),
            detection: Mutex::default(),
            filters: Mutex::default(),
            detectors: Mutex::default(),
            lists: Mutex::default(),
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

    /// The proxy port for a saved user's own browser window, on loopback.
    /// Started the first time the user's window opens, then kept.
    pub async fn user_proxy(self: &Arc<Self>, id: &str) -> Result<SocketAddr> {
        let mut ports = self.user_proxies.lock().await;
        if let Some(addr) = ports.get(id) {
            return Ok(*addr);
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.context("binding a proxy port for a saved user")?;
        let addr = listener.local_addr()?;
        tokio::spawn(crate::proxy::serve_as(listener, self.clone(), Some(id.to_string())));
        ports.insert(id.to_string(), addr);
        Ok(addr)
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

    /// Adds a match-and-replace rule and applies it at once.
    pub fn add_replace_rule(&self, rule: &crate::replace::Rule) -> Result<crate::replace::Rule> {
        let stored = self.store.add_replace_rule(rule)?;
        self.reload_replace_rules()?;
        Ok(stored)
    }

    pub fn update_replace_rule(&self, rule: &crate::replace::Rule) -> Result<bool> {
        let changed = self.store.update_replace_rule(rule)?;
        self.reload_replace_rules()?;
        Ok(changed)
    }

    pub fn delete_replace_rule(&self, id: i64) -> Result<bool> {
        let removed = self.store.delete_replace_rule(id)?;
        self.reload_replace_rules()?;
        Ok(removed)
    }

    /// Reads the rules again after they changed.
    fn reload_replace_rules(&self) -> Result<()> {
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
            self.store.compact()?;
        }
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
        self.filters.lock().unwrap().set_library(library);
    }

    /// Named filters in effect (`is:name`), reloading them if packs changed.
    pub fn filters(&self) -> Arc<FilterSet> {
        self.filters.lock().unwrap().get(FilterLibrary::stamp, |lib| {
            let set = lib.map_or_else(|| FilterLibrary::at(std::path::Path::new("/nonexistent")).load(), FilterLibrary::load);
            for p in &set.problems {
                tracing::warn!("filters: {p}");
            }
            set
        })
    }

    /// Loads installed detector packs from this library (the built-in pack is
    /// always loaded).
    pub fn set_detector_library(&self, library: DetectorLibrary) {
        self.detectors.lock().unwrap().set_library(library);
    }

    /// Detectors in effect (Mind Reader suggestions), reloading if packs changed.
    pub fn detectors(&self) -> Arc<DetectorSet> {
        self.detectors.lock().unwrap().get(DetectorLibrary::stamp, |lib| {
            let set = lib.map_or_else(|| DetectorLibrary::at(std::path::Path::new("/nonexistent")).load(), DetectorLibrary::load);
            for p in &set.problems {
                tracing::warn!("detectors: {p}");
            }
            set
        })
    }

    pub fn set_list_library(&self, library: ListLibrary) {
        self.lists.lock().unwrap().set_library(library);
    }

    /// The payload lists in effect, reloading them if installed packs changed.
    pub fn lists(&self) -> Arc<ListSet> {
        self.lists.lock().unwrap().get(ListLibrary::stamp, |lib| {
            let set = lib.map_or_else(|| ListLibrary::at(std::path::Path::new("/nonexistent")).load(), ListLibrary::load);
            for p in &set.problems {
                tracing::warn!("lists: {p}");
            }
            set
        })
    }

    pub fn set_rule_library(&self, library: Library) {
        self.detection.lock().unwrap().set_library(library);
    }

    /// The detection rules in effect, reloading them if packs changed.
    pub fn detection_rules(&self) -> Arc<LoadedRules> {
        self.detection.lock().unwrap().get(Library::stamp, |lib| {
            let loaded = lib.map_or_else(|| Library::at(std::path::Path::new("/nonexistent")).load(), Library::load);
            for p in &loaded.problems {
                tracing::warn!("detection rules: {p}");
            }
            LoadedRules {
                detector: loaded.detector(),
                packs: loaded.packs.into_iter().map(|(_, info)| info).collect(),
                problems: loaded.problems,
            }
        })
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
            // normalize_host cuts at '/', which would turn a range into one address.
            None if crate::bounty::is_cidr(domain.trim()) => (domain.trim().to_ascii_lowercase(), false),
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
        let pattern = if crate::bounty::is_cidr(domain.trim()) { domain.trim().to_ascii_lowercase() } else { scope::normalize_host(domain.trim().trim_start_matches("*.")) };
        let removed = self.store.delete_rule(&pattern)?;
        *self.rules.write().unwrap() = self.store.rules()?;
        self.rescan()?;
        Ok(removed)
    }

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
