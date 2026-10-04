//! What AI agents may do through the local API.
//!
//! Agents (the MCP server, `plonix mcp`) authenticate with their own token,
//! `$PLONIX_HOME/agent-token`, never the full API token. The engine checks
//! every agent request against the capability list of the current
//! [`AgentMode`], so the limit holds even if the agent's client is changed.
//!
//! Today the only mode is [`AgentMode::ReadOnly`]: agents can look at
//! traffic, the map, detected technologies, scope and findings, and cannot
//! send or replay requests, change scope, record findings or control the
//! engine.
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
//! (`$PLONIX_HOME/agents.json`, changed from the Agents screen): turn agent
//! access off, switch off groups of capabilities, and choose whether agents
//! see all captured traffic or only in-scope hosts.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::now_ms;
use crate::paths::{Home, write_private};

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
    cap("/api/hosts", "Hosts seen", Group::Map),
    cap("/api/hosts/{host}/endpoints", "Endpoints seen on a host", Group::Map),
    cap("/api/tech", "Technologies detected on each host", Group::Map),
    cap("/api/tech/{host}", "Technologies detected on each host", Group::Map),
    cap("/api/scope", "Scope rules and suggested domains", Group::Scope),
    cap("/api/findings", "Recorded findings", Group::Findings),
    cap("/api/scan/catalog", "Available scan detectors and tactics", Group::Scan),
    cap("/api/scan/suggest/{host}", "Suggested scan profile for a host (read-only advice)", Group::Scan),
    cap("/api/agents", "This access policy", Group::Basics),
    cap("/api/skills", "Skills: playbooks for jobs in Plonix", Group::Basics),
    cap("/api/skills/{name}", "Skills: playbooks for jobs in Plonix", Group::Basics),
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
            Refusal::Disabled => "agent access to Plonix is turned off; the user can turn it on in the Agents screen",
            Refusal::NotAllowed => {
                "agents have read-only access: they can read traffic, the map, scope and findings, but not send requests or change anything"
            }
            Refusal::SwitchedOff => "the user has switched this capability off for agents in the Agents screen",
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
            "Record or change findings",
            "Open browsers, sign in to the window or stop the engine",
            "Install, update or remove anything from the Market",
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
            ("GET", "/api/hosts"),
            ("GET", "/api/hosts/example.com/endpoints"),
            ("GET", "/api/tech"),
            ("GET", "/api/tech/example.com"),
            ("GET", "/api/scope"),
            ("GET", "/api/findings"),
            ("GET", "/api/scan/catalog"),
            ("GET", "/api/scan/suggest/example.com"),
            ("GET", "/api/agents"),
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
        ] {
            assert!(!allowed(m, method, path), "{method} {path} should be refused");
        }
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
}
