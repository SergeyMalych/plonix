//! What AI agents may do through the local API.
//!
//! Agents (the MCP server, `plonix mcp`) authenticate with their own token,
//! `$PLONIX_HOME/agent-token`, never the full API token. The engine checks
//! every agent request against the capability list of the current
//! [`AgentMode`], so the limit holds even if the agent's client is changed.
//!
//! Today the only mode is [`AgentMode::ReadOnly`]: agents can look at
//! traffic, the map, detected technologies, scope and findings (and export
//! findings as a report), and cannot send or replay requests, change scope,
//! record, edit or delete findings or control the engine.
//!
//! The one thing an agent may leave behind is a *suggestion*: an edited
//! version of a request the user is working on in the Bench
//! (`POST /api/bench/proposals`, see [`crate::proposal`]). It is kept in
//! memory for the user to review; it sends nothing, and only the user can
//! apply it to the draft (and then send it themselves).
//!
//! # Adding an active mode later
//!
//! An opt-in mode that lets agents send and replay requests would:
//! 1. add an `AgentMode::Active` variant, chosen by the user in the Agents
//!    screen or with a CLI flag and stored per project (never by the agent);
//! 2. list `POST /api/send` and `POST /api/replay` in [`capabilities`] for
//!    that mode only. Those routes already refuse hosts that are not
//!    accepted into scope, so an agent could never reach outside scope;
//! 3. add matching MCP tools in `plonix mcp`, shown only when the engine
//!    reports that mode.
//!
//! Scope changes and engine control stay user-only in every mode.
//!
//! # Settings
//!
//! Within the mode, the user narrows access further with [`AgentSettings`]
//! (`$PLONIX_HOME/agents.json`, changed in Settings › AI agents): turn agent
//! access off, switch off groups of capabilities, and choose whether agents
//! see all captured traffic or only in-scope hosts.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::SystemTime;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::now_ms;
use crate::paths::{Home, write_private};
use crate::settings::{Field, Level, Section, Values};

/// Who is calling the API, decided by the bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The user's own clients: the window, the app and the CLI.
    User,
    /// An AI agent, limited to its mode's capabilities.
    Agent,
}

/// What agents are allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMode {
    /// Look, never touch.
    ReadOnly,
}

impl AgentMode {
    pub fn current() -> Self {
        AgentMode::ReadOnly
    }
}

/// One thing an agent may do, as an API route.
#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    pub method: &'static str,
    /// Path pattern; `{x}` matches one non-empty segment.
    pub path: &'static str,
    pub what: &'static str,
    pub group: Group,
}

/// Capabilities the user can switch off together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// Engine status and this policy; always on.
    Basics,
    Traffic,
    Insights,
    Map,
    Scope,
    Findings,
    Scan,
    /// Suggesting edits to a Bench draft, for the user to review.
    Bench,
}

impl Group {
    /// The groups the user can switch, with a label for the settings screen.
    pub const SWITCHABLE: &[(Group, &'static str)] = &[
        (Group::Traffic, "Captured requests and responses"),
        (Group::Insights, "What stands out in a request (tokens, personal data, secrets)"),
        (Group::Map, "Hosts, endpoints and detected technologies"),
        (Group::Scope, "Scope rules and suggestions"),
        (Group::Findings, "Findings"),
        (Group::Scan, "Scan detectors, tactics and suggested profiles"),
        (Group::Bench, "Suggest edits to a Bench request (you review and apply them; nothing is sent)"),
    ];
}

const fn cap(path: &'static str, what: &'static str, group: Group) -> Capability {
    Capability { method: "GET", path, what, group }
}

const READ_ONLY: &[Capability] = &[
    cap("/api/status", "Engine status and capture counts", Group::Basics),
    cap("/api/traffic", "Search captured traffic", Group::Traffic),
    cap("/api/traffic/{id}", "One request and its response", Group::Traffic),
    cap("/api/traffic/{id}/insights", "Tokens, personal data and secrets spotted in a request", Group::Insights),
    cap("/api/traffic/{id}/messages", "WebSocket messages sent over a request's connection", Group::Traffic),
    cap("/api/hosts", "Hosts seen", Group::Map),
    cap("/api/hosts/{host}/endpoints", "Endpoints seen on a host", Group::Map),
    cap("/api/tech", "Technologies detected on each host", Group::Map),
    cap("/api/tech/{host}", "Technologies detected on each host", Group::Map),
    cap("/api/scope", "Scope rules and suggested domains", Group::Scope),
    cap("/api/findings", "Recorded findings", Group::Findings),
    cap("/api/findings/{id}", "One finding", Group::Findings),
    cap("/api/findings/export", "Findings as a report with their evidence requests", Group::Findings),
    cap("/api/scan/catalog", "Available scan detectors and tactics", Group::Scan),
    cap("/api/scan/suggest/{host}", "Suggested scan profile for a host (read-only advice)", Group::Scan),
    cap("/api/agents", "This access policy", Group::Basics),
    cap("/api/skills", "Skills: playbooks for jobs in Plonix", Group::Basics),
    cap("/api/skills/{name}", "Skills: playbooks for jobs in Plonix", Group::Basics),
    // The only write: it stores a suggestion for the user to review on the
    // Bench. It cannot send, apply or change anything else.
    Capability { method: "POST", path: "/api/bench/proposals", what: "Suggest an edit to a Bench request, for the user to review", group: Group::Bench },
];

/// Which captured traffic agents may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataScope {
    /// Only requests to hosts accepted into scope (the default).
    #[default]
    InScope,
    /// Everything captured, in scope or not.
    All,
}

/// The user's choices about agent access. Agents can read them but never
/// change them: the settings route is not in any mode's capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// When false, every agent request is refused.
    pub enabled: bool,
    pub data: DataScope,
    /// Capability groups the user switched off.
    pub off: Vec<Group>,
    /// Largest context an "Ask Claude Code" hand-off may carry before the
    /// user has to confirm or trim it, in estimated tokens.
    pub context_budget: usize,
    /// Each request or response body in a hand-off is clipped to this.
    pub max_body_chars: usize,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self { enabled: true, data: DataScope::InScope, off: vec![], context_budget: 8000, max_body_chars: 4000 }
    }
}

impl AgentSettings {
    pub const BUDGETS: &[usize] = &[2000, 4000, 8000, 16000, 32000];

    pub fn load(home: &Home) -> Self {
        std::fs::read(home.agent_settings()).ok().and_then(|b| serde_json::from_slice::<Self>(&b).ok()).map(Self::sanitized).unwrap_or_default()
    }

    pub fn save(&self, home: &Home) -> Result<()> {
        write_private(&home.agent_settings(), &serde_json::to_vec_pretty(self)?)
    }

    /// Keeps values in sane bounds whatever the file or request said.
    pub fn sanitized(mut self) -> Self {
        self.off.retain(|g| *g != Group::Basics);
        self.off.dedup();
        self.context_budget = self.context_budget.clamp(500, 200_000);
        self.max_body_chars = self.max_body_chars.clamp(200, 100_000);
        self
    }

    pub fn group_on(&self, g: Group) -> bool {
        g == Group::Basics || !self.off.contains(&g)
    }

    pub fn in_scope_only(&self) -> bool {
        self.data == DataScope::InScope
    }
}

// ---- the AI agents section of Settings -----------------------------------

pub const SETTINGS_SECTION: &str = "agents";

/// The settings key for switching a capability group on or off.
fn group_key(g: Group) -> String {
    format!("allow_{}", serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default())
}

/// AI agent access as a Settings section. The values live in `agents.json`,
/// where the engine has always kept them.
pub fn settings_section() -> Section {
    let mut s = Section::new(SETTINGS_SECTION, "AI agents", Level::Global)
        .describe("What AI agents such as Claude Code may read from your projects, and how much an \"Ask Claude\" hand-off may carry. Agents can only read (and suggest Bench edits for you to apply); they can never change these settings.")
        .order(40)
        .field(Field::toggle("enabled", "Let AI agents read projects", true).group("Access").help("When off, every agent request is refused."))
        .field(
            Field::choice("data", "Agents see", "in_scope", &[("in_scope", "In-scope hosts only"), ("all", "Everything captured")])
                .group("Access")
                .help("In-scope only keeps traffic to other sites away from agents."),
        );
    for (g, label) in Group::SWITCHABLE {
        s = s.field(Field::toggle(&group_key(*g), label, true).group("What agents can read"));
    }
    s.field(
        Field::number("context_budget", "Hand-off size limit", 8000, 500, 200_000)
            .unit("tokens")
            .group("Ask Claude")
            .help("Above this, Plonix asks you to confirm or trim before handing context to Claude Code."),
    )
    .field(Field::number("max_body_chars", "Clip each body to", 4000, 200, 100_000).unit("characters").group("Ask Claude"))
    .stored_by(|home| AgentSettings::load(home).to_values(), |home, v| AgentSettings::from_values(v).sanitized().save(home))
}

impl AgentSettings {
    pub fn to_values(&self) -> Values {
        let mut v = Values::new();
        v.insert("enabled".into(), json!(self.enabled));
        v.insert("data".into(), json!(if self.in_scope_only() { "in_scope" } else { "all" }));
        for (g, _) in Group::SWITCHABLE {
            v.insert(group_key(*g), json!(self.group_on(*g)));
        }
        v.insert("context_budget".into(), json!(self.context_budget));
        v.insert("max_body_chars".into(), json!(self.max_body_chars));
        v
    }

    pub fn from_values(v: &Values) -> Self {
        let d = Self::default();
        let num = |k: &str, def: usize| v.get(k).and_then(Value::as_u64).map(|n| n as usize).unwrap_or(def);
        Self {
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(d.enabled),
            data: if v.get("data").and_then(Value::as_str) == Some("all") { DataScope::All } else { DataScope::InScope },
            off: Group::SWITCHABLE.iter().filter(|(g, _)| v.get(&group_key(*g)).and_then(Value::as_bool) == Some(false)).map(|(g, _)| *g).collect(),
            context_budget: num("context_budget", d.context_budget),
            max_body_chars: num("max_body_chars", d.max_body_chars),
        }
    }
}

/// The agent settings as the engine applies them. Every open project reads
/// the same file, so a change made in one window (or on the Start screen)
/// reaches all of them on their next request.
pub struct SharedAgentSettings {
    home: Home,
    cached: RwLock<(Option<(SystemTime, u64)>, AgentSettings)>,
}

impl SharedAgentSettings {
    pub fn new(home: &Home) -> Self {
        Self { home: home.clone(), cached: RwLock::new((Self::stamp(home), AgentSettings::load(home))) }
    }

    /// Changes when the file does: its modification time and size.
    fn stamp(home: &Home) -> Option<(SystemTime, u64)> {
        std::fs::metadata(home.agent_settings()).ok().and_then(|m| Some((m.modified().ok()?, m.len())))
    }

    pub fn get(&self) -> AgentSettings {
        let stamp = Self::stamp(&self.home);
        {
            let c = self.cached.read().unwrap();
            if c.0 == stamp {
                return c.1.clone();
            }
        }
        let fresh = AgentSettings::load(&self.home);
        *self.cached.write().unwrap() = (stamp, fresh.clone());
        fresh
    }

    pub fn set(&self, new: AgentSettings) -> Result<AgentSettings> {
        let new = new.sanitized();
        new.save(&self.home)?;
        *self.cached.write().unwrap() = (Self::stamp(&self.home), new.clone());
        Ok(new)
    }
}

/// Why an agent request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The user turned agent access off.
    Disabled,
    /// The mode does not allow this at all (writes, engine control).
    NotAllowed,
    /// The user switched off this capability.
    SwitchedOff,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Disabled => "agents_disabled",
            Refusal::NotAllowed => "agent_not_allowed",
            Refusal::SwitchedOff => "capability_off",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Refusal::Disabled => "agent access to Plonix is turned off; the user can turn it on in Settings › AI agents",
            Refusal::NotAllowed => {
                "agents have read-only access: they can read traffic, the map, scope and findings, but not send requests or change anything"
            }
            Refusal::SwitchedOff => "the user has switched this capability off for agents in Settings › AI agents",
        }
    }
}

/// The capabilities agents have right now: the mode's, minus what the user
/// switched off.
pub fn effective(mode: AgentMode, settings: &AgentSettings) -> Vec<Capability> {
    if !settings.enabled {
        return capabilities(mode).iter().filter(|c| c.path == "/api/agents").cloned().collect();
    }
    capabilities(mode).iter().filter(|c| settings.group_on(c.group)).cloned().collect()
}

/// Checks one agent request against the mode and the user's settings.
pub fn check(mode: AgentMode, settings: &AgentSettings, method: &str, path: &str) -> Result<(), Refusal> {
    let Some(c) = capabilities(mode).iter().find(|c| c.method.eq_ignore_ascii_case(method) && path_matches(c.path, path)) else {
        return Err(Refusal::NotAllowed);
    };
    // Agents may always read the policy, to explain a refusal.
    if c.path == "/api/agents" {
        return Ok(());
    }
    if !settings.enabled {
        return Err(Refusal::Disabled);
    }
    if !settings.group_on(c.group) {
        return Err(Refusal::SwitchedOff);
    }
    Ok(())
}

/// What agents may do in a mode.
pub fn capabilities(mode: AgentMode) -> &'static [Capability] {
    match mode {
        AgentMode::ReadOnly => READ_ONLY,
    }
}

/// Things no agent may do in the current mode, for display.
pub fn not_allowed(mode: AgentMode) -> &'static [&'static str] {
    match mode {
        AgentMode::ReadOnly => &[
            "Send or replay requests",
            "Accept, reject or remove scope rules",
            "Record, edit or delete findings",
            "Open browsers, sign in to the window or stop the engine",
            "Install, update or remove anything from the Market",
            "See, edit, forward or drop requests held in Intercept",
            "Apply a suggested edit to a Bench draft, or start a payload run",
        ],
    }
}

/// Whether an agent in `mode` may call `method path`.
pub fn allowed(mode: AgentMode, method: &str, path: &str) -> bool {
    capabilities(mode).iter().any(|c| c.method.eq_ignore_ascii_case(method) && path_matches(c.path, path))
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let mut p = pattern.split('/');
    let mut a = path.split('/');
    loop {
        match (p.next(), a.next()) {
            (None, None) => return true,
            (Some(want), Some(got)) => {
                let ok = if want.starts_with('{') { !got.is_empty() } else { want == got };
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Agent clients seen since the engine started, for the Agents screen.
#[derive(Default)]
pub struct AgentActivity {
    clients: Mutex<HashMap<String, ClientActivity>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientActivity {
    pub name: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub requests: u64,
    /// Requests refused because the mode does not allow them.
    pub refused: u64,
    pub last_request: String,
}

impl AgentActivity {
    pub fn record(&self, client: &str, method: &str, path: &str, refused: bool) {
        let now = now_ms();
        let mut map = self.clients.lock().unwrap();
        let c = map.entry(client.to_string()).or_insert_with(|| ClientActivity {
            name: client.to_string(),
            first_seen: now,
            last_seen: now,
            requests: 0,
            refused: 0,
            last_request: String::new(),
        });
        c.last_seen = now;
        // Reading this policy is how idle agents say they are still connected.
        if path == "/api/agents" && !refused {
            return;
        }
        c.requests += 1;
        if refused {
            c.refused += 1;
        }
        c.last_request = format!("{method} {path}");
    }

    /// Most recently active first.
    pub fn clients(&self) -> Vec<ClientActivity> {
        let mut v: Vec<_> = self.clients.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.last_seen));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_allows_reads() {
        let m = AgentMode::ReadOnly;
        for (method, path) in [
            ("GET", "/api/status"),
            ("GET", "/api/traffic"),
            ("GET", "/api/traffic/42"),
            ("GET", "/api/traffic/42/insights"),
            ("GET", "/api/traffic/42/messages"),
            ("GET", "/api/hosts"),
            ("GET", "/api/hosts/example.com/endpoints"),
            ("GET", "/api/tech"),
            ("GET", "/api/tech/example.com"),
            ("GET", "/api/scope"),
            ("GET", "/api/findings"),
            ("GET", "/api/findings/3"),
            ("GET", "/api/findings/export"),
            ("GET", "/api/scan/catalog"),
            ("GET", "/api/scan/suggest/example.com"),
            ("GET", "/api/agents"),
            ("POST", "/api/bench/proposals"),
        ] {
            assert!(allowed(m, method, path), "{method} {path} should be allowed");
        }
    }

    #[test]
    fn read_only_refuses_everything_else() {
        let m = AgentMode::ReadOnly;
        for (method, path) in [
            ("POST", "/api/send"),
            ("POST", "/api/replay"),
            ("POST", "/api/scan"),
            ("POST", "/api/scope/accept"),
            ("POST", "/api/scope/reject"),
            ("POST", "/api/scope/remove"),
            ("POST", "/api/findings"),
            ("POST", "/api/shutdown"),
            ("POST", "/api/ui/launch"),
            ("POST", "/api/browser/open"),
            ("GET", "/api/rules/../send"),
            ("GET", "/api/traffic/"),
            ("GET", "/api/traffic/1/insights/x"),
            ("GET", "/api/rules"),
            ("POST", "/api/traffic"),
            ("DELETE", "/api/findings"),
            ("PATCH", "/api/findings/3"),
            ("PUT", "/api/findings/3"),
            ("DELETE", "/api/findings/3"),
            ("POST", "/api/findings/3"),
            ("GET", "/api/findings/3/x"),
            ("GET", "/api/intercept"),
            ("PUT", "/api/intercept"),
            ("POST", "/api/intercept/1/forward"),
            ("POST", "/api/intercept/1/drop"),
            ("POST", "/api/intercept/forward-all"),
            ("GET", "/api/replace"),
            ("POST", "/api/replace"),
            ("PATCH", "/api/replace/1"),
            ("DELETE", "/api/replace/1"),
            ("GET", "/api/bench/proposals"),
            ("POST", "/api/bench/proposals/1/diff"),
            ("DELETE", "/api/bench/proposals/1"),
            ("POST", "/api/bench/proposals/1/apply"),
            ("POST", "/api/run"),
        ] {
            assert!(!allowed(m, method, path), "{method} {path} should be refused");
        }
    }

    #[test]
    fn suggesting_a_bench_edit_is_the_only_write() {
        let writes: Vec<&Capability> = READ_ONLY.iter().filter(|c| c.method != "GET").collect();
        assert_eq!(writes.len(), 1);
        assert_eq!((writes[0].method, writes[0].path), ("POST", "/api/bench/proposals"));
        let mut s = AgentSettings::default();
        assert_eq!(check(AgentMode::ReadOnly, &s, "POST", "/api/bench/proposals"), Ok(()));
        s.off = vec![Group::Bench];
        assert_eq!(check(AgentMode::ReadOnly, &s, "POST", "/api/bench/proposals"), Err(Refusal::SwitchedOff));
    }

    #[test]
    fn settings_narrow_what_agents_get() {
        let m = AgentMode::ReadOnly;
        let mut s = AgentSettings::default();
        assert_eq!(check(m, &s, "GET", "/api/traffic/1"), Ok(()));
        assert_eq!(check(m, &s, "POST", "/api/send"), Err(Refusal::NotAllowed));
        s.off = vec![Group::Traffic];
        assert_eq!(check(m, &s, "GET", "/api/traffic/1"), Err(Refusal::SwitchedOff));
        assert_eq!(check(m, &s, "GET", "/api/traffic/1/insights"), Ok(()));
        assert!(!effective(m, &s).iter().any(|c| c.group == Group::Traffic));
        s.enabled = false;
        assert_eq!(check(m, &s, "GET", "/api/status"), Err(Refusal::Disabled));
        assert_eq!(check(m, &s, "GET", "/api/agents"), Ok(()));
        assert_eq!(check(m, &s, "POST", "/api/agents/settings"), Err(Refusal::NotAllowed));
        assert_eq!(check(m, &s, "PUT", "/api/agents/settings"), Err(Refusal::NotAllowed));
        assert_eq!(effective(m, &s).len(), 1);
    }

    #[test]
    fn settings_are_sanitized_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home { root: dir.path().into() };
        assert_eq!(AgentSettings::load(&home), AgentSettings::default());
        let s = AgentSettings { off: vec![Group::Basics, Group::Scope], context_budget: 1, ..Default::default() }.sanitized();
        assert_eq!((s.off.clone(), s.context_budget), (vec![Group::Scope], 500));
        s.save(&home).unwrap();
        assert_eq!(AgentSettings::load(&home), s);
        std::fs::write(home.agent_settings(), "{\"data\": \"all\"}").unwrap();
        let s = AgentSettings::load(&home);
        assert!(s.enabled && s.data == DataScope::All && s.context_budget == 8000);
    }

    #[test]
    fn activity_is_recorded() {
        let a = AgentActivity::default();
        a.record("mcp", "GET", "/api/agents", false);
        a.record("mcp", "GET", "/api/traffic", false);
        a.record("mcp", "POST", "/api/send", true);
        let c = &a.clients()[0];
        assert_eq!((c.requests, c.refused), (2, 1));
        assert_eq!(c.last_request, "POST /api/send");
    }

    #[test]
    fn agent_settings_live_in_the_settings_hub() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home { root: dir.path().into() };
        let shared = SharedAgentSettings::new(&home);
        assert!(shared.get().enabled && shared.get().in_scope_only());

        // Defaults show through the settings section.
        let v = crate::settings::global(&home, SETTINGS_SECTION);
        assert_eq!(v["data"], "in_scope");
        assert_eq!(v["allow_traffic"], true);

        // Saving the section writes agents.json, and engines pick it up.
        let section = crate::settings::section(SETTINGS_SECTION).unwrap();
        let new = section.check(&json!({ "data": "all", "allow_findings": false, "context_budget": 2000 }), &v).unwrap();
        crate::settings::save_global(&home, SETTINGS_SECTION, &new).unwrap();
        let saved = AgentSettings::load(&home);
        assert_eq!(saved.data, DataScope::All);
        assert_eq!(saved.off, vec![Group::Findings]);
        assert_eq!(saved.context_budget, 2000);
        let fresh = shared.get();
        assert_eq!(fresh, saved);

        // Values round-trip, and bad numbers are refused field by field.
        assert_eq!(AgentSettings::from_values(&saved.to_values()), saved);
        let bad = section.check(&json!({ "context_budget": 10 }), &v).unwrap_err();
        assert_eq!(bad[0].field, "context_budget");
    }
}
