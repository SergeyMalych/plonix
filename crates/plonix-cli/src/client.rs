//! Client for the engine's local API, shared by the CLI and the MCP server.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use plonix_core::paths::Home;
use serde_json::Value;

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
    initiator: String,
}

pub const NOT_RUNNING: &str = "The Plonix engine is not running. Start it with `plonix start` (or `plonix open <target>`).";

impl Client {
    /// Connects to the engine described by `$PLONIX_HOME/engine.json`.
    pub fn connect(home: &Home, initiator: &str) -> Result<Self> {
        let info = home.read_engine_info().ok_or_else(|| anyhow!(NOT_RUNNING))?;
        let token = std::fs::read_to_string(home.api_token()).map_err(|_| anyhow!(NOT_RUNNING))?;
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
                bail!("{}", v["error"].as_str().unwrap_or("request failed"))
            }
            Err(ureq::Error::Transport(_)) => bail!(NOT_RUNNING),
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
