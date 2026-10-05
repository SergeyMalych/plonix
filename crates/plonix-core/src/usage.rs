//! Anonymous usage statistics: how often features are used, nothing else.
//!
//! Features call [`record`] with a name from [`EVENTS`]; nothing but those
//! fixed names is ever counted. Counts are kept in `$PLONIX_HOME/usage.json`
//! and, at most once a day, sent to [`ENDPOINT`] together with a random
//! install id, the Plonix version, the OS and the CPU type. docs/privacy.md
//! lists exactly what a report holds.
//!
//! Nothing is counted or sent unless all of these hold:
//! - the terms were accepted in this home (not just bypassed for a script);
//! - Settings › Usage statistics is on;
//! - neither `PLONIX_NO_ANALYTICS` nor `DO_NOT_TRACK` is set.
//!
//! On top of that, debug builds (tests, `cargo run`) and CI never send.
//! Turning statistics off deletes `usage.json`, install id included.
//! Sending happens on a thread of its own; a failure is silent and the
//! counts wait for the next day.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::paths::{Home, write_atomic};
use crate::settings::{self, Field, Level, Section};

/// Where reports go.
pub const ENDPOINT: &str = "https://plonix.io/api/usage";

/// The settings section with the on/off switch.
pub const SECTION: &str = "usage";

/// Everything that can be counted. The website's endpoint accepts these
/// names only (functions/api/usage.js); keep both lists in step.
pub const EVENTS: &[&str] = &[
    "app_launched",
    "web_launcher_opened",
    "cli_used",
    "project_opened",
    "demo_opened",
    "capture_started",
    "intercept_used",
    "bench_send",
    "bench_run",
    "scan_run",
    "crawl_run",
    "finding_added",
    "report_exported",
    "market_install",
    "ask_claude",
    "agent_launch",
    "mcp_session",
    "screen_traffic",
    "screen_bench",
    "screen_scope",
    "screen_map",
    "screen_findings",
    "screen_agents",
    "screen_market",
    "screen_scans",
    "screen_settings",
];

/// Version of the report's shape.
const SCHEMA: u32 = 1;
/// At most one report per this many seconds.
const PERIOD_S: i64 = 24 * 3600;
/// How often a long-running process writes its counts to disk.
const FLUSH_EVERY: Duration = Duration::from_secs(60);

struct State {
    home: Option<Home>,
    /// Whether this process may send reports (long-running processes only).
    sender: bool,
    pending: BTreeMap<&'static str, u64>,
    ticking: bool,
}

static STATE: Mutex<State> = Mutex::new(State { home: None, sender: false, pending: BTreeMap::new(), ticking: false });
/// One flush at a time in this process.
static FLUSHING: Mutex<()> = Mutex::new(());

/// What `usage.json` holds.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    install_id: String,
    /// When counting started (unix seconds), for the first report.
    #[serde(default)]
    started_at: i64,
    #[serde(default)]
    last_sent: i64,
    #[serde(default)]
    counts: BTreeMap<String, u64>,
}

fn path(home: &Home) -> std::path::PathBuf {
    home.root.join("usage.json")
}

fn load(home: &Home) -> File {
    std::fs::read(path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn now() -> i64 {
    crate::model::now_ms() / 1000
}

/// Sets where counts are kept for this process. The first call wins. A
/// `sender` (the app, the Start screen, an engine) writes counts every minute
/// and sends the daily report from that timer; short CLI commands only call
/// [`flush`] before they exit.
pub fn init(home: &Home, sender: bool) {
    let mut s = STATE.lock().unwrap();
    if s.home.is_none() {
        s.home = Some(home.clone());
        s.sender = sender;
    }
    if s.sender {
        tick(&mut s);
    }
}

fn tick(s: &mut State) {
    if s.ticking || s.home.is_none() {
        return;
    }
    s.ticking = std::thread::Builder::new()
        .name("plonix-usage".into())
        .spawn(|| {
            loop {
                std::thread::sleep(FLUSH_EVERY);
                save(true);
            }
        })
        .is_ok();
}

/// Counts one use of a feature. Names not in [`EVENTS`] are ignored.
pub fn record(event: &str) {
    let Some(e) = EVENTS.iter().find(|e| **e == event) else { return };
    let mut s = STATE.lock().unwrap();
    *s.pending.entry(e).or_default() += 1;
    if s.sender {
        tick(&mut s);
    }
}

/// Set in the environment to opt out, whatever Settings say.
pub fn disabled_by_env() -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no"));
    set("PLONIX_NO_ANALYTICS") || set("DO_NOT_TRACK")
}

/// Whether statistics are collected in this home.
pub fn sharing(home: &Home) -> bool {
    !disabled_by_env() && crate::terms::accepted(home) && settings::global(home, SECTION).get("share").and_then(Value::as_bool).unwrap_or(true)
}

/// Whether this build, in this environment, may send at all.
fn may_send() -> bool {
    !cfg!(debug_assertions) && std::env::var_os("CI").is_none()
}

/// Turns statistics on or off (the first-launch screen and `plonix usage`).
pub fn set_sharing(home: &Home, on: bool) -> Result<()> {
    let Some(sec) = settings::section(SECTION) else { return Ok(()) };
    let values = sec.check(&json!({ "share": on }), &settings::global(home, SECTION)).map_err(|_| anyhow::anyhow!("bad usage setting"))?;
    settings::save_global(home, SECTION, &values)?;
    if !on {
        let _ = std::fs::remove_file(path(home));
    }
    Ok(())
}

/// Writes this process's counts to disk. Call before a process exits.
pub fn flush() {
    save(false);
}

/// Writes the counts and, from the minute ticker of a long-running process,
/// sends the report when one is due.
fn save(may_report: bool) {
    let _one = FLUSHING.lock().unwrap();
    let (home, sender, pending) = {
        let mut s = STATE.lock().unwrap();
        (s.home.clone(), s.sender && may_report, std::mem::take(&mut s.pending))
    };
    let Some(home) = home else { return };
    if !sharing(&home) {
        // Off: keep nothing, not even the install id.
        if path(&home).exists() {
            let _ = std::fs::remove_file(path(&home));
        }
        return;
    }
    let mut f = load(&home);
    let mut changed = !pending.is_empty();
    if f.install_id.len() != 32 {
        let Some(id) = random_id() else { return };
        f.install_id = id;
        f.started_at = now();
        changed = true;
    }
    for (k, n) in pending {
        *f.counts.entry(k.to_string()).or_default() += n;
    }
    let since = if f.last_sent > 0 { f.last_sent } else { f.started_at };
    let mut report = None;
    if sender && may_send() && !f.counts.is_empty() && now() - since >= PERIOD_S {
        report = Some(payload(&f.install_id, &f.counts));
        f.last_sent = now();
        f.counts.clear();
        changed = true;
    }
    if changed && write_atomic(&path(&home), &serde_json::to_vec_pretty(&f).unwrap_or_default()).is_err() {
        return;
    }
    if let Some(report) = report {
        let _ = std::thread::Builder::new().name("plonix-usage-send".into()).spawn(move || {
            if send(&report).is_err() {
                // Keep the counts for the next report.
                let _one = FLUSHING.lock().unwrap();
                let mut f = load(&home);
                if let Some(counts) = report["counts"].as_object() {
                    for (k, n) in counts {
                        *f.counts.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
                    }
                }
                let _ = write_atomic(&path(&home), &serde_json::to_vec_pretty(&f).unwrap_or_default());
            }
        });
    }
}

fn send(report: &Value) -> Result<()> {
    ureq::post(ENDPOINT)
        .timeout(Duration::from_secs(10))
        .set("User-Agent", concat!("plonix/", env!("CARGO_PKG_VERSION")))
        .send_json(report.clone())?;
    Ok(())
}

fn random_id() -> Option<String> {
    let mut buf = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// A report: exactly the fields docs/privacy.md lists.
fn payload(install_id: &str, counts: &BTreeMap<String, u64>) -> Value {
    json!({
        "schema": SCHEMA,
        "install_id": install_id,
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "os_version": os_version(),
        "arch": std::env::consts::ARCH,
        "counts": counts,
    })
}

/// The OS release, such as 15.1 on macOS. Only digits, letters and dots.
fn os_version() -> String {
    let raw = if cfg!(target_os = "macos") {
        std::process::Command::new("sw_vers").arg("-productVersion").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    } else {
        std::fs::read_to_string("/etc/os-release")
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("VERSION_ID="))
            .unwrap_or("")
            .trim_matches('"')
            .to_string()
    };
    raw.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').take(24).collect()
}

/// Whether statistics are on, and the report as it would be sent now.
pub fn preview(home: &Home) -> Value {
    let f = load(home);
    let id = if f.install_id.is_empty() { "(made when the first count is saved)".to_string() } else { f.install_id.clone() };
    json!({
        "sharing": sharing(home),
        "disabled_by_env": disabled_by_env(),
        "terms_accepted": crate::terms::accepted(home),
        "endpoint": ENDPOINT,
        "last_sent": f.last_sent,
        "next_report": payload(&id, &f.counts),
    })
}

pub fn settings_section() -> Section {
    Section::new(SECTION, "Usage statistics", Level::Global)
        .describe(
            "Helps decide what to improve. Anonymous and at most once a day: a random install id, the Plonix version, your OS and CPU type, \
             and how often features were used. Never URLs, hosts, traffic, project names, paths or anything you type.",
        )
        .order(90)
        .field(Field::toggle("share", "Share anonymous usage statistics", true).help(
            "PLONIX_NO_ANALYTICS=1 or DO_NOT_TRACK=1 turns it off too. `plonix usage` shows the next report as it would be sent.",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_hold_only_the_listed_fields() {
        let mut counts = BTreeMap::new();
        counts.insert("bench_send".to_string(), 3);
        let p = payload("0123456789abcdef0123456789abcdef", &counts);
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["arch", "counts", "install_id", "os", "os_version", "schema", "version"]);
        assert!(p["os_version"].as_str().unwrap().chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
    }

    #[test]
    fn the_endpoint_accepts_every_event() {
        let function = include_str!("../../../functions/api/usage.js");
        for e in EVENTS {
            assert!(function.contains(&format!("'{e}'")), "functions/api/usage.js does not accept {e}");
            assert!(e.len() <= 40 && e.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }

    #[test]
    fn nothing_is_kept_without_consent() {
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        assert!(!sharing(&home), "terms not accepted");
        crate::terms::accept(&home).unwrap();
        assert_eq!(sharing(&home), !disabled_by_env(), "on by default once accepted");
        set_sharing(&home, false).unwrap();
        assert!(!sharing(&home));
        assert!(!path(&home).exists());
        assert!(!may_send(), "test builds never send");
    }
}
