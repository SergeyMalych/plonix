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

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

use crate::model::now_ms;

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
}

const READ_ONLY: &[Capability] = &[
    Capability { method: "GET", path: "/api/status", what: "Engine status and capture counts" },
    Capability { method: "GET", path: "/api/traffic", what: "Search captured traffic" },
    Capability { method: "GET", path: "/api/traffic/{id}", what: "One request and its response" },
    Capability { method: "GET", path: "/api/traffic/{id}/insights", what: "Tokens, personal data and secrets spotted in a request" },
    Capability { method: "GET", path: "/api/hosts", what: "Hosts seen" },
    Capability { method: "GET", path: "/api/hosts/{host}/endpoints", what: "Endpoints seen on a host" },
    Capability { method: "GET", path: "/api/tech", what: "Technologies detected on each host" },
    Capability { method: "GET", path: "/api/tech/{host}", what: "Technologies detected on each host" },
    Capability { method: "GET", path: "/api/scope", what: "Scope rules and suggested domains" },
    Capability { method: "GET", path: "/api/findings", what: "Recorded findings" },
    Capability { method: "GET", path: "/api/agents", what: "This access policy" },
];

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
