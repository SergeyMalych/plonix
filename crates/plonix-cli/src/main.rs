//! The `plonix` command.
//!
//! Every subcommand except `engine` and `ca` is a client of the engine's
//! local API (see `client`), exactly like the GUI. The MCP server will be
//! another front end over the same `client` and `render` modules.

mod client;
mod engine_ctl;
mod open;
mod render;

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use plonix_core::ca::CertAuthority;
use plonix_core::paths::Home;
use serde_json::{Value, json};

use client::{ApiError, Client, NotRunning, encode};
use engine_ctl::{DEFAULT_API_PORT, DEFAULT_PROXY_PORT, StartOptions};

const EXAMPLES: &str = "\
Get started:
  plonix open example.com          start capturing and open a browser at the target
  plonix search host:example.com status:5xx
  plonix show 42
  plonix scope                     review domains Plonix thinks belong in scope
  plonix replay 42 -H 'X-Debug: 1'

Exit codes:
  0 ok · 1 error · 2 bad usage or query · 3 engine not running
  4 refused: host not in scope · 5 not found";

#[derive(Parser)]
#[command(
    name = "plonix",
    version,
    about = "Plonix: the open-source web security workbench.\nCapture traffic, learn the target's real scope, search it, replay it.",
    after_help = EXAMPLES,
    disable_help_subcommand = true
)]
struct Cli {
    /// Plonix data directory [default: $PLONIX_HOME or ~/.plonix]
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Print machine-readable JSON instead of text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start capturing and open a browser at the target, in one step
    Open(OpenArgs),
    /// Start the engine (proxy + API) in the background
    Start(StartArgs),
    /// Stop the engine
    Stop,
    /// Show whether the engine is running and what it has captured
    Status,
    /// Search captured traffic, newest first
    #[command(
        visible_alias = "s",
        after_help = "Filters: host:example.com  method:POST  status:404|5xx|none  path:/api  mime:json\n         scope:in|out  source:proxy|replay  \"quoted phrase\"  -negated  free text\nPut options such as -n before the query: everything after the first term is search text."
    )]
    Search(SearchArgs),
    /// Print new traffic as it is captured (Ctrl-C to stop)
    Watch {
        /// Only show traffic matching this search
        #[arg(allow_hyphen_values = true)]
        query: Vec<String>,
    },
    /// Show one request and its response
    Show {
        /// Exchange id (the first column of `plonix search`)
        id: i64,
        /// Print bodies in full
        #[arg(long)]
        full: bool,
    },
    /// Re-send a captured request, optionally modified (accepted hosts only)
    Replay(ReplayArgs),
    /// Review and change scope
    Scope {
        #[command(subcommand)]
        cmd: Option<ScopeCmd>,
    },
    /// Hosts seen so far, busiest first
    Hosts,
    /// The local certificate authority
    Ca {
        #[command(subcommand)]
        cmd: Option<CaCmd>,
    },
    /// Run the engine in the foreground (used by `plonix start`)
    #[command(hide = true)]
    Engine(EngineArgs),
}

#[derive(Args)]
struct OpenArgs {
    /// Host name or URL, e.g. example.com or http://localhost:3000
    target: String,
    /// Project to record into (default: the target's host name)
    #[arg(long, short)]
    project: Option<String>,
    /// Do not add the target to scope
    #[arg(long)]
    no_scope: bool,
    /// Start the engine but do not open a browser
    #[arg(long)]
    no_browser: bool,
    /// Return right away instead of printing traffic as it arrives
    #[arg(long)]
    no_watch: bool,
    #[command(flatten)]
    engine: EngineFlags,
}

#[derive(Args)]
struct StartArgs {
    /// Project to record into
    #[arg(long, short, default_value = "default")]
    project: String,
    #[command(flatten)]
    engine: EngineFlags,
}

#[derive(Args, Clone)]
struct EngineFlags {
    /// Proxy port (another free port is used if it is taken)
    #[arg(long, default_value_t = DEFAULT_PROXY_PORT)]
    port: u16,
    /// Local API port (another free port is used if it is taken)
    #[arg(long, default_value_t = DEFAULT_API_PORT)]
    api_port: u16,
    /// Accept invalid upstream certificates (self-signed staging hosts)
    #[arg(long)]
    insecure_upstream: bool,
}

#[derive(Args)]
struct EngineArgs {
    #[arg(long, default_value = "default")]
    project: String,
    #[arg(long, default_value_t = DEFAULT_PROXY_PORT)]
    proxy_port: u16,
    #[arg(long, default_value_t = DEFAULT_API_PORT)]
    api_port: u16,
    #[arg(long)]
    insecure_upstream: bool,
}

#[derive(Args)]
struct SearchArgs {
    /// Search terms; no terms lists the most recent traffic
    #[arg(allow_hyphen_values = true)]
    query: Vec<String>,
    /// Maximum number of results
    #[arg(long, short = 'n', default_value_t = 50)]
    limit: usize,
    /// Skip this many results (for paging)
    #[arg(long, default_value_t = 0)]
    offset: usize,
}

#[derive(Args)]
struct ReplayArgs {
    /// Exchange id to replay
    id: i64,
    /// Use a different method
    #[arg(long, short = 'X')]
    method: Option<String>,
    /// Use a different path and query, e.g. /api/users/2?debug=1
    #[arg(long, short = 't')]
    target: Option<String>,
    /// Add or replace a header, e.g. -H 'Authorization: Bearer x' (repeatable)
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
    /// Remove a header (repeatable)
    #[arg(long = "remove-header", value_name = "NAME")]
    remove_headers: Vec<String>,
    /// Replace the body; @file reads it from a file
    #[arg(long, short = 'd', value_name = "BODY")]
    data: Option<String>,
    /// Print bodies in full
    #[arg(long)]
    full: bool,
}

#[derive(Subcommand)]
enum ScopeCmd {
    /// Scope rules and suggested domains with their evidence (the default)
    List,
    /// Go through suggestions one by one and decide
    Review,
    /// Bring domains into scope (`*.example.com` includes subdomains)
    #[command(visible_alias = "add")]
    Accept(DomainArgs),
    /// Keep domains out of scope
    Reject(DomainArgs),
    /// Forget the rule for a domain
    Remove {
        #[arg(required = true)]
        domains: Vec<String>,
    },
}

#[derive(Args)]
struct DomainArgs {
    #[arg(required = true)]
    domains: Vec<String>,
    /// Include subdomains (same as writing *.domain)
    #[arg(long, short = 's')]
    subdomains: bool,
}

#[derive(Subcommand)]
enum CaCmd {
    /// Where the certificate is and how to trust it (the default)
    Show,
    /// Print the certificate in PEM form
    Pem,
    /// Trust the certificate in your macOS login keychain
    Trust,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(exit_code(&e))
        }
    }
}

fn exit_code(e: &anyhow::Error) -> u8 {
    if e.downcast_ref::<NotRunning>().is_some() {
        return 3;
    }
    match e.downcast_ref::<ApiError>().map(|a| a.code.as_str()) {
        Some("out_of_scope") => 4,
        Some("not_found") => 5,
        Some("bad_query" | "bad_request" | "bad_domain") => 2,
        _ => 1,
    }
}

struct Ctx {
    home: Home,
    json: bool,
}

impl Ctx {
    fn client(&self) -> Result<Client> {
        Client::connect(&self.home, "cli")
    }

    fn print_json(&self, v: &Value) -> Result<()> {
        println!("{}", serde_json::to_string_pretty(v)?);
        Ok(())
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let ctx = Ctx { home: Home::resolve(cli.home.as_deref())?, json: cli.json };
    match cli.command {
        Cmd::Open(a) => open_cmd(&ctx, a)?,
        Cmd::Start(a) => start_cmd(&ctx, a)?,
        Cmd::Stop => {
            if engine_ctl::stop(&ctx.home)? {
                println!("Plonix engine stopped. Captured traffic is kept.");
            } else {
                println!("Plonix engine is not running.");
            }
        }
        Cmd::Status => return status_cmd(&ctx),
        Cmd::Search(a) => search_cmd(&ctx, a)?,
        Cmd::Watch { query } => {
            let c = ctx.client()?;
            watch(&c, &query.join(" "), ctx.json)?;
        }
        Cmd::Show { id, full } => {
            let v = ctx.client()?.get(&format!("/api/traffic/{id}"))?;
            if ctx.json {
                ctx.print_json(&v)?;
            } else {
                print!("{}", render::exchange(&v, if full { usize::MAX } else { 4000 }));
            }
        }
        Cmd::Replay(a) => replay_cmd(&ctx, a)?,
        Cmd::Scope { cmd } => scope_cmd(&ctx, cmd.unwrap_or(ScopeCmd::List))?,
        Cmd::Hosts => {
            let v = ctx.client()?.get("/api/hosts")?;
            if ctx.json {
                ctx.print_json(&v)?;
            } else {
                let items = v.as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    println!("No hosts yet.");
                } else {
                    println!("{:>7}  {:<5}  HOST", "REQS", "SCOPE");
                    print!("{}", render::hosts(&items));
                }
            }
        }
        Cmd::Ca { cmd } => ca_cmd(&ctx, cmd.unwrap_or(CaCmd::Show))?,
        Cmd::Engine(a) => engine_ctl::run_foreground(
            ctx.home.clone(),
            &StartOptions {
                project: engine_ctl::project_name(&a.project),
                proxy_port: a.proxy_port,
                api_port: a.api_port,
                insecure_upstream: a.insecure_upstream,
            },
        )?,
    }
    Ok(ExitCode::SUCCESS)
}

fn start_options(project: &str, f: &EngineFlags) -> StartOptions {
    StartOptions {
        project: engine_ctl::project_name(project),
        proxy_port: f.port,
        api_port: f.api_port,
        insecure_upstream: f.insecure_upstream,
    }
}

fn start_cmd(ctx: &Ctx, a: StartArgs) -> Result<()> {
    let opts = start_options(&a.project, &a.engine);
    let (c, started) = engine_ctl::start(&ctx.home, &opts)?;
    let running_project = c.status["project"].as_str().unwrap_or("");
    if !started && running_project != opts.project && opts.project != "default" {
        bail!(
            "the engine is already running with project '{running_project}'. Stop it first with `plonix stop`."
        );
    }
    if ctx.json {
        return ctx.print_json(&json!({ "started": started, "status": c.status }));
    }
    println!("{}", if started { "Plonix engine started." } else { "Plonix engine is already running." });
    print!("{}", render::status(&c.status, &open::tilde(&ctx.home.ca_cert())));
    println!("\nNext: `plonix open <target>` opens a browser that captures through the proxy.");
    Ok(())
}

fn status_cmd(ctx: &Ctx) -> Result<ExitCode> {
    let running = engine_ctl::running(&ctx.home, "cli");
    match &running {
        Some(c) if ctx.json => ctx.print_json(&json!({ "running": true, "status": c.status }))?,
        Some(c) => print!("{}", render::status(&c.status, &open::tilde(&ctx.home.ca_cert()))),
        None if ctx.json => ctx.print_json(&json!({ "running": false }))?,
        None => println!("{}", client::NOT_RUNNING),
    }
    // Scripts can test `plonix status` for a running engine.
    Ok(if running.is_some() { ExitCode::SUCCESS } else { ExitCode::from(3) })
}

fn search_cmd(ctx: &Ctx, a: SearchArgs) -> Result<()> {
    let q = a.query.join(" ");
    let v = ctx.client()?.get(&format!("/api/traffic?q={}&limit={}&offset={}", encode(&q), a.limit, a.offset))?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let total = v["total"].as_i64().unwrap_or(0);
    if items.is_empty() {
        if q.trim().is_empty() {
            println!("Nothing captured yet. Run `plonix open <target>` and browse the site.");
        } else {
            println!("No traffic matches `{q}`.");
        }
        return Ok(());
    }
    print!("{}", render::traffic_table(&items));
    let shown = a.offset + items.len();
    let more = if (shown as i64) < total { format!(" · next page: --offset {shown}") } else { String::new() };
    println!("\n{} of {} match(es){} · `plonix show <id>` for details", items.len(), total, more);
    Ok(())
}

fn parse_header(h: &str) -> Result<(String, String)> {
    match h.split_once(':') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.trim().to_string())),
        _ => bail!("header must look like 'Name: value', got '{h}'"),
    }
}

fn replay_cmd(ctx: &Ctx, a: ReplayArgs) -> Result<()> {
    let set_headers = a.headers.iter().map(|h| parse_header(h)).collect::<Result<Vec<_>>>()?;
    let body = match a.data {
        Some(d) => match d.strip_prefix('@') {
            Some(path) => Some(std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?),
            None => Some(d),
        },
        None => None,
    };
    let req = json!({
        "id": a.id,
        "method": a.method,
        "target": a.target,
        "set_headers": set_headers,
        "remove_headers": a.remove_headers,
        "body": body,
    });
    let v = ctx.client()?.post("/api/replay", req)?;
    if ctx.json {
        return ctx.print_json(&v);
    }
    println!("Replayed #{} as #{}\n", a.id, v["id"]);
    print!("{}", render::exchange(&v, if a.full { usize::MAX } else { 4000 }));
    Ok(())
}

fn scope_cmd(ctx: &Ctx, cmd: ScopeCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        ScopeCmd::List => {
            let v = c.get("/api/scope")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            print!("{}", render::scope(&v));
        }
        ScopeCmd::Accept(d) => decide(ctx, &c, "accept", &d.domains, d.subdomains)?,
        ScopeCmd::Reject(d) => decide(ctx, &c, "reject", &d.domains, d.subdomains)?,
        ScopeCmd::Remove { domains } => {
            let mut out = vec![];
            for d in &domains {
                let v = c.post("/api/scope/remove", json!({ "domain": d }))?;
                if !ctx.json {
                    let removed = v["removed"].as_bool() == Some(true);
                    println!("{} {d}", if removed { "removed rule for" } else { "no rule for" });
                }
                out.push(json!({ "domain": d, "removed": v["removed"] }));
            }
            if ctx.json {
                ctx.print_json(&Value::from(out))?;
            }
        }
        ScopeCmd::Review => review(&c)?,
    }
    Ok(())
}

fn decide(ctx: &Ctx, c: &Client, action: &str, domains: &[String], subdomains: bool) -> Result<()> {
    let mut rules = vec![];
    for d in domains {
        let rule = c.post(&format!("/api/scope/{action}"), json!({ "domain": d, "include_subdomains": subdomains }))?;
        if !ctx.json {
            let mark = if action == "accept" { "✓ in scope:    " } else { "✗ out of scope:" };
            println!("{mark} {}", rule_label(&rule));
        }
        rules.push(rule);
    }
    if ctx.json {
        return ctx.print_json(&Value::from(rules));
    }
    let pending = c.get("/api/scope")?["suggestions"].as_array().map_or(0, Vec::len);
    if pending > 0 {
        println!("\n{pending} suggested domain(s) waiting. See them with `plonix scope`.");
    }
    Ok(())
}

fn rule_label(rule: &Value) -> String {
    let p = rule["pattern"].as_str().unwrap_or("");
    if rule["include_subdomains"].as_bool() == Some(true) { format!("{p} (+ subdomains)") } else { p.to_string() }
}

/// Interactive review: one suggestion at a time, strongest evidence first.
fn review(c: &Client) -> Result<()> {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut skipped: Vec<String> = vec![];
    loop {
        let v = c.get("/api/scope")?;
        let next = v["suggestions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| !skipped.iter().any(|d| s["domain"].as_str() == Some(d)))
            .cloned();
        let Some(s) = next else {
            println!("{}", if skipped.is_empty() { "No suggestions to review." } else { "Done." });
            return Ok(());
        };
        let domain = s["domain"].as_str().unwrap_or("").to_string();
        print!("{}", render::suggestion(&s));
        print!("\n  [a]ccept · accept with [s]ubdomains · [r]eject · [n]ext · [q]uit > ");
        std::io::stdout().flush()?;
        let Some(line) = lines.next().transpose()? else { return Ok(()) };
        let (action, subs) = match line.trim().to_ascii_lowercase().as_str() {
            "a" | "y" => ("accept", false),
            "s" => ("accept", true),
            "r" => ("reject", false),
            "q" => return Ok(()),
            _ => {
                skipped.push(domain);
                continue;
            }
        };
        let rule = c.post(&format!("/api/scope/{action}"), json!({ "domain": domain, "include_subdomains": subs }))?;
        println!("  {} {}", if action == "accept" { "✓ in scope:" } else { "✗ out of scope:" }, rule_label(&rule));
    }
}

/// Prints traffic captured from now on. Runs until interrupted.
fn watch(c: &Client, q: &str, json_out: bool) -> Result<()> {
    let page = |limit: usize| c.get(&format!("/api/traffic?q={}&limit={limit}", encode(q)));
    let mut last = page(1)?["items"][0]["id"].as_i64().unwrap_or(0);
    loop {
        let v = page(500)?;
        let mut fresh: Vec<Value> = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["id"].as_i64().unwrap_or(0) > last)
            .cloned()
            .collect();
        fresh.reverse();
        for item in &fresh {
            if json_out {
                println!("{item}");
            } else {
                println!("{}", render::traffic_line(item));
            }
            last = last.max(item["id"].as_i64().unwrap_or(0));
        }
        std::io::stdout().flush()?;
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn open_cmd(ctx: &Ctx, a: OpenArgs) -> Result<()> {
    let target = open::parse_target(&a.target)?;
    let first_run = !ctx.home.ca_cert().exists();
    ctx.home.ensure()?;
    let ca = CertAuthority::load_or_create(&ctx.home)?;

    let project = a.project.clone().unwrap_or_else(|| target.host.clone());
    let (c, started) = engine_ctl::start(&ctx.home, &start_options(&project, &a.engine))?;
    let proxy = c.status["proxy"].as_str().unwrap_or("").to_string();
    let running_project = c.status["project"].as_str().unwrap_or("").to_string();

    let say = |label: &str, text: &str| {
        if !ctx.json {
            println!("  ✓ {label:<13}{text}");
        }
    };
    if !ctx.json {
        println!("Plonix · {}\n", target.url);
    }
    say(
        "Certificate",
        &format!("{}{}", open::tilde(&ctx.home.ca_cert()), if first_run { "  (created)" } else { "" }),
    );
    say("Proxy", &format!("{proxy}  ({}, project {running_project})", if started { "started" } else { "already running" }));

    let mut scope_rule = Value::Null;
    if !a.no_scope {
        scope_rule = c.client.post("/api/scope/accept", json!({ "domain": target.host, "include_subdomains": true }))?;
        say("Scope", &format!("{}  (more domains are suggested as you browse)", rule_label(&scope_rule)));
    }

    let mut browser_json = Value::Null;
    let mut needs_trust = false;
    if a.no_browser {
        say("Browser", &format!("skipped; set your browser's proxy to {proxy}"));
    } else {
        match open::detect() {
            Some(b) => {
                let args = open::launch(&ctx.home, &b, &proxy, &ca.spki_sha256(), &target.url)?;
                needs_trust = b.kind == open::Kind::Firefox;
                let how = if needs_trust { "isolated profile" } else { "isolated profile, trusts Plonix" };
                say("Browser", &format!("{} ({how})", b.name));
                browser_json = json!({ "name": b.name, "exe": b.exe, "args": args });
            }
            None => {
                needs_trust = true;
                say("Browser", "none found to launch");
                if !ctx.json {
                    println!(
                        "\n  Install Google Chrome, Brave, Edge or Firefox, or point any browser at the proxy:\n  \
                         HTTP and HTTPS proxy {proxy}, then visit {}",
                        target.url
                    );
                }
            }
        }
    }

    if ctx.json {
        return ctx.print_json(&json!({
            "target": target.url,
            "host": target.host,
            "proxy": proxy,
            "project": running_project,
            "engine_started": started,
            "ca": ctx.home.ca_cert(),
            "ca_created": first_run,
            "scope": scope_rule,
            "browser": browser_json,
        }));
    }

    // Trust guidance: always when the browser needs it, otherwise once.
    let marker = ctx.home.root.join(".trust-guidance-shown");
    if needs_trust || !marker.exists() {
        println!("\n{}", open::trust_guidance(&ctx.home));
        let _ = std::fs::write(&marker, b"");
    }

    let watching = !a.no_watch && std::io::stdout().is_terminal();
    if watching {
        println!("\nCapturing. Browse the site; requests appear below. Ctrl-C stops watching, capture keeps running.\n");
        watch(&c.client, "", false)?;
    } else {
        println!("\nCapturing. Browse the site, then: plonix search host:{}", target.host);
    }
    Ok(())
}

fn ca_cmd(ctx: &Ctx, cmd: CaCmd) -> Result<()> {
    ctx.home.ensure()?;
    let ca = CertAuthority::load_or_create(&ctx.home)?;
    let path = ctx.home.ca_cert();
    match cmd {
        CaCmd::Pem => print!("{}", ca.ca_pem()),
        CaCmd::Show if ctx.json => ctx.print_json(&json!({ "path": path, "sha256": ca.fingerprint() }))?,
        CaCmd::Show => {
            println!("Plonix CA\n  File      {}\n  SHA-256   {}\n", open::tilde(&path), ca.fingerprint());
            print!("{}", open::trust_guidance(&ctx.home));
        }
        CaCmd::Trust => trust(&path)?,
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn trust(path: &std::path::Path) -> Result<()> {
    let keychain = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/Keychains/login.keychain-db"))
        .context("HOME is not set")?;
    println!("Adding {} to your login keychain as a trusted root. macOS will ask you to confirm.", path.display());
    let status = std::process::Command::new("/usr/bin/security")
        .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
        .arg(&keychain)
        .arg(path)
        .status()
        .context("running /usr/bin/security")?;
    if !status.success() {
        bail!("macOS did not add the certificate ({status}). Nothing was changed.");
    }
    println!("✓ Trusted. Safari, curl and other apps now accept HTTPS through the Plonix proxy.");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn trust(path: &std::path::Path) -> Result<()> {
    bail!(
        "`plonix ca trust` is macOS only. On this system add {} to your trust store by hand \
         (e.g. Debian/Ubuntu: copy it to /usr/local/share/ca-certificates/plonix.crt and run update-ca-certificates).",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn headers_parse() {
        assert_eq!(parse_header("X-Debug: 1").unwrap(), ("X-Debug".into(), "1".into()));
        assert_eq!(parse_header("Cookie: a=b; c=d").unwrap().1, "a=b; c=d");
        assert!(parse_header("nocolon").is_err());
        assert!(parse_header(": v").is_err());
    }

    #[test]
    fn api_errors_map_to_exit_codes() {
        let api = |code: &str| anyhow::Error::from(ApiError { code: code.into(), message: String::new() });
        assert_eq!(exit_code(&api("out_of_scope")), 4);
        assert_eq!(exit_code(&api("not_found")), 5);
        assert_eq!(exit_code(&api("bad_query")), 2);
        assert_eq!(exit_code(&anyhow::Error::from(NotRunning)), 3);
        assert_eq!(exit_code(&anyhow::anyhow!("boom")), 1);
    }
}
