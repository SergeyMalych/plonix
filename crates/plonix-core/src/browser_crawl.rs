//! The browser crawl: the same bounded, read-only walk as a plain crawl, in
//! a headless Chromium-based browser so JavaScript apps render.
//!
//! The browser is the user's own (whatever [`browser::detect`] finds), run
//! headless on a throwaway profile and routed through the Plonix proxy, so
//! every request the pages make is captured in Traffic and shows up on the
//! Map. It is driven over the DevTools protocol ([`crate::cdp`]):
//!
//! - every request the browser makes is paused and checked against scope
//!   first; one to a host that is not accepted is failed before it leaves,
//!   so pages, scripts and redirects cannot reach past accepted scope;
//! - pages are visited breadth-first from the start URL, within the page,
//!   depth and time budget, each until its network goes quiet;
//! - links come from the rendered DOM: anchors, frames, router links and
//!   the routes the app pushes onto its history (see [`crawl::INIT_JS`]);
//! - with clicking on, buttons and script links outside forms whose label
//!   does not look destructive are clicked, and any page they lead to is
//!   followed like a link;
//! - forms are listed, never submitted.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::process::{Child, Command};

use crate::browser::{self, Kind};
use crate::cdp::{Cdp, Event};
use crate::crawl::{self, CrawlReport, CrawlRequest, PageScan};
use crate::engine::{Engine, SendError};
use crate::scope::ScopeRules;

/// Shown when no Chromium-based browser can be found.
pub const NO_CHROMIUM: &str = "a browser crawl needs a Chromium-based browser (Google Chrome, Chromium, Brave or Microsoft Edge) and none was found. \
     Install one, or point PLONIX_BROWSER at its executable, then try again. A crawl without a browser works meanwhile.";

/// How long a page may take to load and settle.
const PAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a click may take to settle.
const CLICK_TIMEOUT: Duration = Duration::from_secs(4);
/// The network counts as idle after this long with nothing starting or ending...
const QUIET: Duration = Duration::from_millis(500);
/// ...not counting requests open longer than this (long polls, streams).
const LONG_REQUEST: Duration = Duration::from_secs(5);
/// How long the browser may take to start.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// What the event pump tracks for the crawl's page.
#[derive(Default)]
struct Net {
    inflight: HashMap<String, Instant>,
    last: Option<Instant>,
    loaded: bool,
    blocked: BTreeSet<String>,
}

type Shared = Arc<Mutex<Net>>;

/// Crawls from `start` (an in-scope URL on `host`) in a headless browser.
pub async fn run(engine: &Engine, host: &str, start: String, req: &CrawlRequest) -> Result<CrawlReport, SendError> {
    // The Plonix home holds the downloaded Plonix browser, if there is one.
    let home = crate::paths::Home::resolve(None).map_err(|e| SendError::BadRequest(format!("{e:#}")))?;
    let found = browser::detect(&home).filter(|b| b.kind == Kind::Chromium).ok_or_else(|| SendError::BadRequest(NO_CHROMIUM.into()))?;
    let proxy = engine.proxy_addr().ok_or_else(|| SendError::BadRequest("the proxy is not running, so a browser crawl has nothing to route through".into()))?;
    // The proxy may listen on every interface; the browser reaches it on loopback.
    let proxy = if proxy.ip().is_unspecified() { SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), proxy.port()) } else { proxy };

    let mut report = CrawlReport::new(host);
    report.browser = Some(found.name.clone());
    if engine.intercept.is_on() {
        report.notes.push("Intercept is on, so the browser's requests waited in Intercept; turn it off for an unattended crawl".into());
    }

    let profile = TempProfile::create().map_err(SendError::Other)?;
    let args = browser::headless_args(&profile.0, &proxy.to_string(), &engine.ca.spki_sha256());
    let mut cmd = Command::new(&found.exe);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| SendError::Other(anyhow!("could not start {} ({}): {e}", found.name, found.exe.display())))?;

    let outcome = async {
        let (port, path) = wait_for_devtools(&profile.0, &mut child).await.with_context(|| format!("starting {} headless", found.name))?;
        let (cdp, events) = Cdp::connect(port, &path).await?;
        let r = Crawler::start(cdp.clone(), events, engine.rules()).await?.walk(start, req, &mut report).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), cdp.call(None, "Browser.close", json!({}))).await;
        r
    }
    .await;

    if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    drop(profile);
    outcome.map_err(SendError::Other)?;
    Ok(report)
}

/// Waits for the browser to write its DevTools port and path into the profile.
async fn wait_for_devtools(profile: &Path, child: &mut Child) -> Result<(u16, String)> {
    let file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let mut lines = text.lines();
            if let (Some(Ok(port)), Some(path)) = (lines.next().map(|p| p.trim().parse::<u16>()), lines.next()) {
                return Ok((port, path.trim().to_string()));
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("the browser exited before it was ready ({status})");
        }
        if Instant::now() > deadline {
            bail!("the browser did not open its DevTools port within {}s", START_TIMEOUT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct Crawler {
    cdp: Cdp,
    session: String,
    net: Shared,
    rules: ScopeRules,
}

impl Crawler {
    /// Opens a page, starts the event pump and arms scope enforcement before
    /// anything is loaded.
    async fn start(cdp: Cdp, events: tokio::sync::mpsc::UnboundedReceiver<Event>, rules: ScopeRules) -> Result<Crawler> {
        let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" })).await?;
        let target_id = target["targetId"].as_str().context("no target id")?.to_string();
        let attached = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true })).await?;
        let session = attached["sessionId"].as_str().context("no session id")?.to_string();
        let net: Shared = Arc::default();
        tokio::spawn(pump(cdp.clone(), events, rules.clone(), session.clone(), net.clone()));

        let s = Some(session.as_str());
        cdp.call(s, "Fetch.enable", json!({ "patterns": [{ "urlPattern": "*" }] })).await?;
        cdp.call(s, "Page.enable", json!({})).await?;
        cdp.call(s, "Network.enable", json!({})).await?;
        let _ = cdp.call(s, "Network.setBypassServiceWorker", json!({ "bypass": true })).await;
        cdp.call(s, "Page.addScriptToEvaluateOnNewDocument", json!({ "source": crawl::INIT_JS })).await?;
        // Frames in other processes and workers get the same scope check.
        cdp.call(s, "Target.setAutoAttach", json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true })).await?;
        Ok(Crawler { cdp, session, net, rules })
    }

    async fn walk(&self, start: String, req: &CrawlRequest, report: &mut CrawlReport) -> Result<()> {
        let max_pages = req.max_pages.unwrap_or(crawl::DEFAULT_MAX_PAGES).clamp(1, crawl::MAX_PAGES_CEIL);
        let max_depth = req.max_depth.unwrap_or(crawl::DEFAULT_MAX_DEPTH);
        let seconds = req.max_seconds.unwrap_or(crawl::DEFAULT_MAX_SECONDS).clamp(5, crawl::MAX_SECONDS_CEIL);
        let deadline = Instant::now() + Duration::from_secs(seconds);

        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        seen.insert(crawl::dedup_key(&start));
        queue.push_back((start, 0));
        let mut failed = 0usize;
        let mut stopped = None;

        while let Some((url, depth)) = queue.pop_front() {
            if report.pages_fetched >= max_pages {
                stopped = Some(format!("page budget of {max_pages} reached; more pages remain uncrawled"));
                break;
            }
            if Instant::now() >= deadline {
                stopped = Some(format!("time limit of {seconds}s reached; more pages remain uncrawled"));
                break;
            }
            report.pages_fetched += 1;
            if !self.visit(&url, deadline).await? {
                failed += 1;
                continue;
            }
            let Some(scan) = self.collect().await else { continue };
            // A client-side redirect lands somewhere else; that is visited now.
            seen.insert(crawl::dedup_key(&scan.location));
            self.absorb(&scan, depth, max_depth, report, &mut seen, &mut queue);

            if req.click {
                for c in scan.safe_clicks() {
                    if Instant::now() >= deadline {
                        break;
                    }
                    let clicked = self.eval(&format!("{}\n{}", crawl::CLICKABLES_JS, crawl::click_js(&c.key))).await.and_then(|v| v.as_bool()).unwrap_or(false);
                    if !clicked {
                        continue;
                    }
                    report.clicks += 1;
                    self.settle(CLICK_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())), false).await;
                    let Some(after) = self.collect().await else { continue };
                    self.absorb(&after, depth, max_depth, report, &mut seen, &mut queue);
                    // The click led to another page: note it (absorb queued it) and come back.
                    if crawl::dedup_key(&after.location) != crawl::dedup_key(&scan.location) && !self.visit(&scan.location, deadline).await? {
                        break;
                    }
                }
            }
        }
        report.urls_found = seen.len();
        if failed > 0 {
            report.notes.push(format!("{failed} page{} did not load", if failed == 1 { "" } else { "s" }));
        }
        report.notes.extend(stopped);
        let blocked = std::mem::take(&mut self.net.lock().unwrap().blocked);
        if !blocked.is_empty() {
            report.notes.push(format!(
                "requests to {} host{} outside scope were blocked; accept {} in Scope if the app needs {}",
                blocked.len(),
                if blocked.len() == 1 { "" } else { "s" },
                if blocked.len() == 1 { "it" } else { "them" },
                if blocked.len() == 1 { "it" } else { "them" },
            ));
        }
        report.blocked_hosts = blocked.into_iter().take(50).collect();
        Ok(())
    }

    /// Records a page's forms and queues its in-scope links.
    fn absorb(&self, scan: &PageScan, depth: usize, max_depth: usize, report: &mut CrawlReport, seen: &mut BTreeSet<String>, queue: &mut VecDeque<(String, usize)>) {
        for form in &scan.forms {
            report.add_form(form.clone());
        }
        let here = crawl::dedup_key(&scan.location);
        if crawl::follow(&self.rules, &scan.location, &scan.location).is_some() && seen.insert(here) {
            queue.push_back((scan.location.clone(), depth));
        }
        if depth >= max_depth {
            return;
        }
        for raw in &scan.links {
            if let Some(next) = crawl::follow(&self.rules, &scan.location, raw)
                && seen.insert(crawl::dedup_key(&next))
            {
                queue.push_back((next, depth + 1));
            }
        }
    }

    /// Loads `url` and waits for it to settle. False when it did not load.
    async fn visit(&self, url: &str, deadline: Instant) -> Result<bool> {
        {
            let mut n = self.net.lock().unwrap();
            n.loaded = false;
            n.last = Some(Instant::now());
        }
        let nav = match self.cdp.call(Some(&self.session), "Page.navigate", json!({ "url": url })).await {
            Ok(v) => v,
            // The browser itself went away: nothing more to crawl.
            Err(e) if e.to_string().contains("went away") => return Err(e),
            Err(_) => return Ok(false),
        };
        if nav.get("errorText").and_then(Value::as_str).is_some_and(|t| !t.is_empty()) {
            return Ok(false);
        }
        self.settle(PAGE_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())).max(Duration::from_secs(1)), true).await;
        Ok(true)
    }

    /// Waits until the network is idle (and, for a navigation, the page has
    /// loaded), or `max` passes.
    async fn settle(&self, max: Duration, wait_load: bool) {
        let began = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if began.elapsed() >= max {
                return;
            }
            let n = self.net.lock().unwrap();
            let quiet = n.last.is_none_or(|t| t.elapsed() >= QUIET) && began.elapsed() >= QUIET;
            let busy = n.inflight.values().any(|t| t.elapsed() < LONG_REQUEST);
            if quiet && !busy && (n.loaded || !wait_load) {
                return;
            }
        }
    }

    async fn collect(&self) -> Option<PageScan> {
        let v = self.eval(&format!("{}\n{}", crawl::CLICKABLES_JS, crawl::COLLECT_JS)).await?;
        serde_json::from_str(v.as_str()?).ok()
    }

    async fn eval(&self, expression: &str) -> Option<Value> {
        let r = self.cdp.call(Some(&self.session), "Runtime.evaluate", json!({ "expression": expression, "returnByValue": true })).await.ok()?;
        r.get("exceptionDetails").is_none().then(|| r["result"]["value"].clone())
    }
}

/// Answers the events that cannot wait, and tracks the page's network.
async fn pump(cdp: Cdp, mut events: tokio::sync::mpsc::UnboundedReceiver<Event>, rules: ScopeRules, main: String, net: Shared) {
    while let Some(ev) = events.recv().await {
        let s = ev.session.as_deref();
        let p = &ev.params;
        let on_main = s == Some(main.as_str());
        let sent = match ev.method.as_str() {
            "Fetch.requestPaused" => {
                let id = p["requestId"].clone();
                match crawl::blocked_host(&rules, p["request"]["url"].as_str().unwrap_or("")) {
                    Some(host) => {
                        net.lock().unwrap().blocked.insert(host);
                        cdp.send(s, "Fetch.failRequest", json!({ "requestId": id, "errorReason": "BlockedByClient" })).await
                    }
                    None => cdp.send(s, "Fetch.continueRequest", json!({ "requestId": id })).await,
                }
            }
            "Page.javascriptDialogOpening" => {
                // Dismiss: a "really delete?" confirm answers no. Leaving a page is fine.
                let accept = p["type"].as_str() == Some("beforeunload");
                cdp.send(s, "Page.handleJavaScriptDialog", json!({ "accept": accept })).await
            }
            "Target.attachedToTarget" => {
                let child = p["sessionId"].as_str().unwrap_or("").to_string();
                let c = Some(child.as_str());
                let _ = cdp.send(c, "Fetch.enable", json!({ "patterns": [{ "urlPattern": "*" }] })).await;
                let _ = cdp.send(c, "Target.setAutoAttach", json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true })).await;
                cdp.send(c, "Runtime.runIfWaitingForDebugger", json!({})).await
            }
            "Network.requestWillBeSent" if on_main => {
                let mut n = net.lock().unwrap();
                n.inflight.insert(p["requestId"].as_str().unwrap_or("").to_string(), Instant::now());
                n.last = Some(Instant::now());
                Ok(())
            }
            "Network.loadingFinished" | "Network.loadingFailed" if on_main => {
                let mut n = net.lock().unwrap();
                n.inflight.remove(p["requestId"].as_str().unwrap_or(""));
                n.last = Some(Instant::now());
                Ok(())
            }
            "Page.loadEventFired" if on_main => {
                net.lock().unwrap().loaded = true;
                Ok(())
            }
            _ => Ok(()),
        };
        if sent.is_err() {
            break;
        }
    }
}

/// A throwaway browser profile, removed when dropped.
struct TempProfile(PathBuf);

impl TempProfile {
    fn create() -> Result<TempProfile> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("plonix-crawl-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(TempProfile(dir))
    }
}

impl Drop for TempProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
