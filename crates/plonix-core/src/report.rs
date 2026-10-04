//! Findings as a report to hand over: Markdown, HTML or JSON, each finding
//! with the requests that prove it.
//!
//! Evidence comes from captured traffic, which the target controls, so the
//! HTML report escapes everything it prints and loads nothing from outside,
//! and the Markdown report puts requests in code fences longer than any run
//! of backticks inside them.

use std::fmt::Write as _;

use anyhow::Result;
use serde::Serialize;

use crate::ask::{request_text, response_text};
use crate::model::{Exchange, FINDING_STATUSES, Finding, SEVERITIES, now_ms, parse_status};
use crate::store::Store;

/// Each request or response body in a report is clipped to this many characters.
pub const MAX_BODY_CHARS: usize = 4000;
/// At most this many evidence requests are written out per finding.
pub const MAX_EVIDENCE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Html,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "md" | "markdown" => Some(Format::Markdown),
            "html" | "htm" => Some(Format::Html),
            "json" => Some(Format::Json),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Html => "html",
            Format::Json => "json",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Format::Markdown => "text/markdown; charset=utf-8",
            Format::Html => "text/html; charset=utf-8",
            Format::Json => "application/json",
        }
    }
}

/// Which findings go in a report.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Only these findings; empty means all.
    pub ids: Vec<i64>,
    /// Only findings with these statuses. Empty means every status when ids
    /// are given, and everything but false positives otherwise.
    pub statuses: Vec<String>,
}

impl Selection {
    /// Reads `ids=1,2` and `status=open,confirmed` style lists.
    pub fn parse(ids: &str, statuses: &str) -> Result<Self, String> {
        let ids = split(ids)
            .map(|s| s.trim_start_matches('#').parse::<i64>().map_err(|_| format!("'{s}' is not a finding id")))
            .collect::<Result<Vec<_>, _>>()?;
        let statuses = split(statuses)
            .map(|s| parse_status(s).map(str::to_string).ok_or_else(|| format!("status must be one of {}", FINDING_STATUSES.join(", "))))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { ids, statuses })
    }

    fn statuses(&self) -> Vec<String> {
        match (self.statuses.is_empty(), self.ids.is_empty()) {
            (false, _) => self.statuses.clone(),
            (true, false) => FINDING_STATUSES.iter().map(|s| s.to_string()).collect(),
            (true, true) => FINDING_STATUSES.iter().filter(|s| **s != "false_positive").map(|s| s.to_string()).collect(),
        }
    }
}

fn split(s: &str) -> impl Iterator<Item = &str> {
    s.split([',', ' ']).map(str::trim).filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub project: String,
    pub generated_at: i64,
    pub plonix_version: &'static str,
    /// The statuses the report covers.
    pub statuses: Vec<String>,
    pub findings: Vec<ReportFinding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportFinding {
    #[serde(flatten)]
    pub finding: Finding,
    pub evidence: Vec<EvidenceItem>,
}

/// One request that proves a finding, as raw HTTP text with clipped bodies.
#[derive(Debug, Clone, Serialize, Default)]
pub struct EvidenceItem {
    pub id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// A body was cut to [`MAX_BODY_CHARS`].
    pub clipped: bool,
    /// Why the request is not shown, when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Collects the selected findings with their evidence, most severe first.
/// `visible` decides which captured requests the reader may see (agents
/// limited to in-scope traffic get the others as a note).
pub fn build(store: &Store, project: &str, sel: &Selection, visible: &dyn Fn(&Exchange) -> bool) -> Result<Report> {
    let statuses = sel.statuses();
    let mut findings: Vec<Finding> = store
        .findings()?
        .into_iter()
        .filter(|f| (sel.ids.is_empty() || sel.ids.contains(&f.id)) && statuses.contains(&f.status))
        .collect();
    findings.sort_by_key(|f| (std::cmp::Reverse(severity_rank(&f.severity)), f.id));
    let mut out = Vec::with_capacity(findings.len());
    for f in findings {
        let mut evidence = Vec::new();
        for &id in f.exchange_ids.iter().take(MAX_EVIDENCE) {
            evidence.push(match store.get_exchange(id)? {
                None => EvidenceItem { id, note: Some("no longer in the project's traffic".into()), ..Default::default() },
                Some(ex) if !visible(&ex) => EvidenceItem { id, note: Some("not in scope, and agents may see in-scope traffic only".into()), ..Default::default() },
                Some(ex) => {
                    let (request, c1) = request_text(&ex, MAX_BODY_CHARS);
                    let (response, c2) = response_text(&ex, MAX_BODY_CHARS);
                    EvidenceItem {
                        id,
                        method: Some(ex.method.clone()),
                        url: Some(ex.url()),
                        status: ex.status,
                        request: Some(request.trim_end().to_string()),
                        response: Some(response.trim_end().to_string()),
                        clipped: c1 || c2,
                        note: None,
                    }
                }
            });
        }
        if f.exchange_ids.len() > MAX_EVIDENCE {
            let more = f.exchange_ids.len() - MAX_EVIDENCE;
            evidence.push(EvidenceItem { id: 0, note: Some(format!("{more} more request(s) not included")), ..Default::default() });
        }
        out.push(ReportFinding { finding: f, evidence });
    }
    Ok(Report { project: project.to_string(), generated_at: now_ms(), plonix_version: env!("CARGO_PKG_VERSION"), statuses, findings: out })
}

fn severity_rank(s: &str) -> usize {
    SEVERITIES.iter().position(|x| *x == s).unwrap_or(0)
}

/// A file name for the report, e.g. `plonix-findings-shop.md`.
pub fn file_name(project: &str, format: Format) -> String {
    let slug: String = project.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { format!("plonix-findings.{}", format.extension()) } else { format!("plonix-findings-{slug}.{}", format.extension()) }
}

pub fn render(r: &Report, format: Format) -> String {
    match format {
        Format::Markdown => markdown(r),
        Format::Html => html(r),
        Format::Json => serde_json::to_string_pretty(r).unwrap_or_default() + "\n",
    }
}

/// `2026-10-04 14:03 UTC`
pub fn date(ms: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000) {
        Ok(t) => format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute()),
        Err(_) => String::new(),
    }
}

fn status_label(s: &str) -> String {
    s.replace('_', " ")
}

/// `2 high, 1 low` over the findings in the report.
fn tally(r: &Report) -> String {
    let parts: Vec<String> = SEVERITIES
        .iter()
        .rev()
        .filter_map(|sev| {
            let n = r.findings.iter().filter(|f| f.finding.severity == *sev).count();
            (n > 0).then(|| format!("{n} {sev}"))
        })
        .collect();
    parts.join(", ")
}

fn intro(r: &Report) -> String {
    let n = r.findings.len();
    let mut s = format!("Generated by Plonix {} on {}. ", r.plonix_version, date(r.generated_at));
    if n == 0 {
        s.push_str("No findings match.");
    } else {
        let _ = write!(s, "{n} finding{}: {}.", if n == 1 { "" } else { "s" }, tally(r));
    }
    let _ = write!(s, " Statuses included: {}.", r.statuses.iter().map(|s| status_label(s)).collect::<Vec<_>>().join(", "));
    s
}

fn evidence_heading(e: &EvidenceItem) -> String {
    match (&e.method, &e.url) {
        (Some(m), Some(u)) => {
            let status = e.status.map(|s| s.to_string()).unwrap_or_else(|| "no response".into());
            format!("Request #{}: {m} {u} → {status}", e.id)
        }
        _ if e.id == 0 => String::new(),
        _ => format!("Request #{}", e.id),
    }
}

// ---- Markdown -----------------------------------------------------------

fn markdown(r: &Report) -> String {
    let mut out = format!("# Findings: {}\n\n{}\n", md(&one_line(&r.project)), intro(r));
    if !r.findings.is_empty() {
        out.push_str("\n| # | Severity | Status | Title |\n|---|---|---|---|\n");
        for f in &r.findings {
            let f = &f.finding;
            let _ = writeln!(out, "| {} | {} | {} | {} |", f.id, f.severity, status_label(&f.status), md(&one_line(&f.title)).replace('|', "\\|"));
        }
    }
    for rf in &r.findings {
        let f = &rf.finding;
        let _ = write!(out, "\n## #{} {}\n\n", f.id, md(&one_line(&f.title)));
        let _ = writeln!(out, "- **Severity:** {}", f.severity);
        let _ = writeln!(out, "- **Status:** {}", status_label(&f.status));
        let _ = write!(out, "- **Recorded:** {} by {}", date(f.created_at), md(&one_line(&f.created_by)));
        if f.updated_at > f.created_at {
            let _ = write!(out, " · last edited {}", date(f.updated_at));
        }
        out.push('\n');
        if !f.description.trim().is_empty() {
            let _ = write!(out, "\n{}\n", md(f.description.trim_end()));
        }
        if rf.evidence.is_empty() {
            out.push_str("\nNo evidence requests attached.\n");
            continue;
        }
        out.push_str("\n### Evidence\n");
        for e in &rf.evidence {
            let head = evidence_heading(e);
            if !head.is_empty() {
                let _ = write!(out, "\n#### {}\n", md(&one_line(&head)));
            }
            if let Some(note) = &e.note {
                let _ = write!(out, "\n_{note}_\n");
            }
            for text in [&e.request, &e.response].into_iter().flatten() {
                let fence = fence_for(text);
                let _ = write!(out, "\n{fence}http\n{text}\n{fence}\n");
            }
            if e.clipped {
                let _ = write!(out, "\n_Bodies are clipped to {MAX_BODY_CHARS} characters._\n");
            }
        }
    }
    out
}

/// A backtick fence longer than any run of backticks in `text`.
fn fence_for(text: &str) -> String {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

/// Text that Markdown viewers show as written: no raw HTML from titles,
/// descriptions or captured URLs, while the description's own Markdown
/// (lists, emphasis, code) still works.
fn md(s: &str) -> String {
    s.replace('<', "\\<")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- HTML ----------------------------------------------------------------

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const STYLE: &str = "\
body{font:14px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;color:#1d1d1f;background:#fff;max-width:960px;margin:0 auto;padding:32px 20px}\
h1{font-size:24px;margin:0 0 4px}h2{font-size:18px;margin:36px 0 8px;padding-top:18px;border-top:1px solid #e5e5e7}h3{font-size:14px;margin:18px 0 6px}h4{font-size:13px;margin:16px 0 6px;font-family:ui-monospace,Menlo,monospace;word-break:break-all}\
.muted{color:#6e6e73}table{border-collapse:collapse;width:100%;margin:16px 0}td,th{text-align:left;padding:6px 8px;border-bottom:1px solid #e5e5e7;vertical-align:top}\
.sev{display:inline-block;min-width:64px;text-align:center;font-size:11px;font-weight:700;text-transform:uppercase;border-radius:5px;padding:2px 6px;background:#f2f2f4;color:#48484a}\
.sev.critical{background:#c4252b;color:#fff}.sev.high{background:#fde8e8;color:#c4252b}.sev.medium{background:#fdf1dc;color:#9a5b00}.sev.low{background:#e6f0fb;color:#1f5fa8}\
.desc{white-space:pre-wrap}pre{background:#f6f6f8;border:1px solid #e5e5e7;border-radius:6px;padding:10px;overflow:auto;font:12px/1.45 ui-monospace,Menlo,monospace;white-space:pre-wrap;word-break:break-all}\
@media (prefers-color-scheme:dark){body{background:#1c1c1e;color:#f2f2f7}h2,td,th{border-color:#38383a}pre{background:#2c2c2e;border-color:#38383a}.muted{color:#98989d}.sev{background:#3a3a3c;color:#d1d1d6}}";

fn html(r: &Report) -> String {
    let mut out = String::from("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    // Nothing in the report may load or run anything.
    out.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\n");
    let _ = write!(out, "<title>Findings: {}</title>\n<style>{STYLE}</style></head><body>\n", escape(&one_line(&r.project)));
    let _ = write!(out, "<h1>Findings: {}</h1>\n<p class=\"muted\">{}</p>\n", escape(&one_line(&r.project)), escape(&intro(r)));
    if !r.findings.is_empty() {
        out.push_str("<table><thead><tr><th>#</th><th>Severity</th><th>Status</th><th>Title</th></tr></thead><tbody>\n");
        for f in &r.findings {
            let f = &f.finding;
            let _ = writeln!(
                out,
                "<tr><td><a href=\"#finding-{id}\">{id}</a></td><td><span class=\"sev {sev}\">{sev}</span></td><td>{}</td><td>{}</td></tr>",
                escape(&status_label(&f.status)),
                escape(&f.title),
                id = f.id,
                sev = escape(&f.severity)
            );
        }
        out.push_str("</tbody></table>\n");
    }
    for rf in &r.findings {
        let f = &rf.finding;
        let _ = write!(
            out,
            "<h2 id=\"finding-{}\"><span class=\"sev {sev}\">{sev}</span> #{} {}</h2>\n",
            f.id,
            f.id,
            escape(&f.title),
            sev = escape(&f.severity)
        );
        let mut meta = format!("Status: {} · Recorded {} by {}", status_label(&f.status), date(f.created_at), f.created_by);
        if f.updated_at > f.created_at {
            let _ = write!(meta, " · last edited {}", date(f.updated_at));
        }
        let _ = writeln!(out, "<p class=\"muted\">{}</p>", escape(&meta));
        if !f.description.trim().is_empty() {
            let _ = writeln!(out, "<div class=\"desc\">{}</div>", escape(f.description.trim_end()));
        }
        if rf.evidence.is_empty() {
            out.push_str("<p class=\"muted\">No evidence requests attached.</p>\n");
            continue;
        }
        out.push_str("<h3>Evidence</h3>\n");
        for e in &rf.evidence {
            let head = evidence_heading(e);
            if !head.is_empty() {
                let _ = writeln!(out, "<h4>{}</h4>", escape(&head));
            }
            if let Some(note) = &e.note {
                let _ = writeln!(out, "<p class=\"muted\">{}</p>", escape(note));
            }
            for text in [&e.request, &e.response].into_iter().flatten() {
                let _ = writeln!(out, "<pre>{}</pre>", escape(text));
            }
            if e.clipped {
                let _ = writeln!(out, "<p class=\"muted\">Bodies are clipped to {MAX_BODY_CHARS} characters.</p>");
            }
        }
    }
    out.push_str("</body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FindingEdit, NewFinding, Source};

    fn exchange(host: &str, body: &str) -> Exchange {
        Exchange {
            ts: 1,
            scheme: "https".into(),
            host: host.into(),
            port: 443,
            method: "POST".into(),
            path: "/search".into(),
            query: "q=1".into(),
            req_headers: vec![("Content-Type".into(), "text/plain".into())],
            req_body: b"needle".to_vec(),
            status: Some(200),
            resp_headers: vec![("Content-Type".into(), "text/html".into())],
            resp_body: body.as_bytes().to_vec(),
            source: Some(Source::Proxy),
            ..Default::default()
        }
    }

    fn seeded() -> Store {
        let s = Store::open_in_memory().unwrap();
        let reflected = s.insert_exchange(&exchange("shop.example.com", "<script>alert('x')</script> ``` done")).unwrap();
        let big = s.insert_exchange(&exchange("cdn.other.net", &"A".repeat(MAX_BODY_CHARS + 500))).unwrap();
        let add = |title: &str, sev: &str, ids: Vec<i64>| {
            s.add_finding(&NewFinding { title: title.into(), severity: sev.into(), description: format!("About {title} & <b>more</b>"), exchange_ids: ids }, "cli")
                .unwrap()
        };
        add("Reflected <script> in search", "medium", vec![reflected, 999]);
        add("Big response", "high", vec![big]);
        let fp = add("Not real", "critical", vec![]);
        s.update_finding(fp.id, &FindingEdit { status: Some("false_positive".into()), ..Default::default() }.checked().unwrap()).unwrap();
        s
    }

    #[test]
    fn selection_defaults_leave_out_false_positives() {
        let s = seeded();
        let r = build(&s, "Shop", &Selection::default(), &|_| true).unwrap();
        let titles: Vec<&str> = r.findings.iter().map(|f| f.finding.title.as_str()).collect();
        assert_eq!(titles, vec!["Big response", "Reflected <script> in search"], "most severe first, no false positives");
        let r = build(&s, "Shop", &Selection::parse("3", "").unwrap(), &|_| true).unwrap();
        assert_eq!(r.findings.len(), 1, "asking for a finding by id includes it whatever its status");
        let r = build(&s, "Shop", &Selection::parse("", "false-positive,fixed").unwrap(), &|_| true).unwrap();
        assert_eq!((r.findings.len(), r.statuses.clone()), (1, vec!["false_positive".to_string(), "fixed".to_string()]));
        assert!(Selection::parse("x", "").is_err() && Selection::parse("", "done").is_err());
    }

    #[test]
    fn evidence_is_clipped_and_missing_or_hidden_requests_are_noted() {
        let s = seeded();
        let r = build(&s, "Shop", &Selection::default(), &|ex| ex.host.ends_with("example.com")).unwrap();
        let big = &r.findings[0].evidence[0];
        assert!(big.request.is_none() && big.note.as_deref().unwrap().contains("in-scope traffic only"));
        let r = build(&s, "Shop", &Selection::default(), &|_| true).unwrap();
        let big = &r.findings[0].evidence[0];
        assert!(big.clipped && big.response.as_ref().unwrap().len() < MAX_BODY_CHARS + 300);
        let ev = &r.findings[1].evidence;
        assert_eq!(ev[0].url.as_deref(), Some("https://shop.example.com/search?q=1"));
        assert!(ev[0].request.as_ref().unwrap().starts_with("POST /search?q=1 HTTP/1.1") && ev[0].request.as_ref().unwrap().contains("needle"));
        assert_eq!(ev[1].note.as_deref(), Some("no longer in the project's traffic"));
    }

    #[test]
    fn html_report_escapes_everything_and_stands_alone() {
        let s = seeded();
        let out = render(&build(&s, "Shop <1>", &Selection::default(), &|_| true).unwrap(), Format::Html);
        assert!(out.starts_with("<!doctype html>") && out.contains("Content-Security-Policy"));
        assert!(!out.contains("<script") && !out.contains("<b>"), "captured or typed markup must not survive");
        assert!(out.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;") && out.contains("Shop &lt;1&gt;"));
        assert!(!out.contains("src=") && !out.contains("http-equiv=\"refresh\""));
    }

    #[test]
    fn markdown_report_fences_hold_backticks() {
        let s = seeded();
        let out = render(&build(&s, "Shop", &Selection::default(), &|_| true).unwrap(), Format::Markdown);
        assert!(out.starts_with("# Findings: Shop\n"));
        assert!(out.contains("| 2 | high | open | Big response |"));
        assert!(out.contains("## #1 Reflected \\<script> in search") && out.contains("About Big response & \\<b>more\\</b>"));
        assert!(out.contains("#### Request #1: POST https://shop.example.com/search?q=1 → 200"));
        assert!(out.contains("<script>alert('x')</script>"), "inside a code fence, captured text is shown as is");
        // The response holds ``` so its fence is four backticks long.
        assert!(out.contains("````http\nHTTP/1.1 200"));
        assert!(!out.contains("Not real"));
    }

    #[test]
    fn json_report_is_structured() {
        let s = seeded();
        let v: serde_json::Value = serde_json::from_str(&render(&build(&s, "Shop", &Selection::default(), &|_| true).unwrap(), Format::Json)).unwrap();
        assert_eq!(v["project"], "Shop");
        assert_eq!(v["findings"][1]["evidence"][0]["method"], "POST");
        assert_eq!(v["findings"][1]["status"], "open");
        assert_eq!(file_name("My Shop!", Format::Json), "plonix-findings-my-shop.json");
        assert_eq!(Format::parse("Markdown"), Some(Format::Markdown));
        assert!(date(0).starts_with("1970-01-01 00:00"));
    }
}
