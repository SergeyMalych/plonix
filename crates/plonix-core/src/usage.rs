//! Anonymous usage statistics: counts, ranges and fixed names, nothing else.
//!
//! Features call [`record`] with a name from [`EVENTS`]; nothing but those
//! fixed names is ever counted. The project window adds active minutes per
//! screen ([`record_minute`]), Traffic searches add the kinds of their terms
//! ([`record_filters`], never the text), and an open project leaves a
//! [`snapshot_project`]: its sizes as ranges, and which of the well-known
//! third-party domains ([`known_domain`]) it rejected. Everything is kept in
//! `$PLONIX_HOME/usage.json` and, at most once a day, sent to [`ENDPOINT`]
//! with a random install id, the Plonix version, the OS and the CPU type.
//! docs/privacy.md lists exactly what a report holds; plonix.io/analytics
//! shows the totals.
//!
//! Nothing is counted or sent unless all of these hold:
//! - the terms were accepted in this home (not just bypassed for a script);
//! - Settings › Usage statistics is on (it is off until the user ticks it);
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
    "program_applied",
    "crawl_run",
    "finding_added",
    "report_exported",
    "market_install",
    "ask_claude",
    "agent_launch",
    "mcp_session",
    "callbacks_start",
    "access_check",
    "screen_traffic",
    "screen_bench",
    "screen_scope",
    "screen_map",
    "screen_findings",
    "screen_agents",
    "screen_market",
    "screen_scans",
    "screen_programs",
    "screen_settings",
    "screen_users",
    "screen_access",
    "screen_callbacks",
    "screen_rules",
];

/// Screens whose active minutes are counted.
pub const SCREENS: &[&str] = &["traffic", "bench", "scope", "map", "users", "access", "callbacks", "findings", "agents", "market", "scans", "programs", "rules", "settings"];

/// Kinds of Traffic search terms; a leading `-` marks one that hides.
pub const FILTER_KINDS: &[&str] = &["host", "method", "status", "path", "mime", "scope", "source", "ext", "kind", "is", "text"];

/// Ranges every size is reported in, never the number itself.
pub const RANGES: &[&str] = &["0", "1", "2-5", "6-20", "21-100", "101-1k", "1k-10k", "10k-100k", "100k+"];

/// What a rejected host outside the well-known list is reported as.
pub const OTHER: &str = "other";

/// Version of the report's shape.
const SCHEMA: u32 = 2;
/// At most one report per this many seconds.
const PERIOD_S: i64 = 24 * 3600;
/// How often a long-running process writes its counts to disk.
const FLUSH_EVERY: Duration = Duration::from_secs(60);

struct State {
    home: Option<Home>,
    /// Whether this process may send reports (long-running processes only).
    sender: bool,
    pending: BTreeMap<&'static str, u64>,
    minutes: BTreeMap<&'static str, u64>,
    filters: BTreeMap<String, u64>,
    /// The minute (unix minutes) last counted, so two windows never count one minute twice.
    last_minute: i64,
    /// The kinds of the last search counted, so a list that refreshes is one search.
    last_filter: String,
    ticking: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    home: None,
    sender: false,
    pending: BTreeMap::new(),
    minutes: BTreeMap::new(),
    filters: BTreeMap::new(),
    last_minute: 0,
    last_filter: String::new(),
    ticking: false,
});
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
    /// Screen -> active minutes.
    #[serde(default)]
    minutes: BTreeMap<String, u64>,
    /// Search term kind -> searches.
    #[serde(default)]
    filters: BTreeMap<String, u64>,
    /// Projects used since the last report, by their local id (never sent).
    #[serde(default)]
    projects: BTreeMap<String, Snapshot>,
}

/// One project as reported: ranges, and which well-known domains it rejected.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub requests: String,
    pub hosts: String,
    pub in_scope: String,
    pub rejected: std::collections::BTreeSet<String>,
}

/// The range a count is reported as.
pub fn range(n: u64) -> &'static str {
    match n {
        0 => RANGES[0],
        1 => RANGES[1],
        2..=5 => RANGES[2],
        6..=20 => RANGES[3],
        21..=100 => RANGES[4],
        101..=1000 => RANGES[5],
        1001..=10_000 => RANGES[6],
        10_001..=100_000 => RANGES[7],
        _ => RANGES[8],
    }
}

/// Every domain a report may name: the built-in exclusion groups and the
/// scope noise list. Anything else is [`OTHER`].
pub fn known_domains() -> &'static [String] {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        let mut v: Vec<String> = crate::exclude::builtin_groups().into_iter().flat_map(|g| g.domains).chain(crate::scope::NOISE.iter().map(|d| d.to_string())).collect();
        v.sort();
        v.dedup();
        v
    })
}

/// The well-known domain a host belongs to, or [`OTHER`].
pub fn known_domain(host: &str) -> &'static str {
    let host = crate::scope::normalize_host(host.trim_start_matches("*."));
    known_domains()
        .iter()
        .filter(|d| host == **d || crate::scope::is_subdomain_of(&host, d))
        .max_by_key(|d| d.len())
        .map(String::as_str)
        .unwrap_or(OTHER)
}

/// What an open project looks like in a report. Exact numbers and host
/// names stay here; only ranges and well-known domains come out.
pub fn project_snapshot(requests: u64, hosts: &[String], rules: &crate::scope::ScopeRules) -> Snapshot {
    let in_scope = hosts.iter().filter(|h| rules.in_scope(h)).count() as u64;
    let rejected = rules.rules.iter().filter(|r| r.decision == crate::scope::Decision::Rejected).map(|r| known_domain(&r.pattern).to_string()).collect();
    Snapshot { requests: range(requests).into(), hosts: range(hosts.len() as u64).into(), in_scope: range(in_scope).into(), rejected }
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

/// Counts one active minute on a screen of the project window. At most one
/// minute is counted per clock minute, whichever window sends it.
pub fn record_minute(screen: &str) {
    let Some(sc) = SCREENS.iter().find(|s| **s == screen) else { return };
    let minute = now() / 60;
    let mut s = STATE.lock().unwrap();
    if s.last_minute == minute {
        return;
    }
    s.last_minute = minute;
    *s.minutes.entry(sc).or_default() += 1;
}

/// The kinds of a search's terms, such as `host` or `-status`. Never a value.
pub fn filter_kinds(query: &str) -> Vec<String> {
    let mut kinds: Vec<String> = query
        .split_whitespace()
        .filter(|t| !matches!(*t, "-" | "\"" | "-\""))
        .map(|t| {
            let (neg, rest) = match t.strip_prefix('-') {
                Some(r) => ("-", r),
                None => ("", t),
            };
            let kind = match rest.split_once(':') {
                Some((k, v)) if !v.is_empty() && FILTER_KINDS.contains(&k.to_ascii_lowercase().as_str()) => k.to_ascii_lowercase(),
                _ => "text".to_string(),
            };
            format!("{neg}{kind}")
        })
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// Counts one Traffic search by the kinds of its terms. The same kinds again
/// (a list refreshing, a value being typed) count once.
pub fn record_filters(query: &str) {
    let kinds = filter_kinds(query);
    let key = kinds.join(" ");
    let mut s = STATE.lock().unwrap();
    if s.last_filter == key {
        return;
    }
    s.last_filter = key;
    for k in kinds {
        *s.filters.entry(k).or_default() += 1;
    }
}

/// Keeps what an open project looks like for the next report (see
/// [`project_snapshot`]). `id` is the project's local id and is never sent.
pub fn snapshot_project(id: &str, snapshot: Snapshot) {
    let home = STATE.lock().unwrap().home.clone();
    let Some(home) = home else { return };
    let _one = FLUSHING.lock().unwrap();
    if !sharing(&home) {
        return;
    }
    let mut f = load(&home);
    if f.projects.get(id) == Some(&snapshot) || (f.projects.len() >= 100 && !f.projects.contains_key(id)) {
        return;
    }
    f.projects.insert(id.to_string(), snapshot);
    let _ = write_atomic(&path(&home), &serde_json::to_vec_pretty(&f).unwrap_or_default());
}

/// Starts over with a new install id: everything kept so far is deleted.
pub fn reset(home: &Home) -> Result<()> {
    let _one = FLUSHING.lock().unwrap();
    match std::fs::remove_file(path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Set in the environment to opt out, whatever Settings say.
pub fn disabled_by_env() -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no"));
    set("PLONIX_NO_ANALYTICS") || set("DO_NOT_TRACK")
}

/// Whether statistics are collected in this home.
pub fn sharing(home: &Home) -> bool {
    // An unreadable settings file may hold "off": do not guess "on".
    !disabled_by_env() && crate::terms::accepted(home) && !settings::unreadable(home) && settings::global(home, SECTION).get("share").and_then(Value::as_bool).unwrap_or(false)
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
    let (home, sender, pending, minutes, filters) = {
        let mut s = STATE.lock().unwrap();
        (s.home.clone(), s.sender && may_report, std::mem::take(&mut s.pending), std::mem::take(&mut s.minutes), std::mem::take(&mut s.filters))
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
    let mut changed = !pending.is_empty() || !minutes.is_empty() || !filters.is_empty();
    if f.install_id.len() != 32 {
        let Some(id) = random_id() else { return };
        f.install_id = id;
        f.started_at = now();
        changed = true;
    }
    for (k, n) in pending {
        *f.counts.entry(k.to_string()).or_default() += n;
    }
    for (k, n) in minutes {
        *f.minutes.entry(k.to_string()).or_default() += n;
    }
    for (k, n) in filters {
        *f.filters.entry(k).or_default() += n;
    }
    let since = if f.last_sent > 0 { f.last_sent } else { f.started_at };
    let mut report = None;
    if sender && may_send() && (!f.counts.is_empty() || !f.minutes.is_empty()) && now() - since >= PERIOD_S {
        report = Some(payload(&home, &f));
        f.last_sent = now();
        f.counts.clear();
        f.minutes.clear();
        f.filters.clear();
        f.projects.clear();
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
                for (field, into) in [("counts", &mut f.counts), ("minutes", &mut f.minutes), ("filters", &mut f.filters)] {
                    for (k, n) in report[field].as_object().into_iter().flatten() {
                        *into.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
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
fn payload(home: &Home, f: &File) -> Value {
    let mut sizes: BTreeMap<&str, BTreeMap<String, u64>> = BTreeMap::new();
    let mut rejected: BTreeMap<String, u64> = BTreeMap::new();
    for p in f.projects.values() {
        for (k, v) in [("requests", &p.requests), ("hosts", &p.hosts), ("in_scope", &p.in_scope)] {
            if RANGES.contains(&v.as_str()) {
                *sizes.entry(k).or_default().entry(v.clone()).or_default() += 1;
            }
        }
        for d in &p.rejected {
            *rejected.entry(d.clone()).or_default() += 1;
        }
    }
    let look = settings::look(home);
    json!({
        "schema": SCHEMA,
        "install_id": f.install_id,
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "os_version": os_version(),
        "arch": std::env::consts::ARCH,
        "counts": f.counts,
        "minutes": f.minutes,
        "filters": f.filters,
        "profile": crate::profile::current(home, None).map(|p| p.id.as_str()).unwrap_or("none"),
        "look": { "style": look["style"], "theme": look["theme"], "density": look["density"] },
        "projects": range(crate::project::list(home).len() as u64),
        "sizes": sizes,
        "rejected": rejected,
    })
}

/// The OS release, such as 15.1 on macOS. Only digits, letters and dots.
fn os_version() -> String {
    let raw = if cfg!(target_os = "macos") {
        std::process::Command::new("sw_vers").arg("-productVersion").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    } else if cfg!(windows) {
        // "Microsoft Windows [Version 10.0.22631.4317]"
        let out = std::process::Command::new("cmd").args(["/C", "ver"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        out.rsplit(' ').next().unwrap_or("").trim().trim_end_matches(']').to_string()
    } else {
        std::fs::read_to_string("/etc/os-release")
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("VERSION_ID="))
            .unwrap_or("")
            .trim_matches('"')
            .to_string()
    };
    // Major and minor release only, such as 15.1 or 10.0.
    let clean: String = raw.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').take(24).collect();
    clean.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// Whether statistics are on, and the report as it would be sent now.
pub fn preview(home: &Home) -> Value {
    let mut f = load(home);
    if f.install_id.is_empty() {
        f.install_id = "(made when the first count is saved)".to_string();
    }
    json!({
        "sharing": sharing(home),
        "disabled_by_env": disabled_by_env(),
        "terms_accepted": crate::terms::accepted(home),
        "endpoint": ENDPOINT,
        "last_sent": f.last_sent,
        "next_report": payload(home, &f),
    })
}

pub fn settings_section() -> Section {
    Section::new(SECTION, "Usage statistics", Level::Global)
        .describe(
            "Helps decide what to improve, and the totals are public at plonix.io/analytics. Anonymous and at most once a day: a random install id, \
             the Plonix version, your OS and CPU type, how often features and screens were used, and sizes as ranges. \
             Never URLs, hosts, traffic, project names, paths or anything you type.",
        )
        .order(90)
        .field(Field::toggle("share", "Share anonymous usage statistics", false).help(
            "PLONIX_NO_ANALYTICS=1 or DO_NOT_TRACK=1 turns it off too. `plonix usage` shows the next report as it would be sent.",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_hold_only_the_listed_fields() {
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        let mut f = File { install_id: "0123456789abcdef0123456789abcdef".into(), ..Default::default() };
        f.counts.insert("bench_send".to_string(), 3);
        let rules = crate::scope::ScopeRules {
            rules: vec![
                crate::exclude::exclusion_rule("www.google-analytics.com", "analytics", 0),
                crate::exclude::exclusion_rule("secret-target.internal", "mine", 0),
            ],
        };
        f.projects.insert("p1".into(), project_snapshot(4321, &["a.example".into(), "b.example".into()], &rules));
        let p = payload(&home, &f);
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["arch", "counts", "filters", "install_id", "look", "minutes", "os", "os_version", "profile", "projects", "rejected", "schema", "sizes", "version"]);
        assert!(p["os_version"].as_str().unwrap().chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
        assert_eq!(p["rejected"], json!({ "google-analytics.com": 1, "other": 1 }));
        assert_eq!(p["sizes"]["requests"], json!({ "1k-10k": 1 }));
        assert_eq!(p["sizes"]["hosts"], json!({ "2-5": 1 }));
        assert_eq!(p["profile"], "none");
        let text = p.to_string();
        assert!(!text.contains("secret-target") && !text.contains("a.example") && !text.contains("4321"), "{text}");
    }

    #[test]
    fn searches_count_kinds_never_values() {
        assert_eq!(filter_kinds(r#"host:secret.example -status:404 "set-cookie: sid" passw is:graphql HOST:x foo:bar"#), ["-status", "host", "is", "text"]);
        assert!(filter_kinds("").is_empty());
        assert_eq!(filter_kinds("host:"), ["text"]);
    }

    #[test]
    fn sizes_are_ranges() {
        assert_eq!([0, 1, 5, 6, 100, 101, 1000, 1001, 100_001].map(range), ["0", "1", "2-5", "6-20", "21-100", "101-1k", "101-1k", "1k-10k", "100k+"]);
        assert_eq!(known_domain("cdn.segment.io"), "segment.io");
        assert_eq!(known_domain("*.doubleclick.net"), "doubleclick.net");
        assert_eq!(known_domain("api.target.example"), OTHER);
    }

    #[test]
    fn the_endpoint_accepts_every_event() {
        let function = include_str!("../../../functions/api/usage.js");
        for e in EVENTS {
            assert!(function.contains(&format!("'{e}'")), "functions/api/usage.js does not accept {e}");
            assert!(e.len() <= 40 && e.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
        for s in SCREENS {
            assert!(function.contains(&format!("'{s}'")), "functions/api/usage.js does not accept the screen {s}");
        }
        for k in FILTER_KINDS.iter().chain(RANGES) {
            assert!(function.contains(&format!("'{k}'")), "functions/api/usage.js does not accept {k}");
        }
        for d in known_domains() {
            assert!(function.contains(&format!("'{d}'")), "functions/api/usage.js does not accept the domain {d}");
        }
        for p in crate::profile::profiles() {
            assert!(function.contains(&format!("'{}'", p.id)), "functions/api/usage.js does not accept the profile {}", p.id);
        }
    }

    #[test]
    fn nothing_is_kept_without_consent() {
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        assert!(!sharing(&home), "terms not accepted");
        crate::terms::accept(&home).unwrap();
        assert!(!sharing(&home), "off until the user turns it on");
        set_sharing(&home, true).unwrap();
        assert_eq!(sharing(&home), !disabled_by_env());
        set_sharing(&home, false).unwrap();
        assert!(!sharing(&home));
        assert!(!path(&home).exists());
        assert!(!may_send(), "test builds never send");
    }
}
