//! `plonix callbacks`: hosts that tell you when something reached them.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::Subcommand;
use plonix_core::chromium::{Progress, Stage};
use plonix_core::{callbacks, interactsh};
use serde_json::{Value, json};

use crate::client::{ApiError, NotRunning};
use crate::render::{clip, safe};
use crate::{Ctx, open};

#[derive(Subcommand)]
pub enum CallbacksCmd {
    /// Whether interactsh-client is installed and the listener is on (the default)
    Status,
    /// Download interactsh-client (about 10 MB) into the Plonix data folder
    Install,
    /// Start listening: registers with the callback server, sends nothing to any target
    Start,
    /// Stop listening; hosts already handed out keep working once you start again
    Stop,
    /// Callbacks received, newest first
    #[command(visible_alias = "ls")]
    List {
        /// Show at most this many
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
        /// Print each callback's raw request
        #[arg(long)]
        full: bool,
    },
    /// Make a host for one test and print it
    Payload {
        /// What it is for, e.g. the request it goes into
        #[arg(long, short = 'l', default_value = "")]
        label: String,
    },
}

pub fn callbacks_cmd(ctx: &Ctx, cmd: CallbacksCmd) -> Result<()> {
    match cmd {
        CallbacksCmd::Status => status(ctx),
        CallbacksCmd::Install => install(ctx),
        CallbacksCmd::Start => {
            let c = ctx.client()?;
            let mut v = c.post("/api/callbacks/start", json!({}))?;
            // Registering takes a moment; wait for the server to hand out a host.
            let t = Instant::now();
            while v["phase"] == "starting" && t.elapsed() < Duration::from_secs(50) {
                std::thread::sleep(Duration::from_millis(500));
                v = c.get(&format!("/api/callbacks?since={}", u64::MAX))?;
            }
            if ctx.json {
                return ctx.print_json(&v);
            }
            if v["phase"] == "failed" {
                bail!("{}", v["error"].as_str().unwrap_or("the listener stopped"));
            }
            println!("{}", listener_line(&v));
            if v["phase"] == "listening" {
                println!("Make a host for a test with `plonix callbacks payload --label 'what it is for'`.");
            }
            Ok(())
        }
        CallbacksCmd::Stop => {
            let v = ctx.client()?.post("/api/callbacks/stop", json!({}))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Not listening. Hosts already handed out keep working once you start again.");
            Ok(())
        }
        CallbacksCmd::List { limit, full } => {
            let v = ctx.client()?.get("/api/callbacks?since=0")?;
            let mut list = v["interactions"].as_array().cloned().unwrap_or_default();
            list.reverse();
            list.truncate(limit);
            if ctx.json {
                return ctx.print_json(&json!(list));
            }
            if list.is_empty() {
                println!("No callbacks yet. Put a host in a request; anything that later reaches it shows up here.");
                return Ok(());
            }
            let labels: Vec<(String, String)> = v["payloads"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| (p["id"].as_str().unwrap_or("").to_string(), p["label"].as_str().unwrap_or("").to_string()))
                .collect();
            println!("{:<19}  {:<6} {:<18} {:<24} NAME", "TIME (UTC)", "PROTO", "FROM", "HOST");
            for i in &list {
                let host = i["payload"].as_str().and_then(|id| labels.iter().find(|(p, _)| p == id)).map(|(_, l)| if l.is_empty() { "Untitled host" } else { l.as_str() });
                let at = plonix_core::har::iso_time(i["at"].as_i64().unwrap_or(0));
                println!(
                    "{}",
                    safe(&format!(
                        "{:<19}  {:<6} {:<18} {:<24} {}",
                        at.get(..19).unwrap_or(&at).replace('T', " "),
                        i["protocol"].as_str().unwrap_or("").to_ascii_uppercase(),
                        clip(i["remote"].as_str().unwrap_or(""), 18),
                        clip(host.unwrap_or("-"), 24),
                        i["full_id"].as_str().unwrap_or("")
                    ))
                );
                if full {
                    println!("{}\n", safe(i["raw_request"].as_str().unwrap_or("").trim_end()));
                }
            }
            Ok(())
        }
        CallbacksCmd::Payload { label } => {
            let c = ctx.client()?;
            let p = match c.post("/api/callbacks/payloads", json!({ "label": label })) {
                Err(e) if e.downcast_ref::<ApiError>().is_some_and(|a| a.code == "not_listening") => {
                    bail!("start listening first: `plonix callbacks start`; the server gives out the hosts")
                }
                r => r?,
            };
            if ctx.json {
                return ctx.print_json(&p);
            }
            println!("{}", p["host"].as_str().unwrap_or(""));
            Ok(())
        }
    }
}

fn listener_line(v: &Value) -> String {
    let server = v["base"].as_str().and_then(|b| b.split_once('.')).map(|(_, d)| d.to_string()).unwrap_or_else(|| {
        let s = v["config"]["server"].as_str().unwrap_or("");
        if s.is_empty() { "the public servers".into() } else { s.into() }
    });
    match v["phase"].as_str().unwrap_or("") {
        "listening" => format!("Listening on {server}"),
        "starting" => format!("Connecting to {server}…"),
        "failed" => format!("Stopped: {}", v["error"].as_str().unwrap_or("the listener stopped")),
        _ => "Not listening".into(),
    }
}

fn status(ctx: &Ctx) -> Result<()> {
    let exe = callbacks::locate(&ctx.home);
    let own = interactsh::installed(&ctx.home);
    let engine = match ctx.client().and_then(|c| c.get(&format!("/api/callbacks?since={}", u64::MAX))) {
        Ok(v) => Some(v),
        Err(e) if e.downcast_ref::<NotRunning>().is_some() => None,
        // The Callbacks tool is switched off in the Market.
        Err(e) if e.downcast_ref::<ApiError>().is_some_and(|a| a.code == "not_installed") => None,
        Err(e) => return Err(e),
    };
    if ctx.json {
        let mut v = json!({
            "installed": exe.is_some(),
            "exe": exe,
            "downloaded": own.as_ref().is_some_and(|i| Some(&i.exe) == exe.as_ref()),
            "downloaded_version": own.as_ref().map(|i| &i.version),
            "can_download": interactsh::platform().is_some(),
        });
        if let Some(e) = &engine {
            for k in ["phase", "error", "base", "seq", "total", "config"] {
                v[k] = e[k].clone();
            }
            v["hosts"] = json!(e["payloads"].as_array().map_or(0, Vec::len));
        }
        return ctx.print_json(&v);
    }
    match &exe {
        Some(path) => {
            let how = match &own {
                Some(i) if &i.exe == path => format!("downloaded by Plonix, version {}", i.version),
                _ => "installed by you".into(),
            };
            println!("interactsh-client   {}\n                    {how}", open::tilde(path));
        }
        None => {
            println!("interactsh-client   not installed");
            if interactsh::platform().is_some() {
                println!("                    `plonix callbacks install` downloads it ({})", interactsh::SIZE);
            }
            println!("                    or build it with Go: {}", callbacks::INSTALL);
        }
    }
    match &engine {
        Some(v) => {
            println!("Listener            {}", listener_line(v));
            println!("Hosts               {}", v["payloads"].as_array().map_or(0, Vec::len));
            println!("Callbacks           {}", v["total"].as_u64().unwrap_or(0));
        }
        None => println!("Listener            engine not running (or Callbacks is switched off in the Market)"),
    }
    Ok(())
}

fn install(ctx: &Ctx) -> Result<()> {
    if let Some(i) = interactsh::installed(&ctx.home).filter(|i| i.version == interactsh::VERSION) {
        if ctx.json {
            return ctx.print_json(&json!(i));
        }
        println!("interactsh-client {} is already downloaded: {}", i.version, open::tilde(&i.exe));
        return Ok(());
    }
    ctx.home.ensure()?;
    let quiet = ctx.json || !std::io::stderr().is_terminal();
    let show = |p: &Progress| {
        if quiet {
            return;
        }
        let line = match p.stage {
            Stage::Looking => "Reading the release's checksums…".to_string(),
            Stage::Downloading if p.total > 0 => {
                format!("Downloading interactsh-client {}  {:>3}%  {:.1} of {:.1} MB", p.version, p.received * 100 / p.total, p.received as f64 / 1048576.0, p.total as f64 / 1048576.0)
            }
            Stage::Downloading => format!("Downloading interactsh-client {}  {:.1} MB", p.version, p.received as f64 / 1048576.0),
            Stage::Unpacking => "Unpacking…".to_string(),
            Stage::Verifying => "Checking it…".to_string(),
            Stage::Done | Stage::Failed | Stage::Idle => String::new(),
        };
        eprint!("\r\x1b[2K  {line}");
        if matches!(p.stage, Stage::Done | Stage::Failed) {
            eprint!("\r\x1b[2K");
        }
        let _ = std::io::stderr().flush();
    };
    let i = interactsh::install(&ctx.home, &show)?;
    if ctx.json {
        return ctx.print_json(&json!(i));
    }
    println!("✓ interactsh-client {} ready: {}", i.version, open::tilde(&i.exe));
    println!("  Start listening with `plonix callbacks start`, or press Start listening in Callbacks.");
    Ok(())
}
