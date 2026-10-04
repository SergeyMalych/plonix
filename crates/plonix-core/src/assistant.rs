//! The in-app "Ask Claude Code" conversation: runs the `claude` CLI headless
//! on the user's own machine and streams its answer straight back into Plonix,
//! so the user never has to leave the app or open a terminal to get help with
//! what they are looking at.
//!
//! The run is wired to the same read-only Plonix MCP server that
//! `plonix connect claude` registers, so Claude can look at the live project
//! while it answers — but only through the agent's read-only capabilities
//! (see [`crate::access`]). Nothing here can send traffic or change anything.
//!
//! A conversation is driven by polling: [`start`](Conversations::start) spawns
//! `claude -p` and returns a run id, a background task parses its streamed JSON
//! into [`Event`]s, and the UI calls [`poll`](Conversations::poll) for whatever
//! is new. Follow-up turns resume the same Claude session with
//! `--resume <session_id>`, so the conversation keeps its context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::paths::Home;

/// How far a single answer may run before it is cut off, a guard against a
/// stuck or runaway `claude` process holding a child open forever.
const MAX_SECS: u64 = 300;

/// Finished conversations kept around for late polls; older ones are dropped.
const KEEP_FINISHED: usize = 32;

/// One live or finished Claude Code conversation.
pub struct Run {
    events: Vec<Event>,
    status: Status,
    /// Claude's session id, set when the turn finishes; used to resume.
    session_id: Option<String>,
    /// Aborting this ends the background task, which kills the child.
    abort: Option<tokio::task::AbortHandle>,
    /// Monotonic id for pruning finished runs oldest-first.
    ord: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// `claude` is still working.
    Running,
    /// The turn finished; a follow-up may resume it.
    Done,
    /// `claude` failed or was cancelled; see the last [`Event`].
    Error,
}

/// A piece of the conversation the UI can render as it arrives.
#[derive(Clone, Serialize)]
pub struct Event {
    pub seq: usize,
    #[serde(rename = "type")]
    pub kind: Kind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Assistant text to show in the panel.
    Text,
    /// Claude used a tool (e.g. read some traffic); `text` names it.
    Tool,
    /// Something went wrong; `text` explains it.
    Error,
}

/// What the UI reads on each poll: whatever is new since `since`, plus state.
#[derive(Serialize)]
pub struct Snapshot {
    pub events: Vec<Event>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Why a conversation could not be started.
pub enum StartError {
    /// The `claude` command is not installed (or not on the PATH).
    NoCli,
}

/// All conversations for this engine, keyed by run id.
#[derive(Default)]
pub struct Conversations {
    runs: Mutex<HashMap<String, std::sync::Arc<Mutex<Run>>>>,
    next: AtomicU64,
}

impl Conversations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `claude` can be found; the UI shows a clear state otherwise.
    pub fn cli_available() -> bool {
        claude_bin().is_some()
    }

    /// Starts a new turn. With `resume`, it continues that Claude session so
    /// the follow-up keeps the earlier context. Returns the run id to poll.
    pub fn start(&self, home: &Home, prompt: String, resume: Option<String>) -> Result<String, StartError> {
        let bin = claude_bin().ok_or(StartError::NoCli)?;
        let ord = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("c{ord}");

        let run = std::sync::Arc::new(Mutex::new(Run { events: Vec::new(), status: Status::Running, session_id: None, abort: None, ord }));
        let mcp = mcp_config(home);
        let cwd = run_dir(home);
        let task_run = run.clone();
        let handle = tokio::spawn(async move { drive(bin, mcp, cwd, prompt, resume, task_run).await });
        run.lock().unwrap().abort = Some(handle.abort_handle());

        let mut map = self.runs.lock().unwrap();
        map.insert(id.clone(), run);
        prune_finished(&mut map);
        Ok(id)
    }

    /// Events from `id` with `seq >= since`, plus the run's current state.
    pub fn poll(&self, id: &str, since: usize) -> Option<Snapshot> {
        let run = self.runs.lock().unwrap().get(id).cloned()?;
        let run = run.lock().unwrap();
        Some(Snapshot {
            events: run.events.iter().filter(|e| e.seq >= since).cloned().collect(),
            status: run.status,
            session_id: run.session_id.clone(),
        })
    }

    /// Stops a running conversation; the child process is killed on drop.
    pub fn cancel(&self, id: &str) -> bool {
        let Some(run) = self.runs.lock().unwrap().get(id).cloned() else { return false };
        let mut run = run.lock().unwrap();
        if run.status == Status::Running {
            if let Some(a) = run.abort.take() {
                a.abort();
            }
            push(&mut run, Kind::Error, "The conversation was stopped.".into());
            run.status = Status::Error;
        }
        true
    }
}

/// Drops finished runs beyond [`KEEP_FINISHED`], oldest first, so a long
/// session does not grow this map without bound.
fn prune_finished(map: &mut HashMap<String, std::sync::Arc<Mutex<Run>>>) {
    let mut finished: Vec<(u64, String)> = map
        .iter()
        .filter_map(|(k, v)| {
            let r = v.lock().unwrap();
            (r.status != Status::Running).then(|| (r.ord, k.clone()))
        })
        .collect();
    if finished.len() <= KEEP_FINISHED {
        return;
    }
    finished.sort_by_key(|(ord, _)| *ord);
    for (_, k) in finished.iter().take(finished.len() - KEEP_FINISHED) {
        map.remove(k);
    }
}

fn push(run: &mut Run, kind: Kind, text: String) {
    let seq = run.events.len();
    run.events.push(Event { seq, kind, text });
}

/// Runs `claude -p` and streams its output into `run` until it exits.
async fn drive(bin: PathBuf, mcp: Value, cwd: PathBuf, prompt: String, resume: Option<String>, run: std::sync::Arc<Mutex<Run>>) {
    let mut cmd = Command::new(&bin);
    cmd.arg("-p")
        .arg(&prompt)
        .args(["--output-format", "stream-json", "--verbose"])
        .arg("--mcp-config")
        .arg(mcp.to_string())
        // The Plonix MCP server is read-only by design; allow all its tools
        // and nothing else, so `claude` never stops to ask about a tool.
        .args(["--allowedTools", "mcp__plonix"]);
    if let Some(sid) = &resume {
        cmd.args(["--resume", sid]);
    }
    // Run in a dedicated empty directory, never the app's working directory
    // (which inside Plonix.app is the bundle or `/`). Otherwise `claude`
    // scans the current folder for context and macOS throws privacy prompts
    // for Photos, Downloads and the like.
    cmd.current_dir(&cwd);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return finish_err(&run, format!("Could not start Claude Code: {e}")),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Collect stderr in the background so it is ready if the run fails.
    let err_task = stderr.map(|e| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                buf.push_str(&l);
                buf.push('\n');
            }
            buf
        })
    });

    let deadline = tokio::time::sleep(tokio::time::Duration::from_secs(MAX_SECS));
    tokio::pin!(deadline);

    if let Some(out) = stdout {
        let mut lines = BufReader::new(out).lines();
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(l)) => on_line(&run, &l),
                    _ => break,
                },
                _ = &mut deadline => {
                    finish_err(&run, "Claude Code took too long and was stopped.".into());
                    return;
                }
            }
        }
    }

    let status = child.wait().await;
    let ok = matches!(&status, Ok(s) if s.success());
    if ok {
        let mut r = run.lock().unwrap();
        if r.status == Status::Running {
            r.status = Status::Done;
        }
    } else {
        let stderr = match err_task {
            Some(t) => t.await.unwrap_or_default(),
            None => String::new(),
        };
        finish_err(&run, error_text(&stderr, &status));
    }
}

/// Parses one line of `claude`'s stream-json output into events.
fn on_line(run: &std::sync::Arc<Mutex<Run>>, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let mut r = run.lock().unwrap();
            if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) {
                                push(&mut r, Kind::Text, t.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("a tool");
                            push(&mut r, Kind::Tool, friendly_tool(name));
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("result") => {
            let mut r = run.lock().unwrap();
            if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                r.session_id = Some(sid.to_string());
            }
            // A subtype other than "success" means the turn ended badly.
            if v.get("subtype").and_then(Value::as_str).is_some_and(|s| s != "success") {
                let msg = v.get("result").and_then(Value::as_str).unwrap_or("Claude Code could not finish the turn.");
                push(&mut r, Kind::Error, msg.to_string());
                r.status = Status::Error;
            }
        }
        _ => {}
    }
}

fn finish_err(run: &std::sync::Arc<Mutex<Run>>, msg: String) {
    let mut r = run.lock().unwrap();
    if r.status == Status::Running {
        push(&mut r, Kind::Error, msg);
        r.status = Status::Error;
    }
}

/// A short, human label for a Plonix MCP tool name like `mcp__plonix__traffic`.
fn friendly_tool(name: &str) -> String {
    let bare = name.rsplit("__").next().unwrap_or(name).replace('_', " ");
    format!("Looked at {bare}")
}

/// Turns a child's failure into a message the user can act on.
fn error_text(stderr: &str, status: &std::io::Result<std::process::ExitStatus>) -> String {
    let s = stderr.trim();
    let low = s.to_lowercase();
    if low.contains("log in") || low.contains("login") || low.contains("not authenticated") || low.contains("unauthorized") || low.contains("api key") {
        return "Claude Code is not signed in on this machine. Open a terminal and run `claude`, sign in once, then try again.".into();
    }
    if !s.is_empty() {
        // Keep it short; the panel is not a log viewer.
        let brief: String = s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(s).chars().take(300).collect();
        return format!("Claude Code stopped: {brief}");
    }
    match status {
        Ok(st) => format!("Claude Code exited without finishing ({st})."),
        Err(e) => format!("Claude Code could not be run: {e}"),
    }
}

/// A dedicated, app-owned empty directory to run `claude` in, so it never
/// scans the user's files (which triggers macOS privacy prompts).
fn run_dir(home: &Home) -> PathBuf {
    let dir = home.root.join("claude").join("run");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The `--mcp-config` payload wiring the read-only Plonix MCP server. The
/// command is this executable run as `<exe> mcp`: the `plonix` CLI serves it
/// via `Cmd::Mcp`, and the desktop app via a headless entry point in `main`
/// (so it never opens a window).
fn mcp_config(home: &Home) -> Value {
    let exe = std::env::current_exe().ok().map(|e| e.canonicalize().unwrap_or(e));
    let command = exe.map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "plonix".into());
    let mut server = json!({ "type": "stdio", "command": command, "args": ["mcp"] });
    if !is_default_home(home) {
        server["env"] = json!({ "PLONIX_HOME": home.root });
    }
    json!({ "mcpServers": { "plonix": server } })
}

/// Whether `home` is the standard `~/.plonix`, in which case `plonix mcp` finds
/// it on its own and needs no `PLONIX_HOME` in the environment.
fn is_default_home(home: &Home) -> bool {
    std::env::var_os("PLONIX_HOME").is_none() && std::env::var_os("HOME").map(PathBuf::from).map(|h| h.join(".plonix")) == Some(home.root.clone())
}

/// `$PLONIX_CLAUDE` (for tests), else `claude` on the PATH.
fn claude_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PLONIX_CLAUDE") {
        return Some(p.into());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("claude")).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_tool_names_read_nicely() {
        assert_eq!(friendly_tool("mcp__plonix__traffic"), "Looked at traffic");
        assert_eq!(friendly_tool("mcp__plonix__scope_suggest"), "Looked at scope suggest");
    }

    #[test]
    fn not_signed_in_is_detected() {
        let msg = error_text("Error: Please log in with `claude`", &Ok(fake_status()));
        assert!(msg.contains("not signed in"));
    }

    #[test]
    fn stderr_is_summarized() {
        let msg = error_text("warming up\nsomething broke", &Ok(fake_status()));
        assert!(msg.contains("something broke") && !msg.contains("warming up"));
    }

    fn fake_status() -> std::process::ExitStatus {
        // A non-success status; the value is irrelevant to these tests.
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }

    #[test]
    fn parses_assistant_text_and_tools() {
        let run = std::sync::Arc::new(Mutex::new(Run { events: Vec::new(), status: Status::Running, session_id: None, abort: None, ord: 0 }));
        on_line(&run, r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","name":"mcp__plonix__hosts"}]}}"#);
        on_line(&run, r#"{"type":"result","subtype":"success","session_id":"abc123"}"#);
        let r = run.lock().unwrap();
        assert_eq!(r.events.len(), 2);
        assert_eq!(r.session_id.as_deref(), Some("abc123"));
    }
}
