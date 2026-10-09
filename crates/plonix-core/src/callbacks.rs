//! Callbacks: hosts that tell you when something reached them.
//!
//! The Callbacks tab hands out unique hostnames to put in a request — a URL
//! parameter, a header, an XML entity — and lists every DNS lookup, HTTP
//! request or mail that later arrives for one of them, so a person can see
//! that a server made a call of its own (a fetch, a lookup, a webhook) that
//! never shows up in the response.
//!
//! Plonix drives `interactsh-client`, a tool the user installs, against a
//! public callback server or one they host. Nothing starts on its own: the
//! listener runs only after the user presses Start, and stops when they press
//! Stop or the project closes. The client registers once with one server and
//! prints a payload host; every host Plonix hands out is that host's
//! correlation id with a fresh suffix, so each test gets a name of its own and
//! each callback can be traced to the test it came from. The client's session
//! is kept per project, so hosts handed out earlier keep working after a
//! restart.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::detect::clean;
use crate::model::now_ms;
use crate::paths::{Home, write_private};
use crate::store::Store;

/// The executable Plonix drives.
pub const PROGRAM: &str = "interactsh-client";
/// interactsh has no Homebrew formula, so the client is built with Go. It
/// lands in ~/go/bin, which [`crate::program::locate_exe`] searches.
#[cfg(not(windows))]
pub const INSTALL: &str = "brew install go && go install github.com/projectdiscovery/interactsh/cmd/interactsh-client@latest";
/// On Windows, Go comes from winget; `go` is on the PATH of terminals opened after that.
#[cfg(windows)]
pub const INSTALL: &str = "winget install GoLang.Go, then in a new terminal: go install github.com/projectdiscovery/interactsh/cmd/interactsh-client@latest";
pub const HOMEPAGE: &str = "https://github.com/projectdiscovery/interactsh";
/// Where a project keeps its hosts and callbacks.
const STATE_VIEW: &str = "callbacks";
/// Callbacks kept per project; older ones are dropped first.
pub const MAX_INTERACTIONS: usize = 500;
/// Hosts kept per project.
pub const MAX_PAYLOADS: usize = 200;
/// Longest raw request or response kept from one callback.
const MAX_RAW: usize = 32 * 1024;
/// How long registering with the server may take before giving up.
const START_TIMEOUT: Duration = Duration::from_secs(45);
/// The client's defaults: a 20-character correlation id, then a 13-character
/// nonce. The server matches on the id, so any nonce reaches the same session.
const CID_LEN: usize = 20;
const NONCE_LEN: usize = 13;
/// The alphabet the client draws nonces from.
const NONCE_ALPHABET: &[u8] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// Where the client is installed, if it is.
pub fn locate() -> Option<PathBuf> {
    crate::program::locate_exe(PROGRAM)
}

// ---- configuration ----------------------------------------------------------------

/// Which server to use. Empty means the public servers the client knows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: String,
    pub token: String,
}

impl Config {
    fn path(home: &Home) -> PathBuf {
        home.root.join("callbacks.json")
    }

    pub fn load(home: &Home) -> Self {
        std::fs::read(Self::path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, home: &Home) -> Result<()> {
        write_private(&Self::path(home), &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// What the window may see: never the token itself.
    pub fn public(&self) -> Value {
        json!({ "server": self.server, "has_token": !self.token.is_empty() })
    }
}

/// Checks a server the user typed: a hostname, optionally with a scheme and
/// port, nothing else.
pub fn check_server(s: &str) -> Result<String, String> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return Ok(String::new());
    }
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let host = rest.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()).map(|(h, _)| h).unwrap_or(rest);
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        && !host.starts_with(['.', '-']);
    if ok { Ok(s.to_ascii_lowercase()) } else { Err(format!("`{}` is not a server address, e.g. oast.example.com", clean(s, 80))) }
}

// ---- state ------------------------------------------------------------------------

/// One host handed out for a test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    /// The first label of the host: correlation id and nonce. Callbacks are
    /// matched on it.
    pub id: String,
    pub host: String,
    /// What the user called it, e.g. the request it went into.
    #[serde(default)]
    pub label: String,
    pub created_at: i64,
}

/// One callback that reached the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    /// Increases with every callback, so the window asks only for new ones.
    pub seq: u64,
    /// `dns`, `http`, `https`, `smtp`, `ldap`, `ftp`, …
    pub protocol: String,
    /// The host's id it arrived for, when it matches one handed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// The name it arrived for, as the server saw it.
    pub full_id: String,
    pub remote: String,
    /// When the server received it, in milliseconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub q_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub smtp_from: String,
    #[serde(default)]
    pub raw_request: String,
    #[serde(default)]
    pub raw_response: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Stopped,
    Starting,
    Listening,
    Failed,
}

/// What a project keeps between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    /// The host the client printed: `<id><nonce>.<domain>`.
    base: String,
    payloads: Vec<Payload>,
    interactions: VecDeque<Interaction>,
    seq: u64,
}

struct Inner {
    saved: Saved,
    phase: Phase,
    error: Option<String>,
    child: Option<Child>,
    /// Bumped on every start, so output from an old client is ignored.
    run: u64,
    started: Option<Instant>,
    dirty: bool,
}

/// The project's callback listener. Cheap to clone.
#[derive(Clone)]
pub struct Callbacks {
    inner: Arc<Mutex<Inner>>,
}

impl Default for Callbacks {
    fn default() -> Self {
        Self { inner: Arc::new(Mutex::new(Inner { saved: Saved::default(), phase: Phase::Stopped, error: None, child: None, run: 0, started: None, dirty: false })) }
    }
}

impl Callbacks {
    /// The listener for a project, with the hosts and callbacks it kept.
    pub fn load(store: &Store) -> Self {
        let cb = Self::default();
        if let Ok(Some(v)) = store.view_state(STATE_VIEW)
            && let Ok(saved) = serde_json::from_value::<Saved>(v)
        {
            cb.inner.lock().unwrap().saved = saved;
        }
        cb
    }

    /// Writes what changed since the last call to the project.
    pub fn persist(&self, store: &Store) -> Result<()> {
        let saved = {
            let mut g = self.inner.lock().unwrap();
            if !g.dirty {
                return Ok(());
            }
            g.dirty = false;
            g.saved.clone()
        };
        store.set_view_state(STATE_VIEW, &serde_json::to_value(saved)?)
    }

    pub fn phase(&self) -> Phase {
        self.inner.lock().unwrap().phase
    }

    /// The newest callback's number, so the sidebar can count unseen ones.
    pub fn latest(&self) -> u64 {
        self.inner.lock().unwrap().saved.seq
    }

    /// Everything the tab shows. `since` leaves out callbacks the window has.
    pub fn snapshot(&self, since: u64) -> Value {
        let mut g = self.inner.lock().unwrap();
        // A client that never printed a host in time is given up on.
        if g.phase == Phase::Starting && g.started.is_some_and(|t| t.elapsed() > START_TIMEOUT) {
            stop_child(&mut g);
            g.phase = Phase::Failed;
            g.error = Some("The callback server did not answer. Check the server address, or try again.".into());
        }
        let cid = split_base(&g.saved.base).map(|(c, _)| c);
        let mut counts = std::collections::HashMap::<&str, u64>::new();
        for i in &g.saved.interactions {
            if let Some(p) = &i.payload {
                *counts.entry(p.as_str()).or_default() += 1;
            }
        }
        let payloads: Vec<Value> = g
            .saved
            .payloads
            .iter()
            .rev()
            .map(|p| {
                let mut v = serde_json::to_value(p).unwrap_or_default();
                v["hits"] = json!(counts.get(p.id.as_str()).copied().unwrap_or(0));
                // A host from another server's session no longer reaches this project.
                v["live"] = json!(cid.is_some_and(|c| p.id.starts_with(c)));
                v
            })
            .collect();
        let interactions: Vec<&Interaction> = g.saved.interactions.iter().filter(|i| i.seq > since).collect();
        json!({
            "phase": g.phase,
            "error": g.error,
            "base": g.saved.base,
            "payloads": payloads,
            "interactions": interactions,
            "seq": g.saved.seq,
            "total": g.saved.interactions.len(),
        })
    }

    /// Starts the client. `session` is where it keeps its keys between runs,
    /// so hosts handed out earlier keep reaching this project.
    pub fn start(&self, exe: &Path, config: &Config, session: &Path) -> Result<(), String> {
        let mut g = self.inner.lock().unwrap();
        if matches!(g.phase, Phase::Starting | Phase::Listening) && g.child.is_some() {
            return Ok(());
        }
        if let Some(dir) = session.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        // Each server (and token) has a session of its own, so switching back
        // to a server resumes it and the hosts handed out there.
        let key = crate::rulepack::sha256_hex(format!("{}\n{}", config.server, config.token).as_bytes());
        let stem = session.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let session = session.with_file_name(format!("{stem}-{}.yaml", &key[..12]));
        let mut args: Vec<String> = ["-json", "-duc", "-n", "1", "-pi", "5", "-sf"].map(String::from).to_vec();
        args.push(session.to_string_lossy().into_owned());
        if !config.server.is_empty() {
            args.extend(["-s".into(), config.server.clone()]);
        }
        if !config.token.is_empty() {
            args.extend(["-t".into(), config.token.clone()]);
        }
        let mut child = Command::new(exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start {PROGRAM}: {e}"))?;
        g.run += 1;
        let run = g.run;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        g.child = Some(child);
        g.phase = Phase::Starting;
        g.error = None;
        g.started = Some(Instant::now());
        drop(g);

        let me = self.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                me.on_stdout(run, &line);
            }
            me.on_exit(run);
        });
        let me = self.clone();
        std::thread::spawn(move || {
            let mut last_problem = String::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let line = strip_ansi(&line);
                if let Some(host) = payload_host_in(&line) {
                    me.on_base(run, &host);
                } else if line.contains("[FTL]") || line.contains("[ERR]") {
                    last_problem = line;
                }
            }
            if !last_problem.is_empty() {
                me.on_problem(run, &last_problem);
            }
        });
        Ok(())
    }

    /// Stops the client. Its session is saved, so the hosts keep working the
    /// next time it starts.
    pub fn stop(&self) {
        let mut g = self.inner.lock().unwrap();
        g.run += 1;
        stop_child(&mut g);
        g.phase = Phase::Stopped;
        g.error = None;
    }

    /// Hands out a new host for one test.
    pub fn new_payload(&self, label: &str) -> Result<Payload, String> {
        let mut g = self.inner.lock().unwrap();
        let Some((cid, domain)) = split_base(&g.saved.base) else {
            return Err("Start listening first: the server gives out the hosts.".into());
        };
        let id = format!("{cid}{}", nonce());
        let p = Payload { host: format!("{id}.{domain}"), id, label: clean(label.trim(), 120), created_at: now_ms() };
        g.saved.payloads.push(p.clone());
        let excess = g.saved.payloads.len().saturating_sub(MAX_PAYLOADS);
        g.saved.payloads.drain(..excess);
        g.dirty = true;
        Ok(p)
    }

    pub fn rename_payload(&self, id: &str, label: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(p) = g.saved.payloads.iter_mut().find(|p| p.id == id) else { return false };
        p.label = clean(label.trim(), 120);
        g.dirty = true;
        true
    }

    pub fn remove_payload(&self, id: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        let before = g.saved.payloads.len();
        g.saved.payloads.retain(|p| p.id != id);
        let removed = g.saved.payloads.len() != before;
        g.dirty |= removed;
        removed
    }

    /// Forgets every callback (the hosts stay).
    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap();
        g.saved.interactions.clear();
        g.dirty = true;
    }

    fn on_base(&self, run: u64, host: &str) {
        let mut g = self.inner.lock().unwrap();
        if g.run != run {
            return;
        }
        // A resumed session prints a host with the same id; keep the one the
        // project knows so earlier hosts and the new one agree.
        let same = split_base(&g.saved.base).zip(split_base(host)).is_some_and(|(a, b)| a == b);
        if !same {
            g.saved.base = host.to_string();
            g.dirty = true;
        }
        g.phase = Phase::Listening;
        g.error = None;
    }

    fn on_stdout(&self, run: u64, line: &str) {
        let line = line.trim();
        if !line.starts_with('{') {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let mut g = self.inner.lock().unwrap();
        if g.run != run {
            return;
        }
        let seq = g.saved.seq + 1;
        let Some(i) = parse_interaction(&v, seq, &g.saved.payloads) else { return };
        g.saved.seq = seq;
        g.saved.interactions.push_back(i);
        while g.saved.interactions.len() > MAX_INTERACTIONS {
            g.saved.interactions.pop_front();
        }
        g.dirty = true;
    }

    fn on_problem(&self, run: u64, line: &str) {
        let mut g = self.inner.lock().unwrap();
        if g.run == run {
            g.error = Some(explain(line));
        }
    }

    fn on_exit(&self, run: u64) {
        let mut g = self.inner.lock().unwrap();
        if g.run != run {
            return;
        }
        if let Some(mut c) = g.child.take() {
            let _ = c.wait();
        }
        g.phase = Phase::Failed;
        if g.error.is_none() {
            g.error = Some(format!("{PROGRAM} stopped unexpectedly."));
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        stop_child(self);
    }
}

/// Asks the client to stop (an interrupt, so it saves its session), and
/// makes sure it does.
fn stop_child(g: &mut Inner) {
    let Some(mut child) = g.child.take() else { return };
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-INT", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Where a project's client sessions are kept (one per server, see
/// [`Callbacks::start`]).
pub fn session_path(home: &Home, project: &str) -> PathBuf {
    let safe: String = project.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' }).take(80).collect();
    home.root.join("callbacks").join(format!("{}.yaml", if safe.is_empty() { "default" } else { &safe }))
}

/// `<id><nonce>.<domain>` → (`<id>`, `<domain>`).
fn split_base(host: &str) -> Option<(&str, &str)> {
    let (label, domain) = host.split_once('.')?;
    (label.len() >= CID_LEN && label.is_ascii() && !domain.is_empty()).then(|| (&label[..CID_LEN], domain))
}

fn nonce() -> String {
    let mut b = [0u8; NONCE_LEN];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    b.iter().map(|x| NONCE_ALPHABET[(*x as usize) % NONCE_ALPHABET.len()] as char).collect()
}

/// The payload host in a line the client logs: `[INF] <id><nonce>.<domain>`.
fn payload_host_in(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("[INF]")?.trim();
    if rest.contains(' ') {
        return None;
    }
    let host = rest.trim_end_matches('/').to_ascii_lowercase();
    let host = host.strip_prefix("https://").or_else(|| host.strip_prefix("http://")).unwrap_or(&host).to_string();
    let (label, _) = host.split_once('.')?;
    (label.len() >= CID_LEN + NONCE_LEN && label.bytes().all(|b| b.is_ascii_alphanumeric()) && split_base(&host).is_some()).then_some(host)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A client error in words a person can act on.
fn explain(line: &str) -> String {
    let l = line.to_ascii_lowercase();
    if l.contains("could not register") {
        return "Could not reach the callback server. Check your connection and the server address, then start again.".into();
    }
    if l.contains("token") && (l.contains("invalid") || l.contains("unauthorized") || l.contains("401")) {
        return "The server refused the token. Check it in Server settings.".into();
    }
    let msg = line.split_once(']').map(|(_, m)| m.trim()).unwrap_or(line);
    clean(msg, 300)
}

fn cap(s: &str) -> String {
    if s.len() <= MAX_RAW {
        return s.to_string();
    }
    let mut end = MAX_RAW;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… cut at {} KB", &s[..end], MAX_RAW / 1024)
}

/// Reads one line of the client's JSON output.
fn parse_interaction(v: &Value, seq: u64, payloads: &[Payload]) -> Option<Interaction> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let protocol = s("protocol").to_ascii_lowercase();
    if protocol.is_empty() {
        return None;
    }
    let full_id = s("full-id").to_ascii_lowercase();
    let raw_request = cap(&s("raw-request"));
    let lower_req = raw_request.to_ascii_lowercase();
    let payload = payloads.iter().find(|p| full_id.contains(&p.id) || lower_req.contains(&p.id)).map(|p| p.id.clone());
    let at = crate::har::parse_iso_time(&s("timestamp")).unwrap_or_else(now_ms);
    Some(Interaction {
        seq,
        protocol: clean(&protocol, 20),
        payload,
        full_id: clean(&full_id, 300),
        remote: clean(&s("remote-address"), 100),
        at,
        q_type: clean(&s("q-type"), 20),
        smtp_from: clean(&s("smtp-from"), 300),
        raw_request,
        raw_response: cap(&s("raw-response")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "d3h8kq0vtc0c73a0g5fgyyyyyyyyyyyyy.oast.pro";

    #[test]
    fn reads_the_payload_host_the_client_logs() {
        assert_eq!(payload_host_in("[INF] d3h8kq0vtc0c73a0g5fgyyyyyyyyyyyyy.oast.pro").as_deref(), Some(BASE));
        assert_eq!(payload_host_in(&strip_ansi("[\u{1b}[34mINF\u{1b}[0m] d3h8kq0vtc0c73a0g5fgyyyyyyyyyyyyy.oast.pro")).as_deref(), Some(BASE));
        assert_eq!(payload_host_in("[INF] Listing 1 payload for OOB Testing"), None);
        assert_eq!(payload_host_in("[INF] Current interactsh version v1.4.1 (latest)"), None);
        assert_eq!(payload_host_in("[INF] short.oast.pro"), None);
    }

    #[test]
    fn hands_out_hosts_with_the_sessions_id_and_matches_callbacks_to_them() {
        let cb = Callbacks::default();
        assert!(cb.new_payload("x").is_err(), "no host before the server gives one");
        cb.on_base(0, BASE);
        assert_eq!(cb.phase(), Phase::Listening);
        let a = cb.new_payload("POST /api/import url").unwrap();
        let b = cb.new_payload("").unwrap();
        assert_ne!(a.host, b.host);
        assert!(a.host.starts_with("d3h8kq0vtc0c73a0g5fg") && a.host.ends_with(".oast.pro"));
        assert_eq!(a.id.len(), CID_LEN + NONCE_LEN);

        let line = json!({ "protocol": "dns", "unique-id": "d3h8kq0vtc0c73a0g5fg", "full-id": format!("deep.{}", a.id.to_uppercase()), "q-type": "A", "raw-request": ";; QUESTION", "remote-address": "203.0.113.9", "timestamp": "2026-10-08T09:00:00.5Z" });
        cb.on_stdout(0, &line.to_string());
        let http = json!({ "protocol": "http", "unique-id": "d3h8kq0vtc0c73a0g5fg", "full-id": "d3h8kq0vtc0c73a0g5fg", "raw-request": format!("GET / HTTP/1.1\r\nHost: {}\r\n\r\n", b.host), "remote-address": "198.51.100.4", "timestamp": "2026-10-08T09:00:01Z" });
        cb.on_stdout(0, &http.to_string());
        cb.on_stdout(0, "not json");
        cb.on_stdout(7, &line.to_string()); // an old client's output

        let snap = cb.snapshot(0);
        let list = snap["interactions"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["payload"], json!(a.id));
        assert_eq!(list[0]["at"].as_i64().unwrap() % 1000, 500);
        assert_eq!(list[1]["payload"], json!(b.id));
        assert_eq!(cb.snapshot(1)["interactions"].as_array().unwrap().len(), 1);
        let hits: Vec<u64> = snap["payloads"].as_array().unwrap().iter().map(|p| p["hits"].as_u64().unwrap()).collect();
        assert_eq!(hits, [1, 1]);
        assert!(snap["payloads"].as_array().unwrap().iter().all(|p| p["live"] == json!(true)));
        // Another server's session: earlier hosts are kept but no longer live.
        cb.on_base(0, "c0ffee0000000000000aaaaaaaaaaaaaa.oast.example");
        assert!(cb.snapshot(0)["payloads"].as_array().unwrap().iter().all(|p| p["live"] == json!(false)));
    }

    #[test]
    fn keeps_hosts_and_callbacks_in_the_project() {
        let store = Store::open_in_memory().unwrap();
        let cb = Callbacks::load(&store);
        cb.on_base(0, BASE);
        let p = cb.new_payload("kept").unwrap();
        cb.on_stdout(0, &json!({ "protocol": "http", "full-id": p.id, "remote-address": "x" }).to_string());
        cb.persist(&store).unwrap();
        let again = Callbacks::load(&store);
        let snap = again.snapshot(0);
        assert_eq!(snap["payloads"][0]["label"], json!("kept"));
        assert_eq!(snap["interactions"].as_array().unwrap().len(), 1);
        assert_eq!(snap["phase"], json!("stopped"));
        // A resumed session with the same id does not change the base.
        let run = again.inner.lock().unwrap().run;
        again.on_base(run, "d3h8kq0vtc0c73a0g5fgzzzzzzzzzzzzz.oast.pro");
        assert_eq!(again.snapshot(0)["base"], json!(BASE));
    }

    #[test]
    fn checks_server_addresses() {
        assert_eq!(check_server(" oast.example.com/ ").unwrap(), "oast.example.com");
        assert_eq!(check_server("https://OAST.example.com:8443").unwrap(), "https://oast.example.com:8443");
        assert_eq!(check_server("").unwrap(), "");
        assert!(check_server("oast.example.com; rm -rf").is_err());
        assert!(check_server("-s").is_err());
        assert!(check_server("localhost").is_err());
    }

    #[test]
    fn explains_registration_failures() {
        assert!(explain("[FTL] Could not create client: could not register to servers").starts_with("Could not reach"));
        assert_eq!(explain("[ERR] something odd"), "something odd");
    }
}
