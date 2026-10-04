//! The `plonix` command.
//!
//! Every subcommand except `engine` and `ca` is a client of the engine's
//! local API (see `client`), exactly like the GUI. The MCP server (`mcp`) is
//! another front end over the same `client` and `render` modules.

mod client;
mod community;
mod connect;
mod engine_ctl;
mod findings;
mod market;
mod mcp;
mod open;
mod render;
mod scan;

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
use engine_ctl::StartOptions;

const EXAMPLES: &str = "\
Get started:
  plonix open example.com          start capturing, open a browser at the target and the Plonix window
  plonix ui                        open the Plonix window (Traffic, Lens, Bench, Scope, Map, Findings)
  plonix launcher                  the Start screen in your browser: pick, create and open projects
  plonix start -p shop             open another project; each project runs in a session of its own
  plonix sessions                  projects open right now, with their proxy ports
  plonix -p shop search status:5xx -p picks the project any command talks to
  plonix search host:example.com status:5xx
  plonix show 42
  plonix scope                     review domains Plonix thinks belong in scope
  plonix replay 42 -H 'X-Debug: 1'
  plonix tech                      technologies detected on each host
  plonix findings                  what you found; `plonix findings export -o report.html` for a report
  plonix store                     community detection rule packs
  plonix connect claude            let Claude Code read this project (read-only MCP)

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

    /// Project (name, id or folder) to start or talk to [default: $PLONIX_PROJECT, else the current session]
    #[arg(long, short = 'p', global = true, value_name = "PROJECT")]
    project: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start capturing and open a browser at the target, in one step
    Open(OpenArgs),
    /// Open the Plonix window in your browser (starts the engine if needed)
    Ui(UiArgs),
    /// Start the engine (proxy + API) in the background
    Start(StartArgs),
    /// Close a project's session (the current one, or -p)
    Stop {
        /// Close every open project
        #[arg(long)]
        all: bool,
    },
    /// Show whether the engine is running and what it has captured
    Status,
    /// Projects open right now, each with its own proxy
    Sessions,
    /// List, create and locate projects
    Projects {
        #[command(subcommand)]
        cmd: Option<ProjectsCmd>,
    },
    /// Open the Start screen in your browser: pick, create and open projects
    Launcher {
        /// Print the sign-in link instead of opening it
        #[arg(long)]
        no_open: bool,
    },
    /// Search captured traffic, newest first
    #[command(
        visible_alias = "s",
        after_help = "Filters: host:example.com  method:POST  status:404|5xx|none  path:/api  mime:json  ext:js\n         kind:static  scope:in|out  source:proxy|replay  \"quoted phrase\"  free text\n         is:graphql  is:auth  -is:trackers   named filters from filter packs (`plonix filters`)\nInclude and exclude: a term shows only what matches, -term hides it; commas match any value:\n  plonix search status:4xx,5xx -kind:static -host:cdn.example.com\nPut options such as -n before the query: everything after the first term is search text."
    )]
    Search(SearchArgs),
    /// Print new traffic as it is captured (Ctrl-C to stop)
    Watch {
        /// Only show traffic matching this search
        #[arg(allow_hyphen_values = true)]
        query: Vec<String>,
    },
    /// Show one request and its response (the Lens, in the terminal)
    Show {
        /// Exchange id (the first column of `plonix search`)
        id: i64,
        /// Print bodies in full
        #[arg(long)]
        full: bool,
    },
    /// Re-send a captured request, optionally modified (a Bench send; accepted hosts only)
    Replay(ReplayArgs),
    /// Review and change scope
    Scope {
        #[command(subcommand)]
        cmd: Option<ScopeCmd>,
    },
    /// Hosts seen so far, busiest first
    Hosts,
    /// Technologies detected on each host (from detection rules)
    Tech {
        /// Only this host
        host: Option<String>,
    },
    /// Scan: suggest a profile, list the catalog, run an active scan
    Scan {
        #[command(subcommand)]
        cmd: scan::ScanCmd,
    },
    /// Crawl a host to discover endpoints, parameters and forms
    Crawl(scan::CrawlArgs),
    /// Findings: list, show, add, edit, set status, delete, export a report
    Findings {
        #[command(subcommand)]
        cmd: Option<findings::FindingsCmd>,
    },
    /// Detection rule packs: list, add from a file or URL, remove, check
    Rules {
        #[command(subcommand)]
        cmd: Option<community::RulesCmd>,
    },
    /// Named Traffic filters (is:name): list, add from a file or URL, remove, check
    Filters {
        #[command(subcommand)]
        cmd: Option<community::FiltersCmd>,
    },
    /// The Market: install skills, rules, filters, bundles and extensions from a signed catalog
    #[command(visible_alias = "store")]
    Market(market::MarketArgs),
    /// Agent skills: playbooks agents find through MCP
    Skills {
        #[command(subcommand)]
        cmd: Option<market::SkillsCmd>,
    },
    /// Connect an AI agent to Plonix (read-only)
    Connect {
        #[command(subcommand)]
        cmd: connect::ConnectCmd,
    },
    /// Run the read-only MCP server on stdin/stdout (started by your agent)
    Mcp,
    /// The local certificate authority
    Ca {
        #[command(subcommand)]
        cmd: Option<CaCmd>,
    },
    /// Run the engine in the foreground (used by `plonix start`)
    #[command(hide = true)]
    Engine(EngineArgs),
    /// Run the Start screen in the foreground (used by `plonix launcher`)
    #[command(hide = true)]
    Hub {
        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Subcommand)]
enum ProjectsCmd {
    /// Known projects and their folders (the default)
    List,
    /// Create a project
    New {
        name: String,
        /// The folder the project folder goes in [default: ~/Plonix]
        #[arg(long, value_name = "DIR")]
        location: Option<PathBuf>,
    },
    /// Print a project's folder
    Path { project: String },
    /// Remove a project from the list (its folder is kept)
    Forget { project: String },
}

#[derive(Args)]
struct OpenArgs {
    /// Host name or URL, e.g. example.com or http://localhost:3000
    target: String,
    /// Do not add the target to scope
    #[arg(long)]
    no_scope: bool,
    /// Start the engine but do not open a browser or the Plonix window
    #[arg(long)]
    no_browser: bool,
    /// Do not open the Plonix window
    #[arg(long)]
    no_ui: bool,
    /// Return right away instead of printing traffic as it arrives
    #[arg(long)]
    no_watch: bool,
    #[command(flatten)]
    engine: EngineFlags,
}

#[derive(Args)]
struct UiArgs {
    /// Print the sign-in link instead of opening it
    #[arg(long)]
    no_open: bool,
    #[command(flatten)]
    engine: EngineFlags,
}

#[derive(Args)]
struct StartArgs {
    #[command(flatten)]
    engine: EngineFlags,
}

#[derive(Args, Clone)]
struct EngineFlags {
    /// Proxy port for this session [default: the project's setting, 8080]; a taken port moves to the next free one
    #[arg(long)]
    port: Option<u16>,
    /// Local API port [default: the project's last one, else 8090, else any free port]
    #[arg(long)]
    api_port: Option<u16>,
    /// Accept invalid upstream certificates (self-signed staging hosts)
    #[arg(long)]
    insecure_upstream: bool,
}

#[derive(Args)]
struct EngineArgs {
    #[arg(long)]
    proxy_port: Option<u16>,
    #[arg(long)]
    api_port: Option<u16>,
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
    project: Option<String>,
}

impl Ctx {
    fn client(&self) -> Result<Client> {
        Client::connect_project(&self.home, self.project.as_deref(), "cli")
    }

    fn print_json(&self, v: &Value) -> Result<()> {
        println!("{}", serde_json::to_string_pretty(v)?);
        Ok(())
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let project = cli.project.or_else(|| std::env::var("PLONIX_PROJECT").ok()).filter(|p| !p.trim().is_empty());
    let ctx = Ctx { home: Home::resolve(cli.home.as_deref())?, json: cli.json, project };
    match cli.command {
        Cmd::Open(a) => open_cmd(&ctx, a)?,
        Cmd::Ui(a) => ui_cmd(&ctx, a)?,
        Cmd::Start(a) => start_cmd(&ctx, a)?,
        Cmd::Stop { all } => stop_cmd(&ctx, all)?,
        Cmd::Status => return status_cmd(&ctx),
        Cmd::Sessions => sessions_cmd(&ctx)?,
        Cmd::Projects { cmd } => projects_cmd(&ctx, cmd.unwrap_or(ProjectsCmd::List))?,
        Cmd::Launcher { no_open } => launcher_cmd(&ctx, no_open)?,
        Cmd::Hub { port } => hub_foreground(&ctx, port)?,
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
        Cmd::Tech { host } => community::tech_cmd(&ctx, host)?,
        Cmd::Scan { cmd } => scan::scan_cmd(&ctx, cmd)?,
        Cmd::Crawl(a) => scan::crawl_cmd(&ctx, a)?,
        Cmd::Findings { cmd } => findings::findings_cmd(&ctx, cmd.unwrap_or(findings::FindingsCmd::List { status: None }))?,
        Cmd::Rules { cmd } => community::rules_cmd(&ctx, cmd.unwrap_or(community::RulesCmd::List))?,
        Cmd::Filters { cmd } => community::filters_cmd(&ctx, cmd.unwrap_or(community::FiltersCmd::List))?,
        Cmd::Market(a) => market::market_cmd(&ctx, a)?,
        Cmd::Skills { cmd } => market::skills_cmd(&ctx, cmd.unwrap_or(market::SkillsCmd::List))?,
        Cmd::Connect { cmd } => connect::connect_cmd(&ctx, cmd)?,
        Cmd::Mcp => mcp::serve(ctx.home.clone())?,
        Cmd::Ca { cmd } => ca_cmd(&ctx, cmd.unwrap_or(CaCmd::Show))?,
        Cmd::Engine(a) => engine_ctl::run_foreground(
            ctx.home.clone(),
            &StartOptions {
                project: ctx.project.clone(),
                proxy_port: a.proxy_port,
                api_port: a.api_port,
                insecure_upstream: a.insecure_upstream,
            },
        )?,
    }
    Ok(ExitCode::SUCCESS)
}

fn start_options(project: Option<String>, f: &EngineFlags) -> StartOptions {
    StartOptions { project, proxy_port: f.port, api_port: f.api_port, insecure_upstream: f.insecure_upstream }
}

fn start_cmd(ctx: &Ctx, a: StartArgs) -> Result<()> {
    let opts = start_options(ctx.project.clone(), &a.engine);
    let (c, started) = engine_ctl::start(&ctx.home, &opts)?;
    if ctx.json {
        return ctx.print_json(&json!({ "started": started, "status": c.status }));
    }
    println!("{}", if started { "Plonix engine started." } else { "Plonix engine is already running." });
    print!("{}", render::status(&c.status, &open::tilde(&ctx.home.ca_cert())));
    let others = plonix_core::session::running(&ctx.home).len().saturating_sub(1);
    if others > 0 {
        println!("\n{others} other project(s) open as well; see `plonix sessions`.");
    }
    println!("\nNext: `plonix open <target>` opens a browser that captures through the proxy.");
    Ok(())
}

fn stop_cmd(ctx: &Ctx, all: bool) -> Result<()> {
    let stopped = if all { engine_ctl::stop_all(&ctx.home)? } else { engine_ctl::stop(&ctx.home, ctx.project.as_deref())?.into_iter().collect() };
    if ctx.json {
        return ctx.print_json(&json!({ "stopped": stopped.iter().map(|s| &s["project"]).collect::<Vec<_>>() }));
    }
    if stopped.is_empty() {
        match &ctx.project {
            Some(p) if !all => println!("Project '{p}' is not open."),
            _ => println!("Plonix engine is not running."),
        }
    }
    for s in &stopped {
        println!("Plonix engine stopped (project {}). Captured traffic is kept.", s["project"].as_str().unwrap_or(""));
    }
    let left = plonix_core::session::running(&ctx.home);
    if !all && !left.is_empty() {
        println!("Still open: {}.", left.iter().map(|i| i.project.as_str()).collect::<Vec<_>>().join(", "));
    }
    Ok(())
}

fn sessions_cmd(ctx: &Ctx) -> Result<()> {
    let list = plonix_core::session::running(&ctx.home);
    let current = ctx.home.read_engine_info().map(|i| i.project_id).unwrap_or_default();
    if ctx.json {
        return ctx.print_json(&json!(list));
    }
    if list.is_empty() {
        println!("No project is open. Start one with `plonix start -p <name>` or `plonix open <target>`.");
        return Ok(());
    }
    println!("  {:<24} {:<21} API", "PROJECT", "PROXY");
    for i in &list {
        let mark = if i.project_id == current { "*" } else { " " };
        println!("{mark} {}", engine_ctl::describe(i));
    }
    println!("\n* the current session: commands without -p talk to it.");
    Ok(())
}

fn projects_cmd(ctx: &Ctx, cmd: ProjectsCmd) -> Result<()> {
    use plonix_core::project;
    match cmd {
        ProjectsCmd::List => {
            let list = project::listings(&ctx.home);
            if ctx.json {
                return ctx.print_json(&json!(list));
            }
            if list.is_empty() {
                println!("No projects yet. Create one with `plonix projects new <name>` or `plonix open <target>`.");
                println!("New projects go in {}.", open::tilde(&ctx.home.default_projects_dir()));
                return Ok(());
            }
            for l in &list {
                let state = if !l.available { "missing" } else if l.open { "open" } else { "" };
                println!("{:<24} {:<8} {}", l.entry.name, state, open::tilde(&l.entry.path));
            }
        }
        ProjectsCmd::New { name, location } => {
            let parent = location.map(|l| project::expand_tilde(&l)).unwrap_or_else(|| ctx.home.default_projects_dir());
            let p = project::Project::create(&parent.join(project::slug(&name)), &name)?;
            project::remember(&ctx.home, &p, false)?;
            if ctx.json {
                return ctx.print_json(&json!({ "id": p.id(), "name": p.name(), "path": p.dir }));
            }
            println!("Created project '{}' in {}.\nOpen it with `plonix start -p \"{}\"`.", p.name(), open::tilde(&p.dir), p.name());
        }
        ProjectsCmd::Path { project: sel } => {
            let entry = project::list(&ctx.home)
                .into_iter()
                .find(|e| e.id == sel || e.name.eq_ignore_ascii_case(&sel))
                .with_context(|| format!("no project named '{sel}' (see `plonix projects`)"))?;
            println!("{}", entry.path.display());
        }
        ProjectsCmd::Forget { project: sel } => {
            let entry = project::list(&ctx.home)
                .into_iter()
                .find(|e| e.id == sel || e.name.eq_ignore_ascii_case(&sel))
                .with_context(|| format!("no project named '{sel}' (see `plonix projects`)"))?;
            if plonix_core::session::find(&ctx.home, &entry.id).is_some() {
                bail!("'{}' is open; close it first with `plonix stop -p \"{}\"`", entry.name, entry.name);
            }
            project::forget(&ctx.home, &entry.id)?;
            println!("Removed '{}' from the list. Its folder is still at {}.", entry.name, open::tilde(&entry.path));
        }
    }
    Ok(())
}

/// Opens the Start screen, starting it in the background if needed.
fn launcher_cmd(ctx: &Ctx, no_open: bool) -> Result<()> {
    let url = engine_ctl::launcher_url(&ctx.home)?;
    let opened = !no_open && open::open_url(&url).is_ok();
    if ctx.json {
        return ctx.print_json(&json!({ "url": url, "opened": opened }));
    }
    if opened {
        println!("Opened the Plonix Start screen in your browser.");
    } else {
        println!("Open this link in your browser (it works once, for {} seconds):\n\n  {url}", plonix_core::ui::CODE_TTL.as_secs());
    }
    Ok(())
}

fn hub_foreground(ctx: &Ctx, port: Option<u16>) -> Result<()> {
    engine_ctl::init_logging();
    let home = ctx.home.clone();
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async move {
        let hub = plonix_core::hub::start(&home, port).await?;
        hub.announce()?;
        println!("Plonix Start screen at {}", hub.url());
        tokio::select! {
            _ = hub.stopped() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        hub.shutdown().await;
        Ok(())
    })
}

fn status_cmd(ctx: &Ctx) -> Result<ExitCode> {
    let running = engine_ctl::running(&ctx.home, ctx.project.as_deref(), "cli");
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

    // Without -p: the current session, else a project named after the target.
    let project = ctx.project.clone().or_else(|| engine_ctl::running(&ctx.home, None, "cli").is_none().then(|| target.host.clone()));
    let (c, started) = engine_ctl::start(&ctx.home, &start_options(project, &a.engine))?;
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
                let project_dir = c.status["project_dir"].as_str().map(PathBuf::from);
                let profile = plonix_core::browser::profile_dir(&ctx.home, project_dir.as_deref());
                let args = open::launch(&profile, &b, &proxy, &ca.spki_sha256(), &target.url)?;
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

    let mut ui_url = Value::Null;
    if !a.no_ui && !a.no_browser {
        let url = ui_link(&c.client)?;
        match open::open_url(&url) {
            Ok(()) => say("Window", &format!("http://{}  (opened in your default browser)", c.status["api"].as_str().unwrap_or(""))),
            Err(e) => say("Window", &format!("could not open it ({e}); run `plonix ui`")),
        }
        ui_url = Value::from(url);
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
            "ui": ui_url,
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

/// A one-time link that opens the Plonix window signed in.
fn ui_link(c: &Client) -> Result<String> {
    let v = c.post("/api/ui/launch", json!({}))?;
    v["url"].as_str().map(String::from).context("the engine did not return a UI link")
}

fn ui_cmd(ctx: &Ctx, a: UiArgs) -> Result<()> {
    let (c, started) = engine_ctl::start(&ctx.home, &start_options(ctx.project.clone(), &a.engine))?;
    let url = ui_link(&c.client)?;
    let opened = !a.no_open && open::open_url(&url).is_ok();
    if ctx.json {
        return ctx.print_json(&json!({ "url": url, "opened": opened, "engine_started": started, "status": c.status }));
    }
    if started {
        println!("Plonix engine started (project {}).", c.status["project"].as_str().unwrap_or(""));
    }
    if opened {
        println!("Opened the Plonix window: {}", c.status["api"].as_str().unwrap_or(""));
    } else {
        println!("Open this link in your browser (it works once, for {} seconds):\n\n  {url}", plonix_core::ui::CODE_TTL.as_secs());
    }
    if c.status["exchanges"].as_i64() == Some(0) {
        println!("\nNothing captured yet. Start with `plonix open <target>`, or point a browser at the proxy {}.", c.status["proxy"].as_str().unwrap_or(""));
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
