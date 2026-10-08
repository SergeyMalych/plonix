//! `plonix program`: follow a bug bounty or disclosure program, so its scope
//! and rules of engagement apply to the project.

use anyhow::{Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::Ctx;

#[derive(Subcommand)]
pub enum ProgramCmd {
    /// The program this project follows, with its scope and rules (the default)
    Show,
    /// Read a program and, with --yes, follow it. SOURCE is `<platform>:<handle>`
    /// (hackerone:acme), a policy file, a policy page's https address, or a domain with a security.txt
    Follow {
        source: String,
        /// The program's name, when Plonix cannot tell
        #[arg(long)]
        name: Option<String>,
        /// Apply it. Without this, only show what would change
        #[arg(long)]
        yes: bool,
    },
    /// Stop following the program. Its scope rules stay unless --remove-scope
    Stop {
        #[arg(long)]
        remove_scope: bool,
    },
    /// Platforms Plonix can read programs from, and whether each is connected
    Platforms,
    /// Connect a platform. The token is read from $PLONIX_PLATFORM_TOKEN or standard input
    Connect {
        platform: String,
        /// The platform user name, for platforms that ask for one
        #[arg(long)]
        user: Option<String>,
    },
    /// Forget a platform's token
    Disconnect { platform: String },
    /// The programs you can work on at a platform, from the last sync
    List { platform: String },
    /// Pull every program at a platform, with its assets and rules
    Sync { platform: String },
    /// Every in-scope asset across a platform's programs, from the last sync
    Assets {
        platform: String,
        /// Only assets (or programs) whose name contains this
        #[arg(long)]
        find: Option<String>,
    },
}

pub fn program_cmd(ctx: &Ctx, cmd: ProgramCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        ProgramCmd::Show => {
            let v = c.get("/api/program")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            if v["program"].is_null() {
                println!("This project follows no program. Bring one in with `plonix program follow hackerone:<handle>` or `plonix program follow ./policy.txt`.");
                return Ok(());
            }
            print_program(&v["program"]);
        }
        ProgramCmd::Follow { source, name, yes } => {
            let program = read(&c, &source, name.as_deref().unwrap_or(""))?;
            let preview = c.post("/api/program/preview", json!({ "program": program }))?;
            if !yes {
                if ctx.json {
                    return ctx.print_json(&preview);
                }
                print_preview(&preview);
                println!("\nNothing changed yet. Run again with --yes to follow this program.");
                return Ok(());
            }
            let applied = c.post("/api/program/apply", json!({ "program": program }))?;
            if ctx.json {
                return ctx.print_json(&applied);
            }
            print_preview(&applied);
            println!("\nThis project now follows {}.", applied["program"]["name"].as_str().unwrap_or(""));
        }
        ProgramCmd::Stop { remove_scope } => {
            let v = c.post("/api/program/clear", json!({ "remove_scope": remove_scope }))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("{}", if v["cleared"].as_bool() == Some(true) { "Stopped following the program." } else { "This project follows no program." });
        }
        ProgramCmd::Platforms => {
            let v = c.get("/api/platforms")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            for p in v["platforms"].as_array().into_iter().flatten() {
                let state = if p["connected"].as_bool() == Some(true) { "connected" } else { "not connected" };
                println!("{:<14} {:<22} {state}", p["name"].as_str().unwrap_or(""), p["title"].as_str().unwrap_or(""));
            }
            for problem in v["problems"].as_array().into_iter().flatten() {
                eprintln!("warning: {}", problem.as_str().unwrap_or(""));
            }
        }
        ProgramCmd::Connect { platform, user } => {
            let secret = match std::env::var("PLONIX_PLATFORM_TOKEN") {
                Ok(t) if !t.trim().is_empty() => t,
                _ => crate::har::ask_secret(&format!("Token for {platform}"))?,
            };
            let v = c.post(&format!("/api/platforms/{}/connect", crate::client::encode(&platform)), json!({ "user": user.unwrap_or_default(), "secret": secret.trim() }))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Connected to {platform}: {} programs. Pulling their scope and rules now; see `plonix program sync {platform}`.", v["programs"]);
        }
        ProgramCmd::Disconnect { platform } => {
            c.post(&format!("/api/platforms/{}/disconnect", crate::client::encode(&platform)), json!({}))?;
            println!("Forgot the {platform} token.");
        }
        ProgramCmd::List { platform } => {
            let cat = catalog(&c, &platform)?;
            if ctx.json {
                return ctx.print_json(&cat);
            }
            for e in cat["programs"].as_array().into_iter().flatten() {
                let p = &e["program"];
                let in_scope = p["assets"].as_array().into_iter().flatten().filter(|a| a["in_scope"].as_bool() == Some(true)).count();
                let bounty = if p["bounty"].as_bool() == Some(true) { "bounty" } else { "" };
                let state = e["state"].as_str().filter(|s| *s != "open").unwrap_or("");
                println!("{:<28} {:<36} {in_scope:>4} in scope  {bounty:<6} {state}", p["id"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or(""));
            }
        }
        ProgramCmd::Sync { platform } => {
            let path = format!("/api/platforms/{}/sync", crate::client::encode(&platform));
            c.post(&path, json!({}))?;
            let mut last = (u64::MAX, u64::MAX);
            let v = loop {
                let v = c.get(&format!("/api/platforms/{}/catalog", crate::client::encode(&platform)))?;
                let s = &v["sync"];
                if s["running"].as_bool() != Some(true) {
                    break v;
                }
                let now = (s["done"].as_u64().unwrap_or(0), s["total"].as_u64().unwrap_or(0));
                if now != last && !ctx.json {
                    eprint!("\rPulling programs: {} of {}   ", now.0, now.1);
                    last = now;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            };
            if !ctx.json && last != (u64::MAX, u64::MAX) {
                eprintln!();
            }
            if let Some(e) = v["sync"]["error"].as_str() {
                bail!("{e}");
            }
            if ctx.json {
                return ctx.print_json(&v["catalog"]);
            }
            let programs = v["catalog"]["programs"].as_array().cloned().unwrap_or_default();
            let assets: usize = programs.iter().map(|e| e["program"]["assets"].as_array().into_iter().flatten().filter(|a| a["in_scope"].as_bool() == Some(true)).count()).sum();
            println!("Synced {} programs with {assets} in-scope assets.", programs.len());
            for f in v["catalog"]["failed"].as_array().into_iter().flatten() {
                eprintln!("could not read {}: {}", f["name"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or(""));
            }
        }
        ProgramCmd::Assets { platform, find } => {
            let cat = catalog(&c, &platform)?;
            let find = find.unwrap_or_default().to_lowercase();
            let mut rows = vec![];
            for e in cat["programs"].as_array().into_iter().flatten() {
                let p = &e["program"];
                let name = p["name"].as_str().unwrap_or("");
                for a in p["assets"].as_array().into_iter().flatten().filter(|a| a["in_scope"].as_bool() == Some(true)) {
                    let id = a["identifier"].as_str().unwrap_or("");
                    if find.is_empty() || id.to_lowercase().contains(&find) || name.to_lowercase().contains(&find) {
                        rows.push(json!({ "asset": id, "kind": a["kind"], "program": p["id"], "program_name": name, "bounty": a["bounty"] }));
                    }
                }
            }
            if ctx.json {
                return ctx.print_json(&json!({ "assets": rows }));
            }
            for r in &rows {
                let bounty = if r["bounty"].as_bool() == Some(true) { "bounty" } else { "" };
                println!("{:<48} {:<9} {:<28} {bounty}", r["asset"].as_str().unwrap_or(""), r["kind"].as_str().unwrap_or(""), r["program"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}

/// A platform's synced programs. Says how to get them when there are none yet.
fn catalog(c: &crate::client::Client, platform: &str) -> Result<Value> {
    let v = c.get(&format!("/api/platforms/{}/catalog", crate::client::encode(platform)))?;
    if v["catalog"].is_null() {
        if v["sync"]["running"].as_bool() == Some(true) {
            bail!("still pulling programs from {platform}; run `plonix program sync {platform}` to wait for it");
        }
        bail!("no programs pulled from {platform} yet; run `plonix program sync {platform}`");
    }
    Ok(v["catalog"].clone())
}

fn read(c: &crate::client::Client, source: &str, name: &str) -> Result<Value> {
    if let Some((platform, handle)) = source.split_once(':')
        && !platform.contains(['/', '.'])
        && !handle.starts_with("//")
    {
        let v = c.get(&format!("/api/platforms/{}/programs/{}?name={}", crate::client::encode(platform), crate::client::encode(handle), crate::client::encode(name)))?;
        return Ok(v["program"].clone());
    }
    let body = if source.starts_with("https://") || source.starts_with("http://") {
        json!({ "url": source, "name": name })
    } else if std::path::Path::new(source).is_file() {
        json!({ "text": std::fs::read_to_string(source)?, "name": name })
    } else if source.contains('.') && !source.contains(['/', ' ']) {
        json!({ "domain": source, "name": name })
    } else {
        bail!("`{source}` is not a platform:handle, a file, an https address or a domain");
    };
    Ok(c.post("/api/program/read", body)?["program"].clone())
}

fn rules_lines(r: &Value) -> Vec<String> {
    let mut out = vec![];
    if let Some(rate) = r["rate_per_second"].as_f64() {
        out.push(format!("at most {rate} requests per second"));
    }
    for h in r["headers"].as_array().into_iter().flatten() {
        let pending = if h["needs_value"].as_bool() == Some(true) { "  (fill in the value; not sent until then)" } else { "" };
        out.push(format!("adds {}: {}{pending}", h["name"].as_str().unwrap_or(""), h["value"].as_str().unwrap_or("")));
    }
    if r["no_automation"].as_bool() == Some(true) {
        out.push("no automated testing: scans, crawls and Bench runs are off".into());
    }
    if r["no_intrusive"].as_bool() == Some(true) {
        out.push("no disruptive tests: intrusive scan checks are off".into());
    }
    out
}

fn print_program(p: &Value) {
    println!("{}  ({})", p["name"].as_str().unwrap_or(""), p["platform"].as_str().unwrap_or(""));
    if let Some(url) = p["url"].as_str().filter(|u| !u.is_empty()) {
        println!("{url}");
    }
    println!();
    for line in rules_lines(&p["rules"]) {
        println!("  rule   {line}");
    }
    for a in p["assets"].as_array().into_iter().flatten() {
        let scope = if a["in_scope"].as_bool() == Some(true) { "in " } else { "out" };
        println!("  {scope}    {:<44} {}", a["identifier"].as_str().unwrap_or(""), a["kind"].as_str().unwrap_or(""));
    }
    for x in p["rules"]["not_accepted"].as_array().into_iter().flatten() {
        println!("  not accepted: {}", x.as_str().unwrap_or(""));
    }
}

fn print_preview(v: &Value) {
    let p = &v["program"];
    println!("{}  ({})", p["name"].as_str().unwrap_or(""), p["platform"].as_str().unwrap_or(""));
    if let Some(old) = v["replaces"].as_str() {
        println!("Replaces {old}, which this project follows now.");
    }
    println!("\nScope:");
    for c in v["scope"].as_array().into_iter().flatten() {
        let pattern = format!("{}{}", if c["include_subdomains"].as_bool() == Some(true) { "*." } else { "" }, c["pattern"].as_str().unwrap_or(""));
        println!("  {:<8} {:<44} {}", c["decision"].as_str().unwrap_or(""), pattern, c["change"].as_str().unwrap_or(""));
    }
    let not: Vec<&str> = v["not_scoped"].as_array().into_iter().flatten().filter_map(|a| a["identifier"].as_str()).collect();
    if !not.is_empty() {
        println!("  not scope rules: {}", not.join(", "));
    }
    println!("\nRules:");
    let lines = rules_lines(&p["rules"]);
    if lines.is_empty() {
        println!("  none found in the policy");
    }
    for line in lines {
        println!("  {line}");
    }
}
