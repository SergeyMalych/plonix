//! `plonix connect claude`: registers `plonix mcp` as an MCP server in
//! Claude Code, so the agent can read the live project.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use plonix_core::access::{self, AgentMode};
use plonix_core::paths::Home;
use serde_json::{Value, json};

use crate::{Ctx, mcp, open};

#[derive(Subcommand)]
pub enum ConnectCmd {
    /// Add Plonix to Claude Code as a read-only MCP server
    Claude(ClaudeArgs),
}

#[derive(Args)]
pub struct ClaudeArgs {
    /// Where Claude Code keeps the server: every project (user), this
    /// directory only (local), or a shared .mcp.json here (project)
    #[arg(long, short, value_enum, default_value_t = ClaudeScope::User)]
    scope: ClaudeScope,
    /// Print the configuration instead of changing Claude Code's settings
    #[arg(long)]
    print: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum ClaudeScope {
    User,
    Local,
    Project,
}

impl ClaudeScope {
    fn as_str(self) -> &'static str {
        match self {
            ClaudeScope::User => "user",
            ClaudeScope::Local => "local",
            ClaudeScope::Project => "project",
        }
    }
}

const NAME: &str = "plonix";

pub const EXAMPLE_PROMPT: &str =
    "Use Plonix to find in-scope API endpoints that returned errors, then read the most interesting request and tell me what stands out.";

pub fn connect_cmd(ctx: &Ctx, cmd: ConnectCmd) -> Result<()> {
    match cmd {
        ConnectCmd::Claude(a) => claude(ctx, a),
    }
}

/// The MCP server entry: this `plonix` binary, run as `plonix mcp`.
fn server_config(home: &Home) -> Result<Value> {
    let exe = std::env::current_exe().context("locating the plonix executable")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let mut cfg = json!({ "type": "stdio", "command": exe, "args": ["mcp"] });
    if !is_default_home(&home.root) {
        cfg["env"] = json!({ "PLONIX_HOME": home.root });
    }
    Ok(cfg)
}

fn is_default_home(root: &Path) -> bool {
    std::env::var_os("HOME").is_some_and(|h| Path::new(&h).join(".plonix") == root)
}

fn claude(ctx: &Ctx, a: ClaudeArgs) -> Result<()> {
    ctx.home.ensure()?;
    ctx.home.load_or_create_agent_token()?;
    let cfg = server_config(&ctx.home)?;
    let snippet = json!({ "mcpServers": { NAME: cfg } });

    let claude = if a.print { None } else { find_claude() };
    let added = match &claude {
        Some(bin) => {
            add_to_claude(bin, a.scope, &cfg)?;
            true
        }
        None => false,
    };

    if ctx.json {
        return ctx.print_json(&json!({
            "added": added,
            "scope": a.scope.as_str(),
            "config": snippet,
            "tools": mcp::tool_names(),
            "mode": AgentMode::current(),
        }));
    }

    println!("Plonix · connect Claude Code\n");
    println!("  ✓ {:<13}{}  (read-only)", "Agent token", open::tilde(&ctx.home.agent_token()));
    let command = format!("{} mcp", cfg["command"].as_str().unwrap_or("plonix"));
    if added {
        let where_ = match a.scope {
            ClaudeScope::User => "for all your projects".to_string(),
            ClaudeScope::Local => "for this directory".to_string(),
            ClaudeScope::Project => "in ./.mcp.json".to_string(),
        };
        println!("  ✓ {:<13}added MCP server \"{NAME}\" {where_}: {command}", "Claude Code");
    } else {
        if !a.print {
            println!("  ! {:<13}the `claude` command was not found, so nothing was changed", "Claude Code");
        }
        println!(
            "\nAdd Plonix to Claude Code with:\n\n  claude mcp add --scope {} {NAME} -- {command}\n\nor put this in a project's .mcp.json:\n\n{}",
            a.scope.as_str(),
            indent(&serde_json::to_string_pretty(&snippet)?)
        );
    }

    let mode = AgentMode::current();
    println!("\nWhat the agent can do (read-only; a Bench edit is only suggested, for you to apply):");
    let tools = mcp::tool_names();
    println!("  {}", tools.join(" · "));
    println!("Not allowed:");
    for n in access::not_allowed(mode) {
        println!("  ✗ {n}");
    }
    println!(
        "\nThe agent reads the engine that is running (`plonix start`, `plonix open <target>` or Plonix.app).\n\
         Captured traffic stays on this machine; it can hold credentials, so connect only agents you trust.\n\n\
         Try it in Claude Code:\n\n  \"{EXAMPLE_PROMPT}\""
    );
    Ok(())
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("  {l}\n")).collect()
}

/// `$PLONIX_CLAUDE` (for tests), else `claude` on the PATH.
fn find_claude() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("PLONIX_CLAUDE") {
        return Some(p.into());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("claude")).find(|p| p.is_file())
}

/// Replaces any previous "plonix" entry, so re-running picks up a moved binary.
fn add_to_claude(bin: &Path, scope: ClaudeScope, cfg: &Value) -> Result<()> {
    let _ = Command::new(bin).args(["mcp", "remove", "--scope", scope.as_str(), NAME]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let out = Command::new(bin)
        .args(["mcp", "add-json", "--scope", scope.as_str(), NAME])
        .arg(cfg.to_string())
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {}", bin.display()))?;
    if !out.status.success() {
        bail!("Claude Code did not add the server: {}{}", String::from_utf8_lossy(&out.stderr).trim(), String::from_utf8_lossy(&out.stdout).trim());
    }
    Ok(())
}
