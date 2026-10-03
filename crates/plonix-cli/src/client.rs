//! Client for the engine's local API, shared by the CLI and the MCP server.

use std::time::Duration;

use anyhow::{Result, anyhow};
use plonix_core::paths::Home;
use serde_json::Value;

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
    initiator: String,
}

pub const NOT_RUNNING: &str = "The Plonix engine is not running. Start it with `plonix start` (or `plonix open <target>`).";

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
    /// Connects to the engine described by `$PLONIX_HOME/engine.json`.
    pub fn connect(home: &Home, initiator: &str) -> Result<Self> {
        let info = home.read_engine_info().ok_or_else(|| anyhow!(NotRunning))?;
        let token = std::fs::read_to_string(home.api_token()).map_err(|_| anyhow!(NotRunning))?;
        let client = Self {
            base: info.api.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            agent: ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(Duration::from_secs(180)).build(),
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
            Err(ureq::Error::Transport(_)) => Err(NotRunning.into()),
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
