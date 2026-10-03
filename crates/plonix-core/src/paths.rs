//! On-disk layout.
//!
//! ```text
//! $PLONIX_HOME (default ~/.plonix)
//! ├── ca.pem / ca.key        local certificate authority (trust once)
//! ├── api-token              bearer token for the local API (0600)
//! ├── agent-token            read-only token for AI agents (0600)
//! ├── engine.json            address of the running engine, written on start
//! ├── logs/engine.log
//! ├── browser/               profile for the pre-configured browser
//! └── projects/<name>.db     one SQLite database per project
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    /// `$PLONIX_HOME`, else `~/.plonix`.
    pub fn resolve(explicit: Option<&Path>) -> Result<Self> {
        let root = match explicit {
            Some(p) => p.to_path_buf(),
            None => match std::env::var_os("PLONIX_HOME") {
                Some(p) => PathBuf::from(p),
                None => {
                    let home = std::env::var_os("HOME").context("HOME is not set")?;
                    PathBuf::from(home).join(".plonix")
                }
            },
        };
        Ok(Self { root })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(self.root.join("projects"))?;
        std::fs::create_dir_all(self.root.join("logs"))?;
        Ok(())
    }

    pub fn ca_cert(&self) -> PathBuf {
        self.root.join("ca.pem")
    }
    pub fn ca_key(&self) -> PathBuf {
        self.root.join("ca.key")
    }
    pub fn api_token(&self) -> PathBuf {
        self.root.join("api-token")
    }
    /// Token for AI agents, limited to what [`crate::access`] allows.
    pub fn agent_token(&self) -> PathBuf {
        self.root.join("agent-token")
    }
    pub fn engine_file(&self) -> PathBuf {
        self.root.join("engine.json")
    }
    pub fn log_file(&self) -> PathBuf {
        self.root.join("logs").join("engine.log")
    }
    pub fn browser_profile(&self) -> PathBuf {
        self.root.join("browser")
    }
    pub fn project_db(&self, project: &str) -> PathBuf {
        self.root.join("projects").join(format!("{project}.db"))
    }

    /// Returns the API token, creating a random one on first use.
    pub fn load_or_create_token(&self) -> Result<String> {
        load_or_create_secret(&self.api_token())
    }

    /// Returns the agent token, creating a random one on first use.
    pub fn load_or_create_agent_token(&self) -> Result<String> {
        load_or_create_secret(&self.agent_token())
    }

    pub fn read_engine_info(&self) -> Option<EngineInfo> {
        let data = std::fs::read_to_string(self.engine_file()).ok()?;
        serde_json::from_str(&data).ok()
    }
}

/// Reads a random secret from `path`, creating it (0600) on first use.
fn load_or_create_secret(path: &Path) -> Result<String> {
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let mut buf = [0u8; 24];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf)
        .map_err(|_| anyhow::anyhow!("no system randomness"))?;
    let token: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    write_private(path, token.as_bytes())?;
    Ok(token)
}

/// Written by a running engine so that clients can find it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfo {
    pub pid: u32,
    pub api: String,
    pub proxy: String,
    pub project: String,
    pub started_at: i64,
}

/// Writes a file readable only by the current user.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?;
        f.write_all(data)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))
    }
}
