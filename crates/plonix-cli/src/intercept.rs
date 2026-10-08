//! `plonix intercept`: hold requests in the proxy, then forward, edit or drop them.

use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::Ctx;

#[derive(Subcommand)]
pub enum InterceptCmd {
    /// Whether Intercept is on, its options and how many items are held (the default)
    Status,
    /// Start holding requests (in-scope hosts only, unless --everything)
    On {
        /// Hold traffic to every host, not only in-scope ones
        #[arg(long, conflicts_with = "in_scope")]
        everything: bool,
        /// Hold in-scope hosts only (the default)
        #[arg(long)]
        in_scope: bool,
        /// Only hold traffic matching this search, e.g. 'method:POST path:/api'; "" clears it
        #[arg(long, value_name = "QUERY", allow_hyphen_values = true)]
        filter: Option<String>,
        /// Hold responses too
        #[arg(long, conflicts_with = "no_responses")]
        responses: bool,
        /// Hold requests only
        #[arg(long)]
        no_responses: bool,
        /// Forward unanswered items after this many seconds
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
    },
    /// Stop holding; everything still held goes on unchanged
    Off,
    /// The held items, oldest first
    #[command(visible_alias = "ls")]
    List {
        /// Print each item's text in full
        #[arg(long)]
        full: bool,
    },
    /// Send a held item on, as it was or edited
    Forward {
        id: u64,
        /// Edit it in $EDITOR first
        #[arg(long)]
        edit: bool,
    },
    /// Drop a held item: it goes no further and the client gets an error page
    Drop { id: u64 },
    /// Send everything held on, unchanged
    ForwardAll,
}

pub fn intercept_cmd(ctx: &Ctx, cmd: InterceptCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        InterceptCmd::Status => {
            let v = c.get("/api/intercept")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            print_status(&v);
        }
        InterceptCmd::On { everything, in_scope, filter, responses, no_responses, timeout } => {
            let mut body = json!({ "on": true });
            if everything || in_scope {
                body["hold"] = json!(if everything { "everything" } else { "in_scope" });
            }
            if let Some(f) = filter {
                body["filter"] = json!(f);
            }
            if responses || no_responses {
                body["responses"] = json!(responses);
            }
            if let Some(t) = timeout {
                body["timeout_s"] = json!(t);
            }
            let v = c.put("/api/intercept", body)?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            print_status(&v);
        }
        InterceptCmd::Off => {
            let v = c.put("/api/intercept", json!({ "on": false }))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            let n = v["released"].as_u64().unwrap_or(0);
            println!("Intercept is off.{}", if n > 0 { format!(" {n} held item(s) went on unchanged.") } else { String::new() });
        }
        InterceptCmd::List { full } => {
            let v = c.get("/api/intercept")?;
            if ctx.json {
                return ctx.print_json(&v["queue"]);
            }
            let queue = v["queue"].as_array().cloned().unwrap_or_default();
            if queue.is_empty() {
                println!("Nothing is held.{}", if v["on"] == true { "" } else { " Intercept is off; turn it on with `plonix intercept on`." });
            }
            for item in &queue {
                println!("{}", crate::render::safe(&headline(item)));
                if full {
                    println!("{}\n", crate::render::safe(item["raw"].as_str().unwrap_or("")));
                }
            }
        }
        InterceptCmd::Forward { id, edit } => {
            let mut body = json!({});
            if edit {
                let v = c.get("/api/intercept")?;
                let Some(item) = v["queue"].as_array().and_then(|q| q.iter().find(|i| i["id"].as_u64() == Some(id))).cloned() else {
                    bail!("held item {id} not found; it may have gone on already");
                };
                if let Some(note) = item["note"].as_str() {
                    eprintln!("{note}");
                }
                body["raw"] = json!(edit_text(item["raw"].as_str().unwrap_or(""))?);
            }
            let v = c.post(&format!("/api/intercept/{id}/forward"), body)?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Forwarded {id}{}.", if edit { " as edited" } else { "" });
        }
        InterceptCmd::Drop { id } => {
            let v = c.post(&format!("/api/intercept/{id}/drop"), json!({}))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Dropped {id}.");
        }
        InterceptCmd::ForwardAll => {
            let v = c.post("/api/intercept/forward-all", json!({}))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Forwarded {} item(s).", v["forwarded"].as_u64().unwrap_or(0));
        }
    }
    Ok(())
}

fn print_status(v: &Value) {
    let on = v["on"] == true;
    let held = v["queue"].as_array().map_or(0, Vec::len);
    println!("Intercept is {}.", if on { "on" } else { "off" });
    println!("  hold       {}", if v["hold"] == "everything" { "everything" } else { "in-scope hosts only" });
    let filter = v["filter"].as_str().unwrap_or("");
    println!("  filter     {}", if filter.is_empty() { "(none)" } else { filter });
    println!("  responses  {}", if v["responses"] == true { "held too" } else { "not held" });
    println!("  timeout    {} s, then items go on unchanged", v["timeout_s"].as_u64().unwrap_or(0));
    println!("  held now   {held}");
}

fn headline(item: &Value) -> String {
    let kind = item["kind"].as_str().unwrap_or("");
    let status = item["status"].as_u64().map(|s| format!(" → {s}")).unwrap_or_default();
    let left = (item["expires_at"].as_i64().unwrap_or(0) - plonix_core::model::now_ms()).max(0) / 1000;
    format!(
        "{:>4}  {:<8} {:<6} {}{}  ({left}s left{})",
        item["id"].as_u64().unwrap_or(0),
        kind,
        item["method"].as_str().unwrap_or(""),
        item["url"].as_str().unwrap_or(""),
        status,
        if item["body_editable"] == false { ", head only" } else { "" }
    )
}

/// Opens the text in $VISUAL or $EDITOR and returns what was saved.
fn edit_text(text: &str) -> Result<String> {
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("plonix-intercept-{}.http", std::process::id()));
    std::fs::File::create(&path)?.write_all(text.as_bytes())?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .with_context(|| format!("running {editor}"))?;
    let edited = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    if !status.success() {
        bail!("{editor} exited with {status}; nothing was forwarded");
    }
    Ok(edited?)
}
