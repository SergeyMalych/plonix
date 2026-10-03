//! Finding, starting and stopping the background engine process.
//!
//! `plonix start` re-executes the current binary as `plonix engine`, detached
//! from the terminal, with its output in `$PLONIX_HOME/logs/engine.log`. The
//! engine announces itself in `engine.json`, which every client reads.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use plonix_core::engine::{self, EngineConfig};
use plonix_core::paths::Home;
use serde_json::Value;

use crate::client::{Client, NotRunning};

pub const DEFAULT_PROXY_PORT: u16 = 8080;
pub const DEFAULT_API_PORT: u16 = 8090;

#[derive(Debug, Clone)]
pub struct StartOptions {
    pub project: String,
    pub proxy_port: u16,
    pub api_port: u16,
    pub insecure_upstream: bool,
}

/// A reachable engine and its `/api/status`.
pub struct Connected {
    pub client: Client,
    pub status: Value,
}

/// Returns the running engine, if any. Cleans up a stale `engine.json` left
/// behind by an engine that no longer answers.
pub fn running(home: &Home, initiator: &str) -> Option<Connected> {
    home.read_engine_info()?;
    let client = Client::connect(home, initiator).ok()?;
    match client.get("/api/status") {
        Ok(status) => Some(Connected { client, status }),
        Err(e) => {
            if e.downcast_ref::<NotRunning>().is_some() {
                let _ = std::fs::remove_file(home.engine_file());
            }
            None
        }
    }
}

/// Starts the engine in the background unless one is already running.
/// Returns the engine and whether this call started it.
pub fn start(home: &Home, opts: &StartOptions) -> Result<(Connected, bool)> {
    if let Some(c) = running(home, "cli") {
        return Ok((c, false));
    }
    home.ensure()?;
    let log_path = home.log_file();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_start = log.metadata().map(|m| m.len()).unwrap_or(0);

    let exe = std::env::current_exe().context("locating the plonix executable")?;
    let mut cmd = Command::new(exe);
    cmd.env("PLONIX_HOME", &home.root)
        .args(["engine", "--project", &opts.project])
        .args(["--proxy-port", &opts.proxy_port.to_string()])
        .args(["--api-port", &opts.api_port.to_string()])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if opts.insecure_upstream {
        cmd.arg("--insecure-upstream");
    }
    detach(&mut cmd);
    let mut child = cmd.spawn().context("starting the engine")?;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(code) = child.try_wait()? {
            bail!("the engine exited during startup ({code}).{}", log_excerpt(&log_path, log_start));
        }
        if home.read_engine_info().is_some_and(|i| i.pid == child.id())
            && let Some(c) = running(home, "cli")
        {
            return Ok((c, true));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("the engine did not start within 15 seconds.{}", log_excerpt(&log_path, log_start));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Asks the engine to shut down and waits for it to go away.
/// Returns false when no engine was running.
pub fn stop(home: &Home) -> Result<bool> {
    let Some(c) = running(home, "cli") else { return Ok(false) };
    c.client.post("/api/shutdown", serde_json::json!({}))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if c.client.get("/api/status").is_err() {
            // The engine removes engine.json itself; make sure it is gone.
            let _ = std::fs::remove_file(home.engine_file());
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the engine did not stop within 10 seconds (pid {})", c.status["pid"])
}

/// Runs the engine in the foreground. This is what `plonix start` launches.
pub fn run_foreground(home: Home, opts: &StartOptions) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PLONIX_LOG").unwrap_or_else(|_| "plonix_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let config = EngineConfig {
        home,
        project: opts.project.clone(),
        proxy_addr: SocketAddr::from(([127, 0, 0, 1], opts.proxy_port)),
        proxy_port_fallback: true,
        api_addr: SocketAddr::from(([127, 0, 0, 1], free_or_any(opts.api_port))),
        insecure_upstream: opts.insecure_upstream,
    };
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(engine::run(config))
}

/// Keeps a preferred port when it is free, else lets the OS pick one.
/// Clients find the API through `engine.json`, so any port works.
fn free_or_any(port: u16) -> u16 {
    if port != 0 && TcpListener::bind(("127.0.0.1", port)).is_ok() { port } else { 0 }
}

/// Turns a free-form name (often a host name) into a safe project name.
pub fn project_name(raw: &str) -> String {
    let name: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let name = name.trim_matches(['-', '.']).to_string();
    if name.is_empty() { "default".into() } else { name }
}

fn log_excerpt(path: &Path, from: u64) -> String {
    let text = std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b[(from as usize).min(b.len())..]).into_owned());
    match text.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => {
            let lines: Vec<&str> = t.lines().collect();
            format!("\n\n{}\n\n(full log: {})", lines[lines.len().saturating_sub(15)..].join("\n"), path.display())
        }
        _ => format!(" See {}.", path.display()),
    }
}

/// Puts the engine in its own process group so Ctrl-C in the terminal that
/// started it does not stop capture.
fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

pub fn spawn_detached(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach(cmd);
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_file_safe() {
        assert_eq!(project_name("Example.com"), "example.com");
        assert_eq!(project_name("localhost:3000"), "localhost-3000");
        assert_eq!(project_name("../etc/passwd"), "etc-passwd");
        assert_eq!(project_name("  "), "default");
    }
}
