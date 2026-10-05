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
    /// The programs you can work on at a platform
    List { platform: String },
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
                _ => {
                    eprint!("Token for {platform}: ");
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line
                }
            };
            let v = c.post(&format!("/api/platforms/{}/connect", seg(&platform)), json!({ "user": user.unwrap_or_default(), "secret": secret.trim() }))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Connected to {platform}: {} programs.", v["programs"]);
        }
        ProgramCmd::Disconnect { platform } => {
            c.post(&format!("/api/platforms/{}/disconnect", seg(&platform)), json!({}))?;
            println!("Forgot the {platform} token.");
        }
        ProgramCmd::List { platform } => {
            let v = c.get(&format!("/api/platforms/{}/programs", seg(&platform)))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            for p in v["programs"].as_array().into_iter().flatten() {
                let bounty = if p["bounty"].as_bool() == Some(true) { "bounty" } else { "" };
                println!("{:<32} {:<40} {bounty}", p["handle"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}

/// Percent-encodes a path segment.
fn seg(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn read(c: &crate::client::Client, source: &str, name: &str) -> Result<Value> {
    if let Some((platform, handle)) = source.split_once(':')
        && !platform.contains(['/', '.'])
        && !handle.starts_with("//")
    {
        let v = c.get(&format!("/api/platforms/{}/programs/{}?name={}", seg(platform), seg(handle), seg(name)))?;
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
