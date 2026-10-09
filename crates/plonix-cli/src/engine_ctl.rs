//! Finding, starting and stopping background sessions.
//!
//! `plonix start` re-executes the current binary as `plonix engine`, detached
//! from the terminal, with its output in `$PLONIX_HOME/logs/engine.log`.
//! Each project runs in a session of its own, so `plonix start -p a` and
//! `plonix start -p b` run side by side. A session announces itself in
//! `sessions/<id>.json`, and in `engine.json` as the current session.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use plonix_core::paths::{EngineInfo, Home};
use plonix_core::project;
use plonix_core::session::{self, OpenOptions};
use serde_json::Value;

use crate::client::{Client, NotRunning};

#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// The project (name, id or folder). `None`: the current session, else `default`.
    pub project: Option<String>,
    pub proxy_port: Option<u16>,
    pub api_port: Option<u16>,
    pub insecure_upstream: bool,
}

/// A reachable engine and its `/api/status`.
pub struct Connected {
    pub client: Client,
    pub status: Value,
}

/// Returns the running session for `project` (or the current session), if
/// any. Cleans up a stale `engine.json` left behind by one that no longer answers.
pub fn running(home: &Home, project: Option<&str>, initiator: &str) -> Option<Connected> {
    let info = crate::client::engine_info(home, project)?;
    let client = Client::to(home, &info, initiator).ok()?;
    match client.get("/api/status") {
        Ok(status) => Some(Connected { client, status }),
        Err(e) => {
            if e.downcast_ref::<NotRunning>().is_some() && home.read_engine_info().is_some_and(|i| i.api == info.api) {
                let _ = std::fs::remove_file(home.engine_file());
            }
            None
        }
    }
}

/// Starts a session in the background unless the project already has one.
/// Without a project, an already running session is used, else `default`.
/// Returns the session and whether this call started it.
pub fn start(home: &Home, opts: &StartOptions) -> Result<(Connected, bool)> {
    if let Some(c) = running(home, opts.project.as_deref(), "cli") {
        return Ok((c, false));
    }
    home.ensure()?;
    let selector = opts.project.clone().unwrap_or_else(|| "default".into());
    // Resolve here, so a bad name or folder fails in the terminal, not in the log.
    let project = project::resolve(home, &selector)?;
    if project.is_open() {
        bail!("project '{}' is open in another Plonix session that is not answering", project.name());
    }
    let log_path = home.log_file();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_start = log.metadata().map(|m| m.len()).unwrap_or(0);

    let exe = std::env::current_exe().context("locating the plonix executable")?;
    let mut cmd = Command::new(exe);
    cmd.env("PLONIX_HOME", &home.root).arg("engine").arg("--project").arg(&project.dir);
    if let Some(port) = opts.proxy_port {
        cmd.args(["--proxy-port", &port.to_string()]);
    }
    if let Some(port) = opts.api_port {
        cmd.args(["--api-port", &port.to_string()]);
    }
    cmd.stdin(Stdio::null())
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
        if session::find(home, project.id()).is_some_and(|i| i.pid == child.id())
            && let Some(c) = running(home, Some(project.id()), "cli")
        {
            return Ok((c, true));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("the engine did not start within 15 seconds ({}).{}", not_ready(home, project.id(), child.id()), log_excerpt(&log_path, log_start));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Why a session that was started is not answering yet, for the timeout message.
fn not_ready(home: &Home, project_id: &str, pid: u32) -> String {
    let Some(info) = session::find(home, project_id) else {
        let file = session::session_file(home, project_id);
        let lock = project::resolve(home, project_id).map(|p| match p.lock() {
            Ok(_) => "the project is not locked".to_string(),
            Err(e) => e.to_string(),
        });
        return format!("no session file for the project; {} exists: {}; lock: {lock:?}", file.display(), file.exists());
    };
    if info.pid != pid {
        return format!("the session file names process {}, not {pid}", info.pid);
    }
    match Client::to(home, &info, "cli").and_then(|c| c.get("/api/status")) {
        Ok(_) => "it answers now".into(),
        Err(e) => format!("{} does not answer: {e:#}", info.api),
    }
}

/// Asks a session to close and waits for it to go away. Returns what was
/// stopped: nothing when no session was running.
pub fn stop(home: &Home, project: Option<&str>) -> Result<Option<Value>> {
    let Some(c) = running(home, project, "cli") else { return Ok(None) };
    stop_connected(home, &c)?;
    Ok(Some(c.status))
}

/// Closes every running session.
pub fn stop_all(home: &Home) -> Result<Vec<Value>> {
    let mut stopped = vec![];
    for info in session::running(home) {
        if let Ok(client) = Client::to(home, &info, "cli")
            && let Ok(status) = client.get("/api/status")
        {
            let c = Connected { client, status };
            stop_connected(home, &c)?;
            stopped.push(c.status);
        }
    }
    Ok(stopped)
}

fn stop_connected(home: &Home, c: &Connected) -> Result<()> {
    c.client.post("/api/shutdown", serde_json::json!({}))?;
    let id = c.status["project_id"].as_str().unwrap_or("").to_string();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        // Gone once it no longer answers and has withdrawn its announcement.
        if c.client.get("/api/status").is_err() && (id.is_empty() || session::find(home, &id).is_none()) {
            if home.read_engine_info().is_some_and(|i| i.project_id == id) && session::running(home).is_empty() {
                let _ = std::fs::remove_file(home.engine_file());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the session did not stop within 15 seconds (pid {})", c.status["pid"])
}

/// Runs a session in the foreground until it is stopped. This is what
/// `plonix start` launches.
pub fn run_foreground(home: Home, opts: &StartOptions) -> Result<()> {
    init_logging();
    let project = project::resolve(&home, opts.project.as_deref().unwrap_or("default"))?;
    let options = OpenOptions { proxy_port: opts.proxy_port, api_port: opts.api_port, insecure_upstream: opts.insecure_upstream, ..Default::default() };
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async move {
        let session = session::open(&home, project, options).await?;
        println!(
            "Plonix session running: proxy {} · API http://{} · project {}",
            session.proxy_addr(),
            session.api_addr,
            session.project.name()
        );
        tokio::select! {
            _ = session.stopped() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        let name = session.project.name().to_string();
        if let Some(r) = tokio::task::spawn_blocking(move || session.close()).await?? {
            match r.skipped.as_str() {
                "" => println!("{name}: kept {} in-scope exchange(s), deleted {} out of scope", r.kept, r.removed),
                why => println!("{name}: {why}"),
            }
        }
        Ok(())
    })
}

pub fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PLONIX_LOG").unwrap_or_else(|_| "plonix_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

/// A one-line description of a running session, for lists.
pub fn describe(info: &EngineInfo) -> String {
    format!("{:<24} {:<21} {}", info.project, info.proxy, info.api)
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
    plonix_core::browser::detach(cmd);
}

/// A one-time link to the Start screen. Starts it in the background
/// (`plonix hub`) unless one is already running.
pub fn launcher_url(home: &Home) -> Result<String> {
    home.ensure()?;
    let token = home.load_or_create_token()?;
    let ask = |url: &str| -> Option<String> {
        let v: Value = ureq::post(&format!("{url}/api/ui/launch"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("X-Plonix-Client", "cli")
            .timeout(Duration::from_secs(3))
            .send_json(serde_json::json!({}))
            .ok()?
            .into_json()
            .ok()?;
        v["url"].as_str().map(String::from)
    };
    if let Some(hub) = home.read_hub()
        && let Some(link) = ask(&hub.url)
    {
        return Ok(link);
    }
    let log_path = home.log_file();
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    let log_start = log.metadata().map(|m| m.len()).unwrap_or(0);
    let mut cmd = Command::new(std::env::current_exe().context("locating the plonix executable")?);
    cmd.env("PLONIX_HOME", &home.root).arg("hub").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    detach(&mut cmd);
    let mut child = cmd.spawn().context("starting the Start screen")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(code) = child.try_wait()? {
            bail!("the Start screen exited during startup ({code}).{}", log_excerpt(&log_path, log_start));
        }
        if let Some(hub) = home.read_hub().filter(|h| h.pid == child.id())
            && let Some(link) = ask(&hub.url)
        {
            return Ok(link);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("the Start screen did not start within 15 seconds.{}", log_excerpt(&log_path, log_start));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
