//! Community content: detection rule packs and the store.
//!
//! Everything fetched here is untrusted. Downloads are size-capped, must be
//! https (or a local file), and rule packs are validated and pinned by
//! SHA-256 by `plonix_core::rulepack` before anything is installed. Nothing
//! fetched here is ever executed.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use plonix_core::filterpack::{self, FilterLibrary};
use plonix_core::registry::{self, Index, Kind, Location};
use plonix_core::rulepack::{self, BUILTIN, Library, MAX_PACK_BYTES};
use serde_json::{Value, json};

use crate::Ctx;
use crate::client::encode;
use crate::render::clip;

#[derive(Subcommand)]
pub enum RulesCmd {
    /// Rule packs in effect: built-in and installed (the default)
    List,
    /// Install a rule pack from a file or an https:// URL
    Add {
        /// Path or URL of a pack (.json)
        source: String,
        /// Refuse the pack unless its SHA-256 is exactly this
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
    },
    /// Uninstall a rule pack
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Validate a pack without installing it (for pack authors)
    Check {
        /// Path or URL of a pack
        source: String,
    },
}

#[derive(Subcommand)]
pub enum FiltersCmd {
    /// Named filters in effect, for `is:<name>` (the default)
    List,
    /// Install a filter pack from a file or an https:// URL
    Add {
        /// Path or URL of a filter pack (.json)
        source: String,
        /// Refuse the pack unless its SHA-256 is exactly this
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
    },
    /// Uninstall a filter pack
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Validate a filter pack without installing it (for pack authors)
    Check {
        /// Path or URL of a filter pack
        source: String,
    },
}

#[derive(Args)]
pub struct StoreArgs {
    /// Store index to use [default: $PLONIX_STORE_INDEX or the Plonix community index]
    #[arg(long, global = true, value_name = "PATH|URL")]
    index: Option<String>,
    #[command(subcommand)]
    cmd: Option<StoreCmd>,
}

#[derive(Subcommand)]
enum StoreCmd {
    /// Packages in the store, optionally filtered (the default)
    List {
        /// Only show packages whose name or description contains this
        filter: Option<String>,
    },
    /// Install packages from the store (verified by SHA-256)
    Install {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Upgrade installed packs that have a newer version in the store
    Update,
}

// ---- fetching ---------------------------------------------------------------

/// Reads a local file or downloads a URL, refusing anything over `max` bytes.
fn fetch(loc: &Location, max: usize) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match loc {
        Location::File(p) => {
            let f = std::fs::File::open(p).with_context(|| format!("reading {}", p.display()))?;
            f.take(max as u64 + 1).read_to_end(&mut buf).with_context(|| format!("reading {}", p.display()))?;
        }
        Location::Url(u) => {
            let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout(Duration::from_secs(60)).redirects(3);
            // Downloads honour the usual proxy variables; the local API client does not.
            let local = u.starts_with("http://");
            if let Some(p) = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("https_proxy").ok()).filter(|_| !local) {
                b = b.proxy(ureq::Proxy::new(p).context("invalid HTTPS_PROXY")?);
            }
            let resp = b.build().get(u).call().map_err(|e| match e {
                ureq::Error::Status(code, _) => anyhow!("{u}: HTTP {code}"),
                ureq::Error::Transport(t) => anyhow!("{u}: {t}"),
            })?;
            // A redirect must not downgrade to plain http or leave https.
            registry::location(resp.get_url()).map_err(|e| anyhow!("{u} redirected to an unsafe location: {e}"))?;
            resp.into_reader().take(max as u64 + 1).read_to_end(&mut buf).with_context(|| format!("downloading {u}"))?;
        }
    }
    if buf.len() > max {
        bail!("{loc} is larger than {max} bytes; refusing it");
    }
    Ok(buf)
}

fn location(s: &str) -> Result<Location> {
    registry::location(s).map_err(|e| anyhow!(e))
}

/// Records a readable source: URLs as given, files as absolute paths.
fn source_label(loc: &Location) -> String {
    match loc {
        Location::File(p) => std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()).display().to_string(),
        Location::Url(u) => u.clone(),
    }
}

// ---- plonix rules -------------------------------------------------------------

pub fn rules_cmd(ctx: &Ctx, cmd: RulesCmd) -> Result<()> {
    let lib = Library::new(&ctx.home);
    match cmd {
        RulesCmd::List => {
            let loaded = lib.load();
            if ctx.json {
                let packs: Vec<_> = loaded.packs.iter().map(|(_, i)| i).collect();
                return ctx.print_json(&json!({ "packs": packs, "problems": loaded.problems }));
            }
            println!("{:<18} {:<9} {:>5}  SOURCE", "PACK", "VERSION", "RULES");
            let mut total = 0;
            for (_, i) in &loaded.packs {
                total += i.rules;
                println!("{:<18} {:<9} {:>5}  {}", i.name, i.version, i.rules, clip(&i.source, 80));
            }
            println!("\n{} rules from {} packs · `plonix tech` shows what they detect · `plonix store` for more", total, loaded.packs.len());
            for p in &loaded.problems {
                eprintln!("warning: {p}");
            }
        }
        RulesCmd::Add { source, sha256 } => {
            let loc = location(&source)?;
            let bytes = fetch(&loc, MAX_PACK_BYTES)?;
            let (pack, previous) = lib.install(&bytes, &source_label(&loc), sha256.as_deref())?;
            if ctx.json {
                return ctx.print_json(&json!({ "installed": pack.info(&source_label(&loc), false, None), "replaced": previous }));
            }
            let what = match previous {
                Some(v) if v == pack.doc.version => format!("Reinstalled {} {}", pack.doc.name, pack.doc.version),
                Some(v) => format!("Updated {} {} → {}", pack.doc.name, v, pack.doc.version),
                None => format!("Installed {} {}", pack.doc.name, pack.doc.version),
            };
            println!("{what}: {} rules by {}.", pack.rules.len(), pack.doc.author);
            if sha256.is_some() {
                println!("sha256 {} verified.", pack.sha256);
            } else {
                println!("Pinned to sha256 {} (pass --sha256 to require a specific build).", pack.sha256);
            }
            println!("Rules apply to traffic you already captured: see `plonix tech`.");
        }
        RulesCmd::Remove { names } => {
            for name in names {
                if lib.remove(&name)? {
                    println!("Removed {name}.");
                } else {
                    bail!("no installed rule pack named `{}` (see `plonix rules list`)", plonix_core::detect::clean(&name, 64));
                }
            }
        }
        RulesCmd::Check { source } => {
            let loc = location(&source)?;
            let bytes = fetch(&loc, MAX_PACK_BYTES)?;
            let pack = rulepack::parse(&bytes).map_err(|e| anyhow!(e))?;
            if ctx.json {
                return ctx.print_json(&json!({ "valid": true, "pack": pack.info(&source_label(&loc), false, None) }));
            }
            println!("{} {} is valid: {} rules.", pack.doc.name, pack.doc.version, pack.rules.len());
            println!("sha256 {}", pack.sha256);
        }
    }
    Ok(())
}

// ---- plonix filters ------------------------------------------------------------

pub fn filters_cmd(ctx: &Ctx, cmd: FiltersCmd) -> Result<()> {
    let lib = FilterLibrary::new(&ctx.home);
    match cmd {
        FiltersCmd::List => {
            let set = lib.load();
            if ctx.json {
                let filters: Vec<_> = set.filters.values().collect();
                return ctx.print_json(&json!({ "filters": filters, "packs": set.packs, "problems": set.problems }));
            }
            println!("{:<16} {:<22} {:<10} QUERY", "FILTER", "LABEL", "PACK");
            for f in set.filters.values() {
                println!("{:<16} {:<22} {:<10} {}", format!("is:{}", f.id), clip(&f.label, 22), clip(&f.pack, 10), clip(&f.query, 70));
            }
            println!(
                "\n{} filters from {} pack(s) · use them in a search (`plonix search is:graphql -is:trackers`) or from + Filter in the window",
                set.filters.len(),
                set.packs.len()
            );
            for p in &set.problems {
                eprintln!("warning: {p}");
            }
        }
        FiltersCmd::Add { source, sha256 } => {
            let loc = location(&source)?;
            let bytes = fetch(&loc, MAX_PACK_BYTES)?;
            let (pack, previous) = lib.install(&bytes, &source_label(&loc), sha256.as_deref())?;
            if ctx.json {
                return ctx.print_json(&json!({ "installed": pack.info(&source_label(&loc), false), "replaced": previous }));
            }
            let what = match previous {
                Some(v) if v == pack.doc.version => format!("Reinstalled {} {}", pack.doc.name, pack.doc.version),
                Some(v) => format!("Updated {} {} → {}", pack.doc.name, v, pack.doc.version),
                None => format!("Installed {} {}", pack.doc.name, pack.doc.version),
            };
            let ids: Vec<String> = pack.doc.filters.iter().map(|f| format!("is:{}", f.id)).collect();
            println!("{what}: {} filters by {}.", ids.len(), pack.doc.author);
            println!("  {}", clip(&ids.join("  "), 200));
            if sha256.is_some() {
                println!("sha256 {} verified.", pack.sha256);
            } else {
                println!("Pinned to sha256 {} (pass --sha256 to require a specific build).", pack.sha256);
            }
        }
        FiltersCmd::Remove { names } => {
            for name in names {
                if lib.remove(&name)? {
                    println!("Removed {name}.");
                } else {
                    bail!("no installed filter pack named `{}` (see `plonix filters`)", plonix_core::detect::clean(&name, 64));
                }
            }
        }
        FiltersCmd::Check { source } => {
            let loc = location(&source)?;
            let bytes = fetch(&loc, MAX_PACK_BYTES)?;
            let pack = filterpack::parse(&bytes).map_err(|e| anyhow!(e))?;
            if ctx.json {
                return ctx.print_json(&json!({ "valid": true, "pack": pack.info(&source_label(&loc), false) }));
            }
            println!("{} {} is valid: {} filters.", pack.doc.name, pack.doc.version, pack.doc.filters.len());
            println!("sha256 {}", pack.sha256);
        }
    }
    Ok(())
}

// ---- plonix tech ----------------------------------------------------------------

pub fn tech_cmd(ctx: &Ctx, host: Option<String>) -> Result<()> {
    let c = ctx.client()?;
    let hosts: Vec<Value> = match &host {
        Some(h) => vec![c.get(&format!("/api/tech/{}", encode(h.trim())))?],
        None => c.get("/api/tech")?.as_array().cloned().unwrap_or_default(),
    };
    if ctx.json {
        let v = if host.is_some() { hosts.into_iter().next().unwrap_or(Value::Null) } else { Value::Array(hosts) };
        return ctx.print_json(&v);
    }
    let mut shown = 0;
    for h in &hosts {
        let tech = h["tech"].as_array().cloned().unwrap_or_default();
        if tech.is_empty() {
            continue;
        }
        shown += 1;
        println!("{}", h["host"].as_str().unwrap_or(""));
        for t in tech {
            let mut name = t["name"].as_str().unwrap_or("").to_string();
            if let Some(v) = t["version"].as_str() {
                name.push(' ');
                name.push_str(v);
            }
            let ex = t["exchange_id"].as_i64().map(|i| format!(" (#{i})")).unwrap_or_default();
            println!(
                "  {:<14} {:<26} {:>3}%  {}{}",
                t["category"].as_str().unwrap_or(""),
                clip(&name, 26),
                t["confidence"].as_u64().unwrap_or(0),
                clip(t["evidence"].as_str().unwrap_or(""), 70),
                ex
            );
        }
        println!();
    }
    if shown == 0 {
        match host {
            Some(h) => println!("Nothing detected on {h} yet."),
            None => println!("No technologies detected yet. Browse the target, or add rules with `plonix store`."),
        }
    } else {
        println!("Evidence ids are exchanges: `plonix show <id>`.");
    }
    Ok(())
}

// ---- plonix store -----------------------------------------------------------------

fn index_location(a: &StoreArgs) -> Result<Location> {
    let s = a.index.clone().or_else(|| std::env::var("PLONIX_STORE_INDEX").ok()).unwrap_or_else(|| registry::DEFAULT_INDEX.to_string());
    location(&s)
}

fn load_index(loc: &Location) -> Result<Index> {
    let bytes = fetch(loc, registry::MAX_INDEX_BYTES).with_context(|| format!("fetching store index {loc}"))?;
    registry::parse(&bytes).map_err(|e| anyhow!("store index {loc}: {e}"))
}

pub fn store_cmd(ctx: &Ctx, a: StoreArgs) -> Result<()> {
    let loc = index_location(&a)?;
    let index = load_index(&loc)?;
    let shelves = Shelves::new(&ctx.home);
    let status = |p: &registry::Package| -> String {
        if shelves.builtin(p) {
            return "built-in".into();
        }
        if p.kind == Kind::Extension {
            return "needs runtime".into();
        }
        match shelves.installed_version(p) {
            Some(v) if rulepack::newer(&p.version, &v) => format!("update {v}→{}", p.version),
            Some(_) => "installed".into(),
            None => "available".into(),
        }
    };
    match a.cmd.unwrap_or(StoreCmd::List { filter: None }) {
        StoreCmd::List { filter } => {
            let f = filter.unwrap_or_default().to_lowercase();
            let items: Vec<_> = index
                .packages
                .iter()
                .filter(|p| f.is_empty() || p.name.contains(&f) || p.description.to_lowercase().contains(&f))
                .collect();
            if ctx.json {
                let rows: Vec<_> = items.iter().map(|p| json!({ "package": p, "status": status(p) })).collect();
                return ctx.print_json(&json!({ "index": loc.to_string(), "name": index.name, "packages": rows }));
            }
            let title = if index.name.is_empty() { "Store".to_string() } else { index.name.clone() };
            println!("{title} · {loc}\n");
            if items.is_empty() {
                println!("No packages match.");
                return Ok(());
            }
            println!("{:<18} {:<9} {:<6} {:<16} DESCRIPTION", "NAME", "VERSION", "KIND", "STATUS");
            for p in &items {
                let kind = match p.kind {
                    Kind::Rules => "rules",
                    Kind::Filters => "filter",
                    Kind::Extension => "ext",
                };
                println!("{:<18} {:<9} {:<6} {:<16} {}", p.name, p.version, kind, status(p), clip(&p.description, 70));
            }
            println!("\nInstall with `plonix store install <name>`.");
        }
        StoreCmd::Install { names } => {
            let mut results = vec![];
            for name in names {
                let Some(p) = index.packages.iter().find(|p| p.name == name) else {
                    bail!("`{}` is not in the store (see `plonix store list`)", plonix_core::detect::clean(&name, 64));
                };
                if shelves.builtin(p) {
                    println!("{} is built in; nothing to install.", p.name);
                    continue;
                }
                results.push(install(&shelves, &loc, p, ctx.json)?);
            }
            if ctx.json {
                return ctx.print_json(&Value::Array(results));
            }
        }
        StoreCmd::Update => {
            let mut results = vec![];
            for p in &index.packages {
                if p.kind != Kind::Extension
                    && let Some(v) = shelves.installed_version(p)
                    && rulepack::newer(&p.version, &v)
                {
                    results.push(install(&shelves, &loc, p, ctx.json)?);
                }
            }
            if ctx.json {
                return ctx.print_json(&Value::Array(results));
            }
            if results.is_empty() {
                println!("All installed packs are up to date.");
            }
        }
    }
    Ok(())
}

/// Where each kind of store package is installed.
struct Shelves {
    rules: Library,
    filters: FilterLibrary,
}

impl Shelves {
    fn new(home: &plonix_core::paths::Home) -> Self {
        Self { rules: Library::new(home), filters: FilterLibrary::new(home) }
    }

    fn builtin(&self, p: &registry::Package) -> bool {
        let list = match p.kind {
            Kind::Rules => BUILTIN,
            Kind::Filters => filterpack::BUILTIN,
            Kind::Extension => return false,
        };
        list.iter().any(|(n, _)| *n == p.name)
    }

    fn installed_version(&self, p: &registry::Package) -> Option<String> {
        match p.kind {
            Kind::Rules => self.rules.installed_version(&p.name),
            Kind::Filters => self.filters.installed_version(&p.name),
            Kind::Extension => None,
        }
    }
}

fn install(shelves: &Shelves, index: &Location, p: &registry::Package, quiet: bool) -> Result<Value> {
    if p.kind == Kind::Extension {
        bail!(
            "{} is an extension. Extensions need the sandboxed extension runtime, which this version of Plonix does not have yet (see docs/extensions.md).",
            p.name
        );
    }
    let src = registry::resolve(index, &p.url).map_err(|e| anyhow!("{}: {e}", p.name))?;
    let bytes = fetch(&src, MAX_PACK_BYTES).with_context(|| format!("downloading {}", p.name))?;
    // Check that the file is what the store says before touching what is installed.
    let actual = rulepack::sha256_hex(&bytes);
    if actual != p.sha256 {
        bail!("{}: checksum mismatch: the store lists sha256 {}, the download is {actual}. Nothing was installed.", p.name, p.sha256);
    }
    let (name, version) = match p.kind {
        Kind::Rules => rulepack::parse(&bytes).map(|x| (x.doc.name, x.doc.version)).map_err(|e| anyhow!("{}: {e}", p.name))?,
        Kind::Filters => filterpack::parse(&bytes).map(|x| (x.doc.name, x.doc.version)).map_err(|e| anyhow!("{}: {e}", p.name))?,
        Kind::Extension => unreachable!(),
    };
    if name != p.name || version != p.version {
        bail!("{}: the store lists {} {} but the file is {name} {version}", p.name, p.name, p.version);
    }
    let source = src.to_string();
    let installing = || format!("installing {}", p.name);
    let (count, what, sha, previous) = match p.kind {
        Kind::Rules => {
            let (pack, prev) = shelves.rules.install(&bytes, &source, Some(&p.sha256)).with_context(installing)?;
            (pack.rules.len(), "rules", pack.sha256, prev)
        }
        Kind::Filters => {
            let (pack, prev) = shelves.filters.install(&bytes, &source, Some(&p.sha256)).with_context(installing)?;
            (pack.doc.filters.len(), "filters", pack.sha256, prev)
        }
        Kind::Extension => unreachable!(),
    };
    if !quiet {
        match &previous {
            Some(v) => println!("Updated {} {v} → {} ({count} {what}, sha256 verified).", p.name, p.version),
            None => println!("Installed {} {} ({count} {what}, sha256 verified).", p.name, p.version),
        }
    }
    Ok(json!({ "name": p.name, "kind": p.kind, "version": p.version, what: count, "sha256": sha, "replaced": previous }))
}
