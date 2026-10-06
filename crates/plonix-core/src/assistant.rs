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
//! is new, along with a live [`Progress`] (the current step, tokens read and
//! written so far, elapsed time) so the UI is never a silent spinner while
//! Claude works. Follow-up turns resume the same Claude session with
//! `--resume <session_id>`, so the conversation keeps its context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::paths::Home;

/// How far a single answer may run before it is cut off, a guard against a
/// stuck or runaway `claude` process holding a child open forever.
const MAX_SECS: u64 = 300;

/// How long `claude` may go without printing anything before it is treated
/// as stuck. While it thinks or writes it streams constantly, so a long
/// silence means it has stalled (e.g. waiting on the network).
const QUIET_SECS: u64 = 120;

/// The longest live draft kept for the UI; the final text arrives whole.
const MAX_DRAFT: usize = 16 * 1024;

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
    progress: Progress,
}

impl Run {
    fn new(ord: u64) -> Self {
        Run { events: Vec::new(), status: Status::Running, session_id: None, abort: None, ord, progress: Progress::new() }
    }
}

/// What Claude is doing right now, kept up to date from its streamed output.
struct Progress {
    started: Instant,
    /// When `claude` last printed anything.
    heard: Instant,
    /// Set when the run ends, so elapsed time stops counting.
    finished: Option<Instant>,
    step: String,
    /// The size of what Claude is reading: the context of its latest call.
    tokens_in: u64,
    /// Tokens written by calls that have finished.
    out_done: u64,
    /// Tokens written so far by the call in flight (estimated until it ends).
    out_cur: u64,
    /// Characters streamed in the call in flight, for the estimate above.
    chars_cur: u64,
    /// Answer text as it is being written, before the finished block lands.
    draft: String,
    /// The last tool Claude called, to say whose result it is reading.
    tool: String,
}

impl Progress {
    fn new() -> Self {
        let now = Instant::now();
        Progress { started: now, heard: now, finished: None, step: "Starting Claude Code".into(), tokens_in: 0, out_done: 0, out_cur: 0, chars_cur: 0, draft: String::new(), tool: String::new() }
    }

    fn view(&self) -> ProgressView {
        let end = self.finished.unwrap_or_else(Instant::now);
        ProgressView {
            step: self.step.clone(),
            elapsed_ms: end.duration_since(self.started).as_millis() as u64,
            idle_ms: if self.finished.is_some() { 0 } else { self.heard.elapsed().as_millis() as u64 },
            tokens_in: self.tokens_in,
            tokens_out: self.out_done + self.out_cur,
            draft: self.draft.clone(),
        }
    }
}

/// The live progress the UI shows under the conversation while it runs.
#[derive(Serialize)]
pub struct ProgressView {
    /// A short phrase for the current step, e.g. "Thinking" or "Using traffic".
    pub step: String,
    pub elapsed_ms: u64,
    /// How long since `claude` last said anything; a long gap means it is slow.
    pub idle_ms: u64,
    /// Tokens Claude is reading (its current context).
    pub tokens_in: u64,
    /// Tokens Claude has written so far, thinking included.
    pub tokens_out: u64,
    /// The answer as it is being written; empty when nothing is in flight.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub draft: String,
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
    /// Claude suggested an edit to the Bench draft; the Bench shows it for
    /// the user to apply or discard.
    Proposal,
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
    pub progress: ProgressView,
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
        // Unique across engine restarts, since saved chats remember it.
        let id = format!("c{}-{ord}", started_tag());

        let run = std::sync::Arc::new(Mutex::new(Run::new(ord)));
        let mcp = mcp_config(home, &id);
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
            progress: run.progress.view(),
        })
    }

    /// What a finished run produced, for saving it with its chat. `None` for
    /// an unknown run, `Some(None)` while it is still running.
    pub fn outcome(&self, id: &str) -> Option<Option<crate::chats::Outcome>> {
        let run = self.runs.lock().unwrap().get(id).cloned()?;
        let run = run.lock().unwrap();
        if run.status == Status::Running {
            return Some(None);
        }
        let of = |k: fn(&Kind) -> bool| run.events.iter().filter(move |e| k(&e.kind)).map(|e| e.text.clone());
        Some(Some(crate::chats::Outcome {
            answer: of(|k| matches!(k, Kind::Text)).collect::<Vec<_>>().join("\n\n"),
            tools: of(|k| matches!(k, Kind::Tool | Kind::Proposal)).collect(),
            error: of(|k| matches!(k, Kind::Error)).next_back().unwrap_or_default(),
            ok: run.status == Status::Done,
            session_id: run.session_id.clone(),
        }))
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
            end(&mut run, Status::Error, "Stopped");
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

/// Marks the run finished and freezes its progress.
fn end(run: &mut Run, status: Status, step: &str) {
    run.status = status;
    let p = &mut run.progress;
    p.finished = Some(Instant::now());
    p.step = step.into();
    p.draft.clear();
    p.out_done += p.out_cur;
    p.out_cur = 0;
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
    // Token-by-token output, so the panel can show Claude thinking and
    // writing instead of sitting silent until a whole block is done.
    if partial_messages(&bin).await {
        cmd.arg("--include-partial-messages");
    }
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
            let quiet = tokio::time::sleep(tokio::time::Duration::from_secs(QUIET_SECS));
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(l)) => on_line(&run, &l),
                    _ => break,
                },
                _ = &mut deadline => {
                    finish_err(&run, format!("Claude Code was still working after {} minutes, so it was stopped. Try a narrower question or share less.", MAX_SECS / 60));
                    return;
                }
                _ = quiet => {
                    finish_err(&run, format!("Claude Code stopped responding (nothing for {} minutes), so it was stopped. Check your connection and that `claude` works in a terminal, then try again.", QUIET_SECS / 60));
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
            end(&mut r, Status::Done, "Done");
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
    let mut r = run.lock().unwrap();
    r.progress.heard = Instant::now();
    match v.get("type").and_then(Value::as_str) {
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            r.progress.step = "Connected, sending your question".into();
        }
        Some("stream_event") => {
            if let Some(ev) = v.get("event") {
                on_stream(&mut r.progress, ev);
            }
        }
        Some("user") => {
            // Tool results flow back to Claude as a user message.
            let has_result = v.pointer("/message/content").and_then(Value::as_array).is_some_and(|c| c.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")));
            if has_result {
                let p = &mut r.progress;
                p.step = if p.tool.is_empty() { "Reading the results".into() } else { format!("Reading {}", p.tool) };
            }
        }
        Some("assistant") => {
            if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) {
                                push(&mut r, Kind::Text, t.to_string());
                            }
                            // The finished block replaces its live draft.
                            r.progress.draft.clear();
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("a tool");
                            if name.ends_with("propose_bench_edit") {
                                push(&mut r, Kind::Proposal, "Suggested an edit to the request".into());
                            } else {
                                push(&mut r, Kind::Tool, friendly_tool(name));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("result") => {
            // The final tally is exact; prefer it to the running estimate.
            if let Some(out) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                let p = &mut r.progress;
                p.out_done = (p.out_done + p.out_cur).max(out);
                p.out_cur = 0;
            }
            if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                r.session_id = Some(sid.to_string());
            }
            // A subtype other than "success" means the turn ended badly.
            if v.get("subtype").and_then(Value::as_str).is_some_and(|s| s != "success") {
                let msg = v.get("result").and_then(Value::as_str).unwrap_or("Claude Code could not finish the turn.");
                push(&mut r, Kind::Error, msg.to_string());
                end(&mut r, Status::Error, "Stopped");
            }
        }
        _ => {}
    }
}

/// Follows one raw API streaming event: what Claude is doing and how many
/// tokens it has read and written.
fn on_stream(p: &mut Progress, ev: &Value) {
    match ev.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            p.out_done += p.out_cur;
            p.out_cur = 0;
            p.chars_cur = 0;
            if let Some(u) = ev.pointer("/message/usage") {
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                p.tokens_in = n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
                p.out_cur = n("output_tokens");
            }
            p.step = "Reading what you shared".into();
        }
        Some("content_block_start") => {
            let block = ev.get("content_block");
            p.step = match block.and_then(|b| b.get("type")).and_then(Value::as_str) {
                Some("thinking") | Some("redacted_thinking") => "Thinking".into(),
                Some("tool_use") => {
                    let name = block.and_then(|b| b.get("name")).and_then(Value::as_str).unwrap_or("a tool");
                    p.tool = tool_label(name);
                    if name.ends_with("propose_bench_edit") { "Drafting an edit to the request".into() } else { format!("Looking at {}", p.tool) }
                }
                _ => "Writing the answer".into(),
            };
        }
        Some("content_block_delta") => {
            let d = ev.get("delta");
            let piece = ["text", "thinking", "partial_json"].iter().find_map(|k| d.and_then(|d| d.get(*k)).and_then(Value::as_str)).unwrap_or("");
            p.chars_cur += piece.len() as u64;
            // About four characters to a token, until the real count lands.
            p.out_cur = p.out_cur.max(p.chars_cur.div_ceil(4));
            if d.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta") && p.draft.len() < MAX_DRAFT {
                p.draft.push_str(piece);
            }
        }
        Some("message_delta") => {
            if let Some(out) = ev.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                p.out_cur = p.out_cur.max(out);
            }
        }
        _ => {}
    }
}

fn finish_err(run: &std::sync::Arc<Mutex<Run>>, msg: String) {
    let mut r = run.lock().unwrap();
    if r.status == Status::Running {
        push(&mut r, Kind::Error, msg);
        end(&mut r, Status::Error, "Stopped");
    }
}

/// A short, human label for a Plonix MCP tool name like `mcp__plonix__traffic`.
fn friendly_tool(name: &str) -> String {
    format!("Looked at {}", tool_label(name))
}

/// `mcp__plonix__scope_suggest` → `scope suggest`.
fn tool_label(name: &str) -> String {
    name.rsplit("__").next().unwrap_or(name).replace('_', " ")
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

/// A short tag for when this engine started, so run ids from different
/// sessions never collide.
fn started_tag() -> &'static str {
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TAG.get_or_init(|| format!("{:x}", crate::model::now_ms() / 1000))
}

/// The `--mcp-config` payload wiring the read-only Plonix MCP server. The
/// command is this executable run as `<exe> mcp`: the `plonix` CLI serves it
/// via `Cmd::Mcp`, and the desktop app via a headless entry point in `main`
/// (so it never opens a window). The server names itself after the run, so
/// the Agents screen can show what each conversation looked at.
fn mcp_config(home: &Home, run: &str) -> Value {
    let exe = std::env::current_exe().ok().map(|e| e.canonicalize().unwrap_or(e));
    let command = exe.map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "plonix".into());
    let mut env = json!({ crate::mcp::CLIENT_ENV: format!("{}/{run}", crate::mcp::ASK_CLIENT) });
    if !is_default_home(home) {
        env["PLONIX_HOME"] = json!(home.root);
    }
    json!({ "mcpServers": { "plonix": { "type": "stdio", "command": command, "args": ["mcp"], "env": env } } })
}

/// Whether `home` is the standard `~/.plonix`, in which case `plonix mcp` finds
/// it on its own and needs no `PLONIX_HOME` in the environment.
fn is_default_home(home: &Home) -> bool {
    std::env::var_os("PLONIX_HOME").is_none() && std::env::var_os("HOME").map(PathBuf::from).map(|h| h.join(".plonix")) == Some(home.root.clone())
}

/// Whether this `claude` can stream token by token. Older versions reject the
/// flag outright, which would fail every run, so ask its help text once.
async fn partial_messages(bin: &PathBuf) -> bool {
    static SUPPORTED: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *SUPPORTED
        .get_or_init(|| async {
            let out = Command::new(bin).arg("--help").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).output();
            matches!(tokio::time::timeout(tokio::time::Duration::from_secs(15), out).await, Ok(Ok(o)) if String::from_utf8_lossy(&o.stdout).contains("--include-partial-messages"))
        })
        .await
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
        let run = std::sync::Arc::new(Mutex::new(Run::new(0)));
        on_line(&run, r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","name":"mcp__plonix__hosts"}]}}"#);
        on_line(&run, r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__plonix__propose_bench_edit","input":{}}]}}"#);
        on_line(&run, r#"{"type":"result","subtype":"success","session_id":"abc123"}"#);
        let r = run.lock().unwrap();
        assert_eq!(r.events.len(), 3);
        assert!(matches!(r.events[2].kind, Kind::Proposal));
        assert_eq!(r.session_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn tracks_progress_from_the_stream() {
        let run = std::sync::Arc::new(Mutex::new(Run::new(0)));
        let step = || run.lock().unwrap().progress.view();
        assert_eq!(step().step, "Starting Claude Code");
        on_line(&run, r#"{"type":"system","subtype":"init"}"#);
        on_line(&run, r#"{"type":"stream_event","event":{"type":"message_start","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":30000,"cache_creation_input_tokens":8000,"output_tokens":1}}}}"#);
        assert_eq!(step().tokens_in, 38002);
        on_line(&run, r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}}"#);
        assert_eq!(step().step, "Thinking");
        on_line(&run, r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"0123456789abcdef0123456789abcdef"}}}"#);
        assert_eq!(step().tokens_out, 8);
        assert!(step().draft.is_empty(), "thinking is not part of the answer");
        on_line(&run, r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"mcp__plonix__traffic"}}}"#);
        assert_eq!(step().step, "Looking at traffic");
        on_line(&run, r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":40}}}"#);
        on_line(&run, r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#);
        assert_eq!(step().step, "Reading traffic");
        on_line(&run, r#"{"type":"stream_event","event":{"type":"message_start","message":{"usage":{"input_tokens":900,"cache_read_input_tokens":38000,"output_tokens":1}}}}"#);
        on_line(&run, r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}}}"#);
        on_line(&run, r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The login "}}}"#);
        let v = step();
        assert_eq!((v.step.as_str(), v.tokens_in, v.tokens_out, v.draft.as_str()), ("Writing the answer", 38900, 43, "The login "));
        on_line(&run, r#"{"type":"assistant","message":{"content":[{"type":"text","text":"The login form has no rate limit."}]}}"#);
        assert!(step().draft.is_empty(), "the finished text replaces the draft");
        on_line(&run, r#"{"type":"result","subtype":"success","session_id":"s","usage":{"output_tokens":60}}"#);
        assert_eq!(step().tokens_out, 60);
    }
}
