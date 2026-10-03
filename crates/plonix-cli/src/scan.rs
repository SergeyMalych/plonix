//! `plonix scan`: suggest a profile, list the catalog, run an active scan.

use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::Ctx;
use crate::client::encode;

#[derive(Subcommand)]
pub enum ScanCmd {
    /// Suggest a scan profile for a host from its fingerprint (sends nothing)
    Suggest {
        /// The host to profile
        host: String,
    },
    /// List the available scan detectors and tactics
    Catalog,
    /// Run an active scan against a host that is accepted into scope
    Run(RunArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// The host to scan (must be accepted into scope)
    pub host: String,
    /// Run only this tactic (repeatable); the default is the suggested profile
    #[arg(long = "tactic", value_name = "ID")]
    pub tactics: Vec<String>,
    /// Include intrusive tactics, which are off by default
    #[arg(long)]
    pub intrusive: bool,
    /// Cap how many requests the scan may send
    #[arg(long, value_name = "N")]
    pub max_requests: Option<usize>,
}

pub fn scan_cmd(ctx: &Ctx, cmd: ScanCmd) -> Result<()> {
    match cmd {
        ScanCmd::Suggest { host } => suggest(ctx, &host),
        ScanCmd::Catalog => catalog(ctx),
        ScanCmd::Run(a) => run(ctx, a),
    }
}

fn suggest(ctx: &Ctx, host: &str) -> Result<()> {
    let c = ctx.client()?;
    let v = c.get(&format!("/api/scan/suggest/{}", encode(host.trim())))?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    let signals = v["signals"].as_array().cloned().unwrap_or_default();
    if signals.is_empty() {
        println!("No signals yet for {host}. Browse the target so Plonix can fingerprint it, then try again.");
        return Ok(());
    }
    println!("Signals on {host}:");
    for s in &signals {
        println!("  {:<18} {}", s["signal"].as_str().unwrap_or(""), s["evidence"].as_str().unwrap_or(""));
    }
    print_tactics("\nRecommended (run by default):", v["recommended"].as_array());
    print_tactics("\nOptional (intrusive, off by default):", v["optional"].as_array());
    let skipped = v["skipped"].as_u64().unwrap_or(0);
    if skipped > 0 {
        println!("\n{skipped} tactic(s) not applicable to this target and skipped.");
    }
    println!("\nRun it: `plonix scan run {host}`  (add --intrusive for the optional checks).");
    Ok(())
}

fn print_tactics(heading: &str, tactics: Option<&Vec<Value>>) {
    let tactics = match tactics {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };
    println!("{heading}");
    for t in tactics {
        println!(
            "  {:<22} {:<8} {}",
            t["id"].as_str().unwrap_or(""),
            t["severity"].as_str().unwrap_or(""),
            t["title"].as_str().unwrap_or("")
        );
    }
}

fn catalog(ctx: &Ctx) -> Result<()> {
    let c = ctx.client()?;
    let v = c.get("/api/scan/catalog")?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    println!("Detectors (passive — decide what is relevant):");
    for d in v["detectors"].as_array().cloned().unwrap_or_default() {
        println!("  {:<18} -> {:<18} {}", d["id"].as_str().unwrap_or(""), d["signal"].as_str().unwrap_or(""), d["description"].as_str().unwrap_or(""));
    }
    println!("\nTactics (active checks — gated by signals):");
    for t in v["tactics"].as_array().cloned().unwrap_or_default() {
        let requires: Vec<&str> = t["requires"].as_array().map(|a| a.iter().filter_map(|r| r.as_str()).collect()).unwrap_or_default();
        println!(
            "  {:<22} {:<8} {:<10} needs [{}]  {}",
            t["id"].as_str().unwrap_or(""),
            t["severity"].as_str().unwrap_or(""),
            t["intrusiveness"].as_str().unwrap_or(""),
            requires.join(", "),
            t["title"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn run(ctx: &Ctx, a: RunArgs) -> Result<()> {
    let c = ctx.client()?;
    let body = json!({
        "host": a.host.trim(),
        "tactics": a.tactics,
        "include_intrusive": a.intrusive,
        "max_requests": a.max_requests,
    });
    let v = c.post("/api/scan", body)?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    let sent = v["requests_sent"].as_u64().unwrap_or(0);
    let ran = v["tactics_run"].as_array().map(|a| a.len()).unwrap_or(0);
    println!("Scanned {} — {ran} tactic(s), {sent} request(s) sent (all in scope).", a.host.trim());
    let findings = v["findings"].as_array().cloned().unwrap_or_default();
    if findings.is_empty() {
        println!("No findings.");
    } else {
        println!("\nFindings:");
        for f in &findings {
            println!("  #{:<5} {:<8} {}", f["id"].as_i64().unwrap_or(0), f["severity"].as_str().unwrap_or(""), f["title"].as_str().unwrap_or(""));
        }
        println!("\nSee them with `plonix show <id>` or on the Findings screen.");
    }
    for note in v["notes"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = note.as_str() {
            println!("Note: {n}");
        }
    }
    Ok(())
}
