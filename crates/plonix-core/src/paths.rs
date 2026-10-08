//! On-disk layout.
//!
//! ```text
//! $PLONIX_HOME (default ~/.plonix)
//! ├── ca.pem / ca.key        local certificate authority (trust once)
//! ├── api-token              bearer token for the local API (0600)
//! ├── agent-token            read-only token for AI agents (0600)
//! ├── agents.json            what agents may see (Settings › AI agents)
//! ├── engine.json            the current session's address (the last one opened)
//! ├── sessions/<id>.json     one file per open project session
//! ├── hub.json               address of the Start screen, while it runs
//! ├── projects.json          projects Plonix knows about, and where they are
//! ├── settings.json          settings shared by all projects
//! ├── logs/engine.log
//! ├── crashes/               crash reports, never sent (see crate::crash)
//! └── projects/              default place for new projects (see below)
//! ```
//!
//! Each project is a folder of its own, anywhere on disk (see
//! [`crate::project`]). Without `$PLONIX_HOME`, new projects go in `~/Plonix`.

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
            None => match std::env::var_os("PLONIX_HOME").filter(|p| !p.is_empty()) {
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
        std::fs::create_dir_all(self.root.join("logs"))?;
        std::fs::create_dir_all(self.sessions_dir())?;
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
    /// The user's agent access settings, see [`crate::access::AgentSettings`].
    pub fn agent_settings(&self) -> PathBuf {
        self.root.join("agents.json")
    }
    pub fn engine_file(&self) -> PathBuf {
        self.root.join("engine.json")
    }
    pub fn log_file(&self) -> PathBuf {
        self.root.join("logs").join("engine.log")
    }
    /// Crash reports, see [`crate::crash`].
    pub fn crashes_dir(&self) -> PathBuf {
        self.root.join("crashes")
    }
    pub fn browser_profile(&self) -> PathBuf {
        self.root.join("browser")
    }
    /// Where earlier versions kept a project's database.
    pub fn project_db(&self, project: &str) -> PathBuf {
        self.root.join("projects").join(format!("{project}.db"))
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    pub fn hub_file(&self) -> PathBuf {
        self.root.join("hub.json")
    }
    pub fn projects_file(&self) -> PathBuf {
        self.root.join("projects.json")
    }
    /// Where new projects go unless the user picks a folder: `~/Plonix` for
    /// the standard data directory, else inside the chosen one.
    pub fn default_projects_dir(&self) -> PathBuf {
        match std::env::var_os("HOME").map(PathBuf::from) {
            Some(h) if self.root == h.join(".plonix") => h.join("Plonix"),
            _ => self.root.join("projects"),
        }
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<PathBuf>,
}

/// Replaces a file in one step, so readers never see half of it.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    replace(path, data, 0o644)
}

/// Writes a file readable only by the current user, in one step like [`write_atomic`].
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    replace(path, data, 0o600)
}

fn replace(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Unique per call, so two threads saving the same file never share a temp file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, mode);
        #[cfg(not(unix))]
        let _ = mode;
        let mut f = opts.open(&tmp)?;
        std::io::Write::write_all(&mut f, data)?;
        // On disk before the rename, so a crash cannot leave an empty file in its place.
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// Before rewriting a JSON file from its parsed contents: when it exists but
/// does not parse, moves it aside (`<name>.corrupt-<ms>`) so the rewrite
/// cannot silently replace what the user had with defaults.
pub fn set_aside_unreadable<T: serde::de::DeserializeOwned>(path: &Path) {
    let Ok(bytes) = std::fs::read(path) else { return };
    if serde_json::from_slice::<T>(&bytes).is_ok() {
        return;
    }
    let aside = path.with_file_name(format!("{}.corrupt-{}", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(), crate::model::now_ms()));
    match std::fs::rename(path, &aside) {
        Ok(()) => tracing::warn!("{} could not be read; kept it as {}", path.display(), aside.display()),
        Err(e) => tracing::warn!("{} could not be read or set aside: {e}", path.display()),
    }
}
