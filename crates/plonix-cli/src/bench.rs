//! `plonix bench`: payload runs from the command line.
//!
//! Mark positions in the URL, a header or the body with `•…•`, point one or
//! more lists at them, and `bench run` sends the whole set through the engine.
//! Every request is a Bench send, so it only reaches hosts that are accepted
//! into scope.

use std::io::Read;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::Ctx;

#[derive(Subcommand)]
pub enum BenchCmd {
    /// List the built-in payload lists
    Lists,
    /// Run payloads through the marked positions of a request (accepted hosts only)
    Run(RunArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// The request URL; mark positions with •…•, e.g. 'https://h/api?id=•1•'
    pub url: String,
    /// HTTP method
    #[arg(short = 'X', long, default_value = "GET")]
    pub method: String,
    /// A request header (repeatable); values may contain •…•
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    pub headers: Vec<String>,
    /// The request body; may contain •…•, or '@file', or '-' for stdin
    #[arg(long, value_name = "TEXT|@FILE|-")]
    pub body: Option<String>,
    /// How to spread the values: sweep (one position at a time), parallel, matrix
    #[arg(long, default_value = "sweep")]
    pub mode: String,
    /// A list for the positions (repeatable): builtin:<id>, range:A-B[:STEP],
    /// values:a,b,c, @file (one per line) or - for stdin
    #[arg(long = "list", value_name = "SPEC")]
    pub lists: Vec<String>,
    /// Send the unmodified request first, as a baseline
    #[arg(long)]
    pub base: bool,
    /// Cap how many requests the run may send
    #[arg(long, value_name = "N")]
    pub max_requests: Option<usize>,
    /// Pause between requests, in milliseconds
    #[arg(long, value_name = "MS")]
    pub delay_ms: Option<u64>,
}

pub fn bench_cmd(ctx: &Ctx, cmd: BenchCmd) -> Result<()> {
    match cmd {
        BenchCmd::Lists => lists(ctx),
        BenchCmd::Run(a) => run(ctx, a),
    }
}

fn lists(ctx: &Ctx) -> Result<()> {
    let c = ctx.client()?;
    let v = c.get("/api/run/lists")?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    println!("Built-in payload lists (use with --list builtin:<id>):");
    for l in v["lists"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {:<18} {:>6}  {}",
            l["id"].as_str().unwrap_or(""),
            l["count"].as_u64().unwrap_or(0),
            l["description"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// Builds the raw headers/body block the API expects from `--header`/`--body`.
fn raw_from(headers: &[String], body: Option<&str>) -> Result<String> {
    let mut out = String::new();
    for h in headers {
        if !h.contains(':') {
            bail!("header '{h}' is not 'Name: value'");
        }
        out.push_str(h);
        out.push('\n');
    }
    out.push('\n');
    if let Some(b) = body {
        out.push_str(&read_source(b)?);
    }
    Ok(out)
}

/// Resolves a value that may be inline text, `@file`, or `-` (stdin).
fn read_source(spec: &str) -> Result<String> {
    if spec == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("reading stdin")?;
        Ok(s)
    } else if let Some(path) = spec.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("reading {path}"))
    } else {
        Ok(spec.to_string())
    }
}

/// Parses one `--list` spec into the API's list shape.
fn parse_list(spec: &str) -> Result<Value> {
    if let Some(id) = spec.strip_prefix("builtin:") {
        return Ok(json!({ "kind": "builtin", "id": id }));
    }
    if let Some(r) = spec.strip_prefix("range:") {
        let (range, step) = match r.split_once(':') {
            Some((a, b)) => (a, b.parse::<i64>().context("range step")?),
            None => (r, 1),
        };
        let (from, to) = range.split_once('-').context("a range looks like range:1-100[:step]")?;
        return Ok(json!({ "kind": "range", "from": from.trim().parse::<i64>().context("range start")?, "to": to.trim().parse::<i64>().context("range end")?, "step": step }));
    }
    let text = if let Some(v) = spec.strip_prefix("values:") {
        v.split(',').map(|s| s.to_string()).collect::<Vec<_>>()
    } else {
        // @file or - : one value per line.
        read_source(spec)?.lines().map(|l| l.to_string()).collect()
    };
    Ok(json!({ "kind": "values", "values": text }))
}

fn run(ctx: &Ctx, a: RunArgs) -> Result<()> {
    if a.lists.is_empty() {
        bail!("choose at least one list with --list (see `plonix bench lists`)");
    }
    let lists: Vec<Value> = a.lists.iter().map(|s| parse_list(s)).collect::<Result<_>>()?;
    let body = json!({
        "method": a.method.trim(),
        "url": a.url.trim(),
        "raw": raw_from(&a.headers, a.body.as_deref())?,
        "lists": lists,
        "mode": a.mode.trim(),
        "include_base": a.base,
        "max_requests": a.max_requests,
        "delay_ms": a.delay_ms,
    });
    let c = ctx.client()?;
    let v = c.post("/api/run", body)?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    let sent = v["requests_sent"].as_u64().unwrap_or(0);
    let positions = v["positions"].as_u64().unwrap_or(0);
    println!("Ran {positions} position(s) in {} mode — {sent} request(s) sent (all in scope).", a.mode.trim());
    let rows = v["rows"].as_array().cloned().unwrap_or_default();
    if !rows.is_empty() {
        println!("\n{:<4} {:<7} {:>8} {:>8}  payload", "#", "status", "length", "ms");
        for row in &rows {
            let vals: Vec<&str> = row["values"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
            let tag = if row["baseline"].as_bool().unwrap_or(false) { " (baseline)" } else { "" };
            println!(
                "{:<4} {:<7} {:>8} {:>8}  {}{}",
                row["n"].as_u64().unwrap_or(0),
                row["status"].as_u64().map(|s| s.to_string()).unwrap_or_else(|| "ERR".into()),
                row["length"].as_u64().unwrap_or(0),
                row["duration_ms"].as_i64().unwrap_or(0),
                vals.join(" | "),
                tag,
            );
        }
        println!("\nEach request is in Traffic (search `source:replay`) and can become a finding.");
    }
    for note in v["notes"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = note.as_str() {
            println!("Note: {n}");
        }
    }
    Ok(())
}
