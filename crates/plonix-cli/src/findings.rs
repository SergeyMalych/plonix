//! `plonix findings`: list, show, record, edit, close, delete and export findings.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::Ctx;
use crate::client::encode;
use crate::render::clip;

#[derive(Subcommand)]
pub enum FindingsCmd {
    /// Findings, most severe first (the default)
    List {
        /// Only these statuses, e.g. open,confirmed
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
    },
    /// One finding with its evidence requests
    Show {
        /// Finding id (the first column of `plonix findings`)
        id: i64,
    },
    /// Record a finding
    Add(AddArgs),
    /// Change a finding's title, severity or description
    Edit(EditArgs),
    /// Set where a finding stands: open, confirmed, false-positive or fixed
    Status {
        id: i64,
        /// open, confirmed, false-positive or fixed
        status: String,
    },
    /// Delete findings (the requests they point to stay)
    #[command(visible_alias = "delete")]
    Rm {
        #[arg(required = true)]
        ids: Vec<i64>,
        /// Do not ask first
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Write findings as a report with their evidence requests
    #[command(after_help = "By default the report leaves out false positives; name findings or pass --status to choose.\nExamples:\n  plonix findings export -o report.html\n  plonix findings export --format md --status confirmed,fixed\n  plonix findings export 3 7 -o two.md")]
    Export(ExportArgs),
}

#[derive(Args)]
pub struct AddArgs {
    pub title: String,
    /// info, low, medium, high or critical
    #[arg(long, short = 's', default_value = "medium")]
    pub severity: String,
    /// What happens and how to reproduce it; @file reads it from a file
    #[arg(long, short = 'd', value_name = "TEXT")]
    pub description: Option<String>,
    /// Request ids that prove it (repeatable, or comma-separated)
    #[arg(long = "request", short = 'r', value_name = "ID", value_delimiter = ',')]
    pub requests: Vec<i64>,
}

#[derive(Args)]
pub struct EditArgs {
    pub id: i64,
    #[arg(long, short = 't')]
    pub title: Option<String>,
    /// info, low, medium, high or critical
    #[arg(long, short = 's')]
    pub severity: Option<String>,
    /// New description; @file reads it from a file
    #[arg(long, short = 'd', value_name = "TEXT")]
    pub description: Option<String>,
}

#[derive(Args)]
pub struct ExportArgs {
    /// Only these findings
    pub ids: Vec<i64>,
    /// md, html or json [default: from the --output extension, else md]
    #[arg(long, short = 'f')]
    pub format: Option<String>,
    /// Write to this file instead of printing
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Only these statuses, e.g. open,confirmed
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
}

pub fn findings_cmd(ctx: &Ctx, cmd: FindingsCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        FindingsCmd::List { status } => {
            let v = c.get("/api/findings")?;
            let wanted: Vec<String> = status.iter().flat_map(|s| s.split(',')).map(normalize_status).filter(|s| !s.is_empty()).collect();
            let mut items: Vec<Value> =
                v.as_array().cloned().unwrap_or_default().into_iter().filter(|f| wanted.is_empty() || wanted.iter().any(|w| f["status"] == w.as_str())).collect();
            items.sort_by_key(|f| (std::cmp::Reverse(severity_rank(f["severity"].as_str().unwrap_or(""))), f["id"].as_i64().unwrap_or(0)));
            if ctx.json {
                return ctx.print_json(&Value::from(items));
            }
            if items.is_empty() {
                println!("{}", if wanted.is_empty() { "No findings yet. Record one with `plonix findings add <title> -r <request id>`." } else { "No findings with that status." });
                return Ok(());
            }
            println!("{:>4}  {:<8}  {:<14}  {:>4}  TITLE", "ID", "SEVERITY", "STATUS", "REQS");
            for f in &items {
                println!(
                    "{:>4}  {:<8}  {:<14}  {:>4}  {}",
                    f["id"].as_i64().unwrap_or(0),
                    f["severity"].as_str().unwrap_or(""),
                    status_label(f["status"].as_str().unwrap_or("")),
                    f["exchange_ids"].as_array().map_or(0, Vec::len),
                    clip(f["title"].as_str().unwrap_or(""), 90)
                );
            }
            println!("\n`plonix findings show <id>` for one finding · `plonix findings export -o report.html` for a report");
        }
        FindingsCmd::Show { id } => {
            let r: Value = serde_json::from_str(&c.get_text(&format!("/api/findings/export?format=json&ids={id}"))?)?;
            let Some(f) = r["findings"].get(0).cloned() else {
                return Err(crate::client::ApiError { code: "not_found".into(), message: format!("finding {id} not found") }.into());
            };
            if ctx.json {
                return ctx.print_json(&f);
            }
            print!("{}", show(&f));
        }
        FindingsCmd::Add(a) => {
            let body = json!({
                "title": a.title,
                "severity": a.severity,
                "description": read_arg(a.description)?.unwrap_or_default(),
                "exchange_ids": a.requests,
            });
            let f = c.post("/api/findings", body)?;
            if ctx.json {
                return ctx.print_json(&f);
            }
            println!("Recorded finding #{}: {}", f["id"], f["title"].as_str().unwrap_or(""));
        }
        FindingsCmd::Edit(a) => {
            if a.title.is_none() && a.severity.is_none() && a.description.is_none() {
                bail!("nothing to change: give --title, --severity or --description (and `plonix findings status` for the status)");
            }
            let body = json!({ "title": a.title, "severity": a.severity, "description": read_arg(a.description)? });
            let f = c.patch(&format!("/api/findings/{}", a.id), body)?;
            if ctx.json {
                return ctx.print_json(&f);
            }
            println!("Updated finding #{}: {} ({}, {})", f["id"], f["title"].as_str().unwrap_or(""), f["severity"].as_str().unwrap_or(""), status_label(f["status"].as_str().unwrap_or("")));
        }
        FindingsCmd::Status { id, status } => {
            let f = c.patch(&format!("/api/findings/{id}"), json!({ "status": status }))?;
            if ctx.json {
                return ctx.print_json(&f);
            }
            println!("Finding #{id} is now {}.", status_label(f["status"].as_str().unwrap_or("")));
        }
        FindingsCmd::Rm { ids, yes } => {
            if !yes && std::io::stdin().is_terminal() {
                let titles: Vec<String> =
                    ids.iter().map(|id| c.get(&format!("/api/findings/{id}")).map(|f| format!("#{id} {}", f["title"].as_str().unwrap_or("")))).collect::<Result<_>>()?;
                print!("Delete {}? The requests they point to stay. [y/N] ", titles.join(", "));
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("Nothing deleted.");
                    return Ok(());
                }
            }
            let mut deleted = vec![];
            for id in &ids {
                c.delete(&format!("/api/findings/{id}"))?;
                if !ctx.json {
                    println!("Deleted finding #{id}.");
                }
                deleted.push(*id);
            }
            if ctx.json {
                return ctx.print_json(&json!({ "deleted": deleted }));
            }
        }
        FindingsCmd::Export(a) => export(ctx, &c, a)?,
    }
    Ok(())
}

fn export(ctx: &Ctx, c: &crate::client::Client, a: ExportArgs) -> Result<()> {
    let from_ext = a.output.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let format = match (a.format, from_ext.as_deref(), ctx.json) {
        (Some(f), _, _) => f,
        (None, Some(e @ ("md" | "markdown" | "html" | "htm" | "json")), _) => e.to_string(),
        (None, _, true) => "json".into(),
        (None, _, false) => "md".into(),
    };
    let ids: Vec<String> = a.ids.iter().map(i64::to_string).collect();
    let path = format!(
        "/api/findings/export?format={}&ids={}&status={}",
        encode(&format),
        encode(&ids.join(",")),
        encode(&a.status.as_deref().map(|s| s.split(',').map(normalize_status).collect::<Vec<_>>().join(",")).unwrap_or_default())
    );
    let doc = c.get_text(&path)?;
    match a.output {
        Some(out) => {
            std::fs::write(&out, &doc).with_context(|| format!("writing {}", out.display()))?;
            if ctx.json {
                return ctx.print_json(&json!({ "path": out, "format": format, "bytes": doc.len() }));
            }
            println!("Wrote the findings report to {}.", out.display());
        }
        None => print!("{doc}"),
    }
    Ok(())
}

/// One finding from the JSON report, with its evidence, for the terminal.
fn show(f: &Value) -> String {
    let mut out = format!(
        "#{} {}\n  Severity  {}\n  Status    {}\n  Recorded  {} by {}\n",
        f["id"],
        f["title"].as_str().unwrap_or(""),
        f["severity"].as_str().unwrap_or(""),
        status_label(f["status"].as_str().unwrap_or("")),
        plonix_core::report::date(f["created_at"].as_i64().unwrap_or(0)),
        f["created_by"].as_str().unwrap_or("")
    );
    if f["updated_at"].as_i64() > f["created_at"].as_i64() {
        out.push_str(&format!("  Edited    {}\n", plonix_core::report::date(f["updated_at"].as_i64().unwrap_or(0))));
    }
    if let Some(d) = f["description"].as_str().filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!("\n{}\n", d.trim_end()));
    }
    let evidence = f["evidence"].as_array().cloned().unwrap_or_default();
    if evidence.is_empty() {
        out.push_str("\nNo evidence requests attached. Add them in the window, or record the finding again with -r <request id>.\n");
    }
    for e in &evidence {
        out.push_str("\n――――――――――――――――――――――――――――――――――――――――\n");
        if let (Some(m), Some(u)) = (e["method"].as_str(), e["url"].as_str()) {
            let status = e["status"].as_u64().map(|s| s.to_string()).unwrap_or_else(|| "no response".into());
            out.push_str(&format!("Evidence #{} {m} {u} → {status}\n\n", e["id"]));
        } else if e["id"].as_i64() != Some(0) {
            out.push_str(&format!("Evidence #{}\n", e["id"]));
        }
        if let Some(n) = e["note"].as_str() {
            out.push_str(&format!("({n})\n"));
        }
        for part in [&e["request"], &e["response"]] {
            if let Some(t) = part.as_str() {
                out.push_str(t);
                out.push_str("\n\n");
            }
        }
        if e["clipped"].as_bool() == Some(true) {
            out.push_str("(bodies clipped; see the whole exchange with `plonix show <id> --full`)\n");
        }
    }
    out
}

/// `--description @notes.md` reads the file.
fn read_arg(v: Option<String>) -> Result<Option<String>> {
    match v {
        Some(d) => match d.strip_prefix('@') {
            Some(path) => Ok(Some(std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?)),
            None => Ok(Some(d)),
        },
        None => Ok(None),
    }
}

fn normalize_status(s: &str) -> String {
    plonix_core::model::parse_status(s).map(str::to_string).unwrap_or_else(|| s.trim().to_string())
}

fn status_label(s: &str) -> String {
    s.replace('_', " ")
}

fn severity_rank(s: &str) -> usize {
    plonix_core::model::SEVERITIES.iter().position(|x| *x == s).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_spelled_as_stored() {
        assert_eq!(normalize_status("False-Positive"), "false_positive");
        assert_eq!(normalize_status(" fixed"), "fixed");
        assert_eq!(normalize_status("later"), "later", "unknown statuses reach the engine, which explains the choices");
    }

    #[test]
    fn show_prints_evidence_and_notes() {
        let f = json!({
            "id": 3, "title": "IDOR", "severity": "high", "status": "false_positive", "created_at": 0, "updated_at": 0, "created_by": "cli",
            "description": "Steps", "evidence": [
                { "id": 7, "method": "GET", "url": "https://shop.test/a", "status": 200, "request": "GET /a HTTP/1.1", "response": "HTTP/1.1 200", "clipped": true },
                { "id": 8, "clipped": false, "note": "no longer in the project's traffic" }
            ]
        });
        let out = show(&f);
        assert!(out.starts_with("#3 IDOR\n") && out.contains("Status    false positive"));
        assert!(out.contains("Evidence #7 GET https://shop.test/a → 200") && out.contains("HTTP/1.1 200") && out.contains("bodies clipped"));
        assert!(out.contains("Evidence #8\n(no longer in the project's traffic)"));
    }
}
