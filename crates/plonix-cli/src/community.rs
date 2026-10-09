//! Community content: detection rule packs and filter packs.
//!
//! Everything fetched here is untrusted. Downloads are size-capped, must be
//! https (or a local file), and rule packs are validated and pinned by
//! SHA-256 by `plonix_core::rulepack` before anything is installed. Nothing
//! fetched here is ever executed.

use anyhow::{Result, anyhow, bail};
use clap::Subcommand;
use plonix_core::filterpack::{self, FilterLibrary};
use plonix_core::registry::{self, Kind, Location};
use plonix_core::rulepack::{self, Library, MAX_PACK_BYTES};
use serde_json::{Value, json};

use crate::Ctx;
use crate::client::encode;
use crate::render::clip;

#[derive(Subcommand)]
pub enum RulesCmd {
    /// Rule packs in effect: built-in and installed (the default)
    List,
    /// Add a rule pack of your own from a file or an https:// URL, after showing what it does
    Add {
        /// Path or URL of a pack (.json)
        source: String,
        /// Refuse the pack unless its SHA-256 is exactly this
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
        /// Add it without asking (it is still marked Your own)
        #[arg(long)]
        yes: bool,
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
    /// Add a filter pack of your own from a file or an https:// URL, after showing what it does
    Add {
        /// Path or URL of a filter pack (.json)
        source: String,
        /// Refuse the pack unless its SHA-256 is exactly this
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
        /// Add it without asking (it is still marked Your own)
        #[arg(long)]
        yes: bool,
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

// ---- fetching ---------------------------------------------------------------

fn fetch(loc: &Location, max: usize) -> Result<Vec<u8>> {
    registry::fetch(loc, max)
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
            println!("\n{} rules from {} packs · `plonix tech` shows what they detect · `plonix market` for more", total, loaded.packs.len());
            for p in &loaded.problems {
                eprintln!("warning: {p}");
            }
        }
        RulesCmd::Add { source, sha256, yes } => crate::market::add_own(ctx, &source, yes, Some(Kind::Rules), sha256.as_deref())?,
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
        FiltersCmd::Add { source, sha256, yes } => crate::market::add_own(ctx, &source, yes, Some(Kind::Filters), sha256.as_deref())?,
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
            None => println!("No technologies detected yet. Browse the target, or add rules with `plonix market`."),
        }
    } else {
        println!("Evidence ids are exchanges: `plonix show <id>`.");
    }
    Ok(())
}
