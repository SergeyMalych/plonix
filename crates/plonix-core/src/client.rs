//! Client for the engine's local API, shared by the CLI and the MCP server.

use std::time::Duration;

use anyhow::{Result, anyhow};
use crate::paths::{EngineInfo, Home};
use serde_json::Value;

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
    initiator: String,
}

pub const NOT_RUNNING: &str = "The Plonix engine is not running. Start it with `plonix start` (or `plonix open <target>`).";

/// Which session commands talk to: `-p`/`$PLONIX_PROJECT` when given, else
/// the current session (the one opened last).
pub fn engine_info(home: &Home, project: Option<&str>) -> Option<EngineInfo> {
    let selector = project.map(str::to_string).or_else(|| std::env::var("PLONIX_PROJECT").ok().filter(|p| !p.trim().is_empty()));
    match selector {
        Some(sel) => crate::session::find(home, &sel),
        None => home.read_engine_info(),
    }
}

/// An error reported by the engine, with its machine-readable code
/// (`out_of_scope`, `not_found`, `bad_query`...).
#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// The engine could not be reached.
#[derive(Debug)]
pub struct NotRunning;

impl std::fmt::Display for NotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(NOT_RUNNING)
    }
}

impl std::error::Error for NotRunning {}

impl Client {
    /// Connects to the session for `project`, if given, else the current one
    /// (or the one `$PLONIX_PROJECT` names).
    pub fn connect_project(home: &Home, project: Option<&str>, initiator: &str) -> Result<Self> {
        match engine_info(home, project) {
            Some(info) => Self::to(home, &info, initiator),
            None => match project {
                Some(p) => Err(anyhow!(NotRunning).context(format!("project '{p}' is not open"))),
                None => Err(anyhow!(NotRunning)),
            },
        }
    }

    /// Connects as an AI agent, with the agent token, to the current session
    /// (or the one `$PLONIX_PROJECT` names): the engine only allows what
    /// agents may do (see `plonix_core::access`).
    pub fn connect_agent(home: &Home, initiator: &str) -> Result<Self> {
        let info = engine_info(home, None).ok_or_else(|| anyhow!(NotRunning))?;
        Self::connect_with(&info, &home.agent_token(), initiator)
    }

    /// Connects to a specific session.
    pub fn to(home: &Home, info: &EngineInfo, initiator: &str) -> Result<Self> {
        Self::connect_with(info, &home.api_token(), initiator)
    }

    fn connect_with(info: &EngineInfo, token_file: &std::path::Path, initiator: &str) -> Result<Self> {
        let token = std::fs::read_to_string(token_file).map_err(|_| anyhow!(NotRunning))?;
        let client = Self {
            base: info.api.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            // A fresh connection per request: a POST sent on a pooled connection
            // the engine has already closed fails instead of being retried.
            agent: ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(Duration::from_secs(180)).max_idle_connections(0).build(),
            initiator: initiator.to_string(),
        };
        Ok(client)
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.handle(self.agent.get(&format!("{}{path}", self.base)).set("Authorization", &self.auth()).set("X-Plonix-Client", &self.initiator).call())
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.handle(
            self.agent
                .post(&format!("{}{path}", self.base))
                .set("Authorization", &self.auth())
                .set("X-Plonix-Client", &self.initiator)
                .send_json(body),
        )
    }

    pub fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.handle(self.request("PUT", path).send_json(body))
    }

    pub fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.handle(self.request("PATCH", path).send_json(body))
    }

    pub fn delete(&self, path: &str) -> Result<Value> {
        self.handle(self.request("DELETE", path).call())
    }

    /// A route that answers with a document rather than JSON (a report).
    pub fn get_text(&self, path: &str) -> Result<String> {
        match self.request("GET", path).call() {
            Ok(resp) => Ok(resp.into_string()?),
            Err(e) => self.handle(Err(e)).map(|_| String::new()),
        }
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        self.agent.request(method, &format!("{}{path}", self.base)).set("Authorization", &self.auth()).set("X-Plonix-Client", &self.initiator)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn handle(&self, r: Result<ureq::Response, ureq::Error>) -> Result<Value> {
        match r {
            Ok(resp) => Ok(resp.into_json()?),
            Err(ureq::Error::Status(_, resp)) => {
                let v: Value = resp.into_json().unwrap_or(Value::Null);
                Err(ApiError {
                    code: v["code"].as_str().unwrap_or("error").to_string(),
                    message: v["error"].as_str().unwrap_or("request failed").to_string(),
                }
                .into())
            }
            // Nothing listening: the engine is not running. Anything else (a
            // reset, a timeout) is worth showing as it is.
            Err(ureq::Error::Transport(t)) if t.kind() == ureq::ErrorKind::ConnectionFailed => Err(NotRunning.into()),
            Err(ureq::Error::Transport(t)) => Err(anyhow!(NotRunning).context(format!("the engine did not answer: {t}"))),
        }
    }
}

/// Percent-encodes a query string value.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
