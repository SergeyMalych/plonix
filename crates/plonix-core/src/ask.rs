//! "Ask Claude Code": hands an agent exactly the context for one spot in the
//! app (a request, a finding, a host) as a ready-to-send prompt.
//!
//! The bundle is split into named parts so the user can see what will be
//! shared and drop parts they do not want to send. Every body is clipped,
//! and a bundle larger than the user's context budget is flagged so the app
//! asks before sending it.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::access::AgentSettings;
use crate::codec;
use crate::engine::Engine;
use crate::model::{Exchange, Headers};
use crate::paths::{Home, write_private};
use crate::scope::Decision;

/// The spot in the app the question is about.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    /// One captured request and its response.
    Request { id: i64 },
    /// A finding and the requests that prove it.
    Finding { id: i64 },
    /// A host: what it serves, its technologies and scope evidence.
    Host { host: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct AskRequest {
    #[serde(flatten)]
    pub subject: Subject,
    /// The user's question; a sensible default for the subject otherwise.
    #[serde(default)]
    pub question: Option<String>,
    /// Ids of parts to leave out.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Overrides the settings' body clip for this ask.
    #[serde(default)]
    pub max_body_chars: Option<usize>,
}

/// One piece of context the user can include or leave out.
#[derive(Debug, Clone, Serialize)]
pub struct Part {
    pub id: String,
    pub label: String,
    pub chars: usize,
    pub tokens: usize,
    pub included: bool,
    /// Whether a body in this part was clipped.
    pub clipped: bool,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    pub title: String,
    pub question: String,
    pub parts: Vec<Part>,
    /// The full prompt, with the included parts only.
    pub prompt: String,
    /// Estimated tokens of the prompt.
    pub tokens: usize,
    pub budget: usize,
    pub over_budget: bool,
    pub max_body_chars: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("{0} not found")]
    NotFound(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// A rough token estimate, good enough to warn about size.
pub fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

pub fn build(engine: &Engine, req: &AskRequest, settings: &AgentSettings) -> Result<Bundle, AskError> {
    let max_body = req.max_body_chars.unwrap_or(settings.max_body_chars).clamp(200, 100_000);
    let (title, default_q, mut parts) = match &req.subject {
        Subject::Request { id } => request_parts(engine, *id, max_body)?,
        Subject::Finding { id } => finding_parts(engine, *id, max_body)?,
        Subject::Host { host } => host_parts(engine, host)?,
    };
    for p in &mut parts {
        p.included = !req.exclude.contains(&p.id);
    }
    let question = req.question.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(String::from).unwrap_or(default_q);
    let prompt = prompt(&question, &parts);
    let tokens = estimate_tokens(prompt.chars().count());
    Ok(Bundle {
        title,
        question,
        parts,
        prompt,
        tokens,
        budget: settings.context_budget,
        over_budget: tokens > settings.context_budget,
        max_body_chars: max_body,
    })
}

const FOOTER: &str = "This comes from Plonix, the user's web security workbench, captured on their own machine while testing \
a target they are authorized to test. If the Plonix MCP tools are connected (search_traffic, get_request, get_insights, \
list_endpoints, get_scope, list_findings), use them to look further. They are read-only. Do not send traffic to the target \
yourself; suggest requests for the user to send from Plonix instead.";

fn prompt(question: &str, parts: &[Part]) -> String {
    let mut out = format!("{question}\n\n");
    for p in parts.iter().filter(|p| p.included) {
        let _ = write!(out, "## {}\n\n```\n{}\n```\n\n", p.label, p.text.trim_end());
    }
    out.push_str(FOOTER);
    out
}

fn part(id: &str, label: String, text: String, clipped: bool) -> Part {
    let chars = text.chars().count();
    Part { id: id.into(), label, chars, tokens: estimate_tokens(chars), included: true, clipped, text }
}

fn headers(h: &Headers) -> String {
    h.iter().map(|(k, v)| format!("{k}: {v}\n")).collect()
}

/// Clips to `max` characters on a char boundary.
fn clip(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => (format!("{}\n… [{} more characters not included]", &s[..i], s[i..].chars().count()), true),
        None => (s.to_string(), false),
    }
}

fn body(headers: &Headers, raw: &[u8], max: usize) -> (String, bool) {
    if raw.is_empty() {
        return (String::new(), false);
    }
    match codec::body_text(headers, raw) {
        Some(t) => {
            let (t, c) = clip(&t, max);
            (format!("\n{t}\n"), c)
        }
        None => (format!("\n<{} bytes of binary data>\n", raw.len()), false),
    }
}

pub(crate) fn request_text(ex: &Exchange, max: usize) -> (String, bool) {
    let target = if ex.query.is_empty() { ex.path.clone() } else { format!("{}?{}", ex.path, ex.query) };
    let (b, clipped) = body(&ex.req_headers, &ex.req_body, max);
    (format!("{} {} HTTP/1.1\n{}{}", ex.method, target, headers(&ex.req_headers), b), clipped)
}

pub(crate) fn response_text(ex: &Exchange, max: usize) -> (String, bool) {
    match ex.status {
        Some(s) => {
            let (b, clipped) = body(&ex.resp_headers, &ex.resp_body, max);
            (format!("HTTP/1.1 {s}\n{}{}", headers(&ex.resp_headers), b), clipped)
        }
        None => (format!("No response: {}", ex.error.as_deref().unwrap_or("unknown error")), false),
    }
}

fn scope_word(d: Decision) -> &'static str {
    match d {
        Decision::Accepted => "in scope",
        Decision::Rejected => "out of scope (rejected)",
        Decision::Unknown => "not in scope",
    }
}

fn request_parts(engine: &Engine, id: i64, max: usize) -> Result<(String, String, Vec<Part>), AskError> {
    let ex = engine.store.get_exchange(id)?.ok_or_else(|| AskError::NotFound(format!("request #{id}")))?;
    let rules = engine.rules();
    let what = format!("#{id} {} {} ({})", ex.method, ex.url(), scope_word(rules.decide(&ex.host)));
    let (req, c1) = request_text(&ex, max);
    let (resp, c2) = response_text(&ex, max);
    let mut parts = vec![part("request", format!("Request {what}"), req, c1), part("response", format!("Response to #{id}"), resp, c2)];
    let spotted = crate::insight::analyze(&ex, crate::insight::detectors());
    if !spotted.is_empty() {
        let mut t = String::new();
        for i in spotted.iter().take(20) {
            let (value, _) = clip(&i.value, 200);
            let _ = writeln!(t, "- {} in {}: {value}", i.label, i.location);
            if let Some(d) = &i.decoded {
                let (d, _) = clip(d, 600);
                let _ = writeln!(t, "  decoded: {}", d.replace('\n', "\n  "));
            }
            if !i.notes.is_empty() {
                let _ = writeln!(t, "  notes: {}", i.notes.join("; "));
            }
        }
        parts.push(part("insights", "What Plonix spotted in it".into(), t, false));
    }
    if let Ok(tech) = engine.detect_host(&ex.host)
        && !tech.is_empty()
    {
        parts.push(part("tech", format!("Technologies detected on {}", ex.host), tech_lines(&tech), false));
    }
    Ok((
        format!("Request #{id}"),
        "Look at this request and its response from my security testing. What stands out, what might be vulnerable, and what would you test next?"
            .into(),
        parts,
    ))
}

fn tech_lines(tech: &[crate::detect::Detection]) -> String {
    tech.iter()
        .map(|t| format!("- {}{} ({}, {}%)\n", t.name, t.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default(), t.category, t.confidence))
        .collect()
}

fn finding_parts(engine: &Engine, id: i64, max: usize) -> Result<(String, String, Vec<Part>), AskError> {
    let f = engine.store.findings()?.into_iter().find(|f| f.id == id).ok_or_else(|| AskError::NotFound(format!("finding #{id}")))?;
    let mut parts = vec![part(
        "finding",
        format!("Finding #{id}: {}", f.title),
        format!("Severity: {}\nStatus: {}\nEvidence: {}\n\n{}", f.severity, f.status, ids(&f.exchange_ids), f.description),
        false,
    )];
    for eid in f.exchange_ids.iter().take(10) {
        if let Some(ex) = engine.store.get_exchange(*eid)? {
            let (req, c1) = request_text(&ex, max);
            let (resp, c2) = response_text(&ex, max);
            parts.push(part(
                &format!("request-{eid}"),
                format!("Evidence #{eid}: {} {}", ex.method, ex.url()),
                format!("{req}\n----\n{resp}"),
                c1 || c2,
            ));
        }
    }
    Ok((
        format!("Finding #{id}"),
        "Review this finding from my security testing and the requests that prove it. Is it convincing, how severe is it really, and what would make the write-up stronger?".into(),
        parts,
    ))
}

fn ids(v: &[i64]) -> String {
    if v.is_empty() { "none".into() } else { v.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(", ") }
}

fn host_parts(engine: &Engine, host: &str) -> Result<(String, String, Vec<Part>), AskError> {
    let host = crate::scope::normalize_host(host.trim_start_matches("*."));
    if host.is_empty() {
        return Err(AskError::NotFound("host".into()));
    }
    let rules = engine.rules();
    let mut parts = vec![];
    let mut scope = format!("{host} is {}.\n", scope_word(rules.decide(&host)));
    let suggestion = engine.store.suggestions(&rules)?.into_iter().find(|s| s.domain.trim_start_matches("*.") == host);
    if let Some(s) = &suggestion {
        let _ = writeln!(scope, "Plonix suggests adding it to scope (score {}). Evidence:", s.score);
        for e in s.evidence.iter().take(15) {
            let (d, _) = clip(&e.detail, 160);
            let _ = writeln!(scope, "- {} (request #{}): {d}", e.summary, e.exchange_id);
        }
    }
    let accepted: Vec<String> = rules
        .rules
        .iter()
        .filter(|r| r.decision == Decision::Accepted)
        .map(|r| if r.include_subdomains { format!("*.{}", r.pattern) } else { r.pattern.clone() })
        .collect();
    let _ = write!(scope, "Accepted scope: {}", if accepted.is_empty() { "nothing yet".into() } else { accepted.join(", ") });
    parts.push(part("scope", format!("Scope for {host}"), scope, false));

    let tech = engine.detect_host(&host)?;
    if !tech.is_empty() {
        parts.push(part("tech", format!("Technologies detected on {host}"), tech_lines(&tech), false));
    }
    let eps = engine.store.endpoints(&host)?;
    if !eps.is_empty() {
        let mut t = String::new();
        for e in eps.iter().take(150) {
            let st: Vec<String> = e.statuses.iter().map(u16::to_string).collect();
            let params = if e.params.is_empty() { String::new() } else { format!("  params: {}", e.params.join(", ")) };
            let _ = writeln!(t, "{} {}  ×{}  [{}]  e.g. #{}{params}", e.method, e.path, e.requests, st.join(","), e.sample_id);
        }
        if eps.len() > 150 {
            let _ = writeln!(t, "… and {} more endpoints", eps.len() - 150);
        }
        parts.push(part("endpoints", format!("Endpoints seen on {host} ({})", eps.len()), t, eps.len() > 150));
    }
    let q = crate::query::Query::parse(&format!("host:{host}")).map_err(|e| AskError::Other(anyhow::anyhow!("{e}")))?;
    let (recent, total) = engine.store.search(&q, &rules, 25, 0)?;
    if !recent.is_empty() {
        let t: String = recent
            .iter()
            .filter(|r| r.host == host)
            .map(|r| {
                format!(
                    "#{} {} {}{} → {}\n",
                    r.id,
                    r.method,
                    r.path,
                    if r.query.is_empty() { String::new() } else { format!("?{}", r.query) },
                    r.status.map(|s| s.to_string()).unwrap_or("no response".into())
                )
            })
            .collect();
        parts.push(part("recent", format!("Most recent requests to {host} (of {total})"), t, false));
    }
    let question = if suggestion.is_some() {
        format!(
            "Does {host} belong to the web application I'm testing? Use the evidence to explain, and say whether I should accept it into scope, with or without subdomains."
        )
    } else {
        format!(
            "Here is what I've seen on {host} while testing. Summarize what it does and its attack surface, and suggest the most promising things to test."
        )
    };
    Ok((host.clone(), question, parts))
}

/// Opens Claude Code in a new Terminal window with `prompt` as its first
/// message (macOS). The prompt goes through a private file, never through
/// the shell command line, so captured text cannot run as a command.
pub fn launch_in_terminal(home: &Home, prompt: &str) -> Result<PathBuf> {
    let dir = home.root.join("claude");
    std::fs::create_dir_all(&dir)?;
    // Old hand-offs are only needed until Terminal has read them.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 3600);
            if old && e.file_name().to_string_lossy().starts_with("ask-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let file = dir.join(format!("ask-{}.txt", crate::model::now_ms()));
    write_private(&file, prompt.as_bytes())?;
    let command = terminal_command(&dir, &file);
    run_terminal(&command)?;
    Ok(file)
}

fn sh_quote(p: &std::path::Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

/// The shell line Terminal runs. Only Plonix's own paths appear in it.
pub fn terminal_command(dir: &std::path::Path, file: &std::path::Path) -> String {
    format!("cd {} && claude \"$(cat {})\"", sh_quote(dir), sh_quote(file))
}

#[cfg(target_os = "macos")]
fn run_terminal(command: &str) -> Result<()> {
    let literal = command.replace('\\', "\\\\").replace('"', "\\\"");
    let status = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &format!("tell application \"Terminal\" to do script \"{literal}\""), "-e", "tell application \"Terminal\" to activate"])
        .status()
        .context("running osascript")?;
    anyhow::ensure!(status.success(), "Terminal did not open ({status})");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn run_terminal(_command: &str) -> Result<()> {
    Err(anyhow::anyhow!("opening Claude Code in a terminal is only supported on macOS; copy the prompt instead")).context("unsupported")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_keeps_char_boundaries() {
        let (t, c) = clip("héllo wörld", 3);
        assert!(c && t.starts_with("hél\n…") && t.contains("8 more"));
        assert_eq!(clip("short", 10), ("short".into(), false));
    }

    #[test]
    fn prompt_includes_only_chosen_parts() {
        let mut parts = vec![part("a", "Part A".into(), "alpha".into(), false), part("b", "Part B".into(), "beta".into(), false)];
        parts[1].included = false;
        let p = prompt("What now?", &parts);
        assert!(p.starts_with("What now?") && p.contains("## Part A") && p.contains("alpha"));
        assert!(!p.contains("beta") && p.contains("read-only"));
    }

    #[test]
    fn terminal_command_never_carries_the_prompt() {
        let cmd = terminal_command(std::path::Path::new("/Users/o'neil/.plonix/claude"), std::path::Path::new("/tmp/ask-1.txt"));
        assert_eq!(cmd, r#"cd '/Users/o'\''neil/.plonix/claude' && claude "$(cat '/tmp/ask-1.txt')""#);
    }
}
