//! `plonix market`: browse and install skills, rule packs, filter packs,
//! bundles and extensions from a signed catalog. `plonix skills`: the
//! agent skills in effect.
//!
//! The work happens in `plonix_core::market`; this is presentation.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use plonix_core::access::{AgentSettings, Group};
use plonix_core::market::{self, Action, Catalog, Change, Market, OpenOptions, Status, Trust, TrustLevel, Verification};
use plonix_core::registry::{self, Kind};
use plonix_core::settings;
use plonix_core::skill::{self, SkillLibrary};
use serde_json::{Value, json};

use crate::Ctx;
use crate::render::clip;

#[derive(Args)]
pub struct MarketArgs {
    /// Market index to use [default: Settings › Market, $PLONIX_MARKET_INDEX, or the Plonix Market]
    #[arg(long, global = true, value_name = "PATH|URL")]
    index: Option<String>,
    /// Accept an index that is not signed (for testing a Market you are writing)
    #[arg(long, global = true)]
    allow_unsigned: bool,
    /// Only list this kind: skill, rules, filters, bundle or extension
    #[arg(long, global = true)]
    kind: Option<String>,
    #[command(subcommand)]
    cmd: Option<MarketCmd>,
}

#[derive(Subcommand)]
enum MarketCmd {
    /// Everything in the Market, optionally filtered (the default)
    List {
        /// Only show packages whose name or description contains this
        filter: Option<String>,
    },
    /// Details of one package: what it is, what it uses, what it includes
    Show { name: String },
    /// Install packages and what they require (verified before installing)
    Install {
        #[arg(required = true)]
        names: Vec<String>,
        /// For an extension: also grant a sensitive capability it asks for (read-out-of-scope, run-program); repeatable
        #[arg(long, value_name = "CAPABILITY")]
        grant: Vec<String>,
        /// For an extension update that asks for more than before: approve it
        #[arg(long)]
        yes: bool,
    },
    /// Install newer versions of everything installed from the Market
    Update,
    /// Remove installed packages; removing a bundle removes what it added
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Add a skill, rule pack, filter pack or extension from a file or link, marked Not verified
    Add {
        /// Path or https:// address of the file
        source: String,
        /// Add it without asking (it is still marked Not verified)
        #[arg(long)]
        yes: bool,
    },
    /// Trust another Market's publisher key (adds it to Settings › Market)
    Trust { key: String },
    /// For Market authors: validate an index and every package it lists
    Check {
        /// Path or URL of an index.json
        #[arg(value_name = "INDEX")]
        catalog: String,
    },
    /// For Market authors: create a signing key
    Keygen {
        /// Where to write the private key (kept secret, never commit it)
        file: std::path::PathBuf,
    },
    /// For Market authors: sign an index, writing <index>.sig next to it
    Sign {
        /// Path of the index.json to sign
        #[arg(value_name = "INDEX")]
        catalog: std::path::PathBuf,
        /// The private key file from `plonix market keygen`
        #[arg(long, value_name = "FILE")]
        key: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
pub enum SkillsCmd {
    /// Skills agents can use, and the ones switched off (the default)
    List,
    /// A skill's instructions, as an agent receives them
    Show {
        name: String,
        /// Fill in an argument, as name=value (repeatable)
        #[arg(long = "arg", value_name = "NAME=VALUE")]
        args: Vec<String>,
    },
    /// Install a skill from a file or an https:// URL
    Add {
        /// Path or URL of a skill (.md)
        source: String,
        /// Refuse the skill unless its SHA-256 is exactly this
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
    },
    /// Uninstall a skill
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Validate a skill without installing it (for skill authors)
    Check {
        /// Path or URL of a skill
        source: String,
    },
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Skill => "skill",
        Kind::Rules => "rules",
        Kind::Filters => "filters",
        Kind::List => "list",
        Kind::Bundle => "bundle",
        Kind::Extension => "ext",
    }
}

fn status_label(s: &Status) -> String {
    match s {
        Status::BuiltIn => "built-in".into(),
        Status::Available => "available".into(),
        Status::Installed { .. } => "installed".into(),
        Status::Update { installed } => format!("update {installed}→"),
        Status::NeedsRuntime => "coming soon".into(),
    }
}

/// A short mark for a table: ✓ verified, ! not verified or changed.
fn trust_mark(v: &Verification) -> &'static str {
    match v.level {
        TrustLevel::BuiltIn => "built-in",
        TrustLevel::Verified => "✓ verified",
        TrustLevel::Unverified => "! NOT VERIFIED",
        TrustLevel::Changed => "! CHANGED",
    }
}

fn trust_line(c: &Catalog) -> String {
    match &c.trust {
        Trust::Verified { publisher, .. } => format!("signed by {publisher}"),
        Trust::Unverified => "NOT SIGNED: packages are checked against the index, but nobody vouches for the index".into(),
    }
}

fn print_changes(changes: &[Change], requested: &str) {
    for c in changes {
        let what = c.kind.noun();
        match c.action {
            Action::Installed if c.kind == Kind::Bundle => println!("Installed {} {} (bundle).", c.name, c.version),
            Action::Installed if c.kind == Kind::Extension => {
                println!("Installed {} {} ({what}, sha256 verified) and switched it on; `plonix extensions show {}` says what it may do.", c.name, c.version, c.name)
            }
            Action::Installed => println!("Installed {} {} ({what}, sha256 verified).", c.name, c.version),
            Action::Updated => println!("Updated {} {} → {} ({what}, sha256 verified).", c.name, c.from.as_deref().unwrap_or("?"), c.version),
            Action::Removed => println!("Removed {} ({what}).", c.name),
            Action::Unchanged if c.name == requested => println!("{} {} is already installed.", c.name, c.version),
            Action::Unchanged => {}
        }
    }
}

pub fn market_cmd(ctx: &Ctx, a: MarketArgs) -> Result<()> {
    let opts = OpenOptions { index: a.index.clone(), allow_unsigned: a.allow_unsigned };
    let m = Market::new(&ctx.home);
    match a.cmd.unwrap_or(MarketCmd::List { filter: None }) {
        MarketCmd::List { filter } => {
            let cat = market::open(&ctx.home, &opts)?;
            let kind = a.kind.as_deref().map(parse_kind).transpose()?;
            let f = filter.unwrap_or_default().to_lowercase();
            let rows: Vec<_> = m
                .listing(&cat)
                .into_iter()
                .filter(|l| kind.is_none_or(|k| l.package.kind == k))
                .filter(|l| f.is_empty() || l.package.name.contains(&f) || l.package.description.to_lowercase().contains(&f))
                .collect();
            if ctx.json {
                return ctx.print_json(&json!({
                    "index": cat.location(), "name": cat.index.name, "trust": cat.trust,
                    "offline_reason": cat.offline_reason, "packages": rows,
                }));
            }
            let title = if cat.index.name.is_empty() { "Market".to_string() } else { cat.index.name.clone() };
            println!("{title} · {} · {}", cat.location(), trust_line(&cat));
            if let Some(why) = &cat.offline_reason {
                println!("Showing the copy built into Plonix: the online Market could not be used ({}).", clip(why, 140));
            }
            println!();
            if rows.is_empty() {
                println!("Nothing matches.");
                return Ok(());
            }
            println!("{:<18} {:<8} {:<9} {:<15} {:<15} DESCRIPTION", "NAME", "KIND", "VERSION", "STATUS", "TRUST");
            for l in &rows {
                let mut status = status_label(&l.status);
                if status.ends_with('→') {
                    status.push_str(&l.package.version);
                }
                println!(
                    "{:<18} {:<8} {:<9} {:<15} {:<15} {}",
                    l.package.name,
                    kind_label(l.package.kind),
                    l.package.version,
                    status,
                    trust_mark(&l.verification),
                    clip(&l.package.description, 60)
                );
            }
            let unverified = rows.iter().filter(|l| matches!(l.verification.level, TrustLevel::Unverified | TrustLevel::Changed)).count();
            if unverified > 0 {
                println!("\n{unverified} not verified: added by hand, from an unsigned Market, or changed since install. `plonix market show <name>` says why.");
            }
            println!("\nDetails with `plonix market show <name>`, install with `plonix market install <name>`.");
        }
        MarketCmd::Show { name } => {
            let cat = market::open(&ctx.home, &opts)?;
            let l = m.listing(&cat).into_iter().find(|l| l.package.name == name).ok_or_else(|| anyhow!("`{name}` is not in the Market"))?;
            let p = &l.package;
            let file = (p.kind != Kind::Bundle && !l.local).then(|| cat.fetch(p)).transpose()?;
            let skill = match (&file, p.kind) {
                (Some(b), Kind::Skill) => skill::parse(b).ok(),
                _ => None,
            };
            let manifest = match (&file, p.kind) {
                (Some(b), Kind::Extension) => market::extension_manifest(b).ok(),
                _ => None,
            };
            let runnable = match (&file, p.kind) {
                (Some(b), Kind::Extension) => Some(market::extension_runnable(b)),
                _ => None,
            };
            if ctx.json {
                return ctx.print_json(&json!({
                    "package": l, "trust": cat.trust, "skill": skill.as_ref().map(|s| json!({ "skill": s, "instructions": s.instructions })),
                    "extension": manifest,
                }));
            }
            println!("{} {} · {} by {}", p.name, p.version, p.kind.noun(), p.author);
            println!("{}", p.description);
            println!("Status: {}{}", status_label(&l.status), if matches!(l.status, Status::Update { .. }) { p.version.as_str() } else { "" });
            println!("Trust: {} - {}", l.verification.label, l.verification.detail);
            if !l.includes.is_empty() {
                println!("Includes: {}", l.includes.join(", "));
            }
            if let Some(s) = skill {
                let uses: Vec<&str> = s.uses.iter().map(|g| group_label(*g)).collect();
                println!("Uses: {} (read-only)", uses.join(", "));
                for arg in &s.arguments {
                    println!("Argument: {}{} - {}", arg.name, if arg.required { "" } else { " (optional)" }, arg.description);
                }
                println!("\n{}", clip(&s.instructions, 1200));
            }
            if let Some(mf) = manifest {
                println!("Would be allowed to:");
                for c in &mf.capabilities {
                    println!("  - {}{}", c.describe(), if c.sensitive() { " (sensitive: only with --grant)" } else { "" });
                }
                match runnable {
                    Some(Ok(())) => println!("{}", market::SANDBOX_NOTE),
                    Some(Err(e)) => println!("Not installable in this version: {e}"),
                    None => {}
                }
            }
            if !p.homepage.is_empty() {
                println!("Homepage: {}", p.homepage);
            }
        }
        MarketCmd::Install { names, grant, yes } => {
            let cat = market::open(&ctx.home, &opts)?;
            if !cat.verified() {
                eprintln!("warning: installing from an unsigned Market index ({})", cat.location());
            }
            let grant = grant.iter().map(|g| plonix_core::extension::Capability::parse(g).map_err(|e| anyhow!(e))).collect::<Result<Vec<_>>>()?;
            let consent = plonix_core::extension::Consent { grant, approve_new: yes };
            let mut all = vec![];
            for name in &names {
                let changes = m.install_with(&cat, name, &consent)?;
                if !ctx.json {
                    print_changes(&changes, name);
                }
                all.extend(changes);
            }
            if ctx.json {
                return ctx.print_json(&json!({ "changes": all }));
            }
        }
        MarketCmd::Update => {
            let cat = market::open(&ctx.home, &opts)?;
            let changes = m.update(&cat)?;
            if ctx.json {
                return ctx.print_json(&json!({ "changes": changes }));
            }
            if changes.is_empty() {
                println!("Everything installed from the Market is up to date.");
            }
            print_changes(&changes, "");
        }
        MarketCmd::Remove { names } => {
            let mut all = vec![];
            for name in &names {
                let changes = m.remove(name)?;
                if !ctx.json {
                    print_changes(&changes, name);
                }
                all.extend(changes);
            }
            if ctx.json {
                return ctx.print_json(&json!({ "changes": all }));
            }
        }
        MarketCmd::Add { source, yes } => {
            let loc = registry::location(&source).map_err(|e| anyhow!(e))?;
            let bytes = match &loc {
                registry::Location::File(p) => plonix_core::extension::read_source(p)?,
                registry::Location::Url(_) => registry::fetch(&loc, market::max_bytes(Kind::Extension))?,
            };
            let label = match &loc {
                registry::Location::File(p) => std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()).display().to_string(),
                registry::Location::Url(u) => u.clone(),
            };
            let ext = m.inspect_external(bytes, &label).map_err(|e| anyhow!(e))?;
            if ext.kind == Kind::Extension {
                bail!("that is an extension: add it with `plonix extensions add {source}`, which shows what it asks to do first");
            }
            if !ctx.json {
                println!("{} {} · {} by {}", ext.name, ext.version, ext.kind.noun(), ext.author);
                println!("{}", ext.description);
                for e in &ext.effects {
                    println!("  - {e}");
                }
                println!("sha256 {}", ext.sha256);
                println!("! NOT VERIFIED: it did not come from a signed Market, so nobody has vouched for it. It is validated and cannot run code.");
            }
            if !yes {
                if ctx.json {
                    return ctx.print_json(&json!({ "added": false, "file": ext }));
                }
                bail!("not added yet. Read the above, then run it again with --yes to add it");
            }
            let change = m.add_external(&ext, &Default::default())?;
            if ctx.json {
                return ctx.print_json(&json!({ "added": true, "file": ext, "change": change }));
            }
            print_changes(&[change], &ext.name);
            println!("It shows as Not verified in the Market.");
        }
        MarketCmd::Trust { key } => {
            registry::parse_public_key(&key).map_err(|e| anyhow!(e))?;
            let mut values = settings::global(&ctx.home, market::SETTINGS);
            let mut keys: Vec<Value> = values.get("trusted_keys").and_then(Value::as_array).cloned().unwrap_or_default();
            if !keys.iter().any(|k| k.as_str() == Some(key.trim())) {
                keys.push(json!(key.trim()));
            }
            values.insert("trusted_keys".into(), Value::Array(keys));
            settings::save_global(&ctx.home, market::SETTINGS, &values)?;
            println!("Markets signed by {} are now trusted (Settings › Market).", key.trim());
        }
        MarketCmd::Check { catalog } => check_index(ctx, &catalog)?,
        MarketCmd::Keygen { file } => {
            if file.exists() {
                bail!("{} already exists; refusing to overwrite a key", file.display());
            }
            let (private, public) = registry::generate_key().map_err(|e| anyhow!(e))?;
            plonix_core::paths::write_private(&file, format!("{private}\n").as_bytes())?;
            println!("Private key written to {} (readable only by you). Keep it secret and out of version control.", file.display());
            println!("Public key: {public}");
            println!("People trust your Market with `plonix market trust {public}`.");
        }
        MarketCmd::Sign { catalog: index, key } => {
            let private = std::fs::read_to_string(&key).with_context(|| format!("reading {}", key.display()))?;
            let bytes = std::fs::read(&index).with_context(|| format!("reading {}", index.display()))?;
            registry::parse(&bytes).map_err(|e| anyhow!("{}: {e}", index.display()))?;
            let sig = registry::sign(&bytes, &private).map_err(|e| anyhow!(e))?;
            let out = registry::signature_location(&registry::Location::File(index.clone()));
            let registry::Location::File(out) = out else { unreachable!() };
            std::fs::write(&out, format!("{}\n", serde_json::to_string_pretty(&sig)?))?;
            println!("Signed {} with {}; wrote {}.", index.display(), sig.key, out.display());
        }
    }
    Ok(())
}

fn parse_kind(s: &str) -> Result<Kind> {
    let s = s.trim().to_ascii_lowercase();
    let s = match s.as_str() {
        "skills" => "skill",
        "rule" | "rule-pack" => "rules",
        "filter" | "filter-pack" => "filters",
        "bundles" => "bundle",
        "ext" | "extensions" => "extension",
        other => other,
    };
    Kind::ALL.iter().copied().find(|k| k.as_str() == s).ok_or_else(|| anyhow!("unknown kind `{s}`: use skill, rules, filters, bundle or extension"))
}

/// Validates an index and every package it lists, and reports its signature.
fn check_index(ctx: &Ctx, index: &str) -> Result<()> {
    let opts = OpenOptions { index: Some(index.to_string()), allow_unsigned: true };
    let cat = market::open(&ctx.home, &opts)?;
    let mut problems = vec![];
    for p in cat.index.packages.iter().filter(|p| p.kind != Kind::Bundle) {
        if let Err(e) = cat.fetch(p) {
            problems.push(format!("{e:#}"));
        }
    }
    if ctx.json {
        return ctx.print_json(&json!({ "packages": cat.index.packages.len(), "trust": cat.trust, "problems": problems }));
    }
    println!("{}: {} packages, {}.", cat.location(), cat.index.packages.len(), trust_line(&cat));
    for p in &problems {
        println!("  ✗ {p}");
    }
    if !problems.is_empty() {
        bail!("{} package(s) do not match the index", problems.len());
    }
    println!("Every package matches its checksum and validates.");
    if !cat.verified() {
        println!("Sign it with `plonix market sign {index} --key <file>` before publishing.");
    }
    Ok(())
}

fn group_label(g: Group) -> &'static str {
    match g {
        Group::Basics => "basics",
        Group::Traffic => "traffic",
        Group::Insights => "insights",
        Group::Map => "map",
        Group::Scope => "scope",
        Group::Findings => "findings",
        Group::Scan => "scan",
        Group::Bench => "bench",
    }
}

// ---- plonix skills ---------------------------------------------------------------

pub fn skills_cmd(ctx: &Ctx, cmd: SkillsCmd) -> Result<()> {
    let lib = SkillLibrary::new(&ctx.home);
    let settings = AgentSettings::load(&ctx.home);
    match cmd {
        SkillsCmd::List => {
            let loaded = lib.load();
            let infos = loaded.infos(&settings);
            if ctx.json {
                return ctx.print_json(&json!({ "skills": infos, "problems": loaded.problems }));
            }
            println!("{:<18} {:<9} {:<28} {:<22} {:<15} AGENTS", "SKILL", "VERSION", "TITLE", "USES", "TRUST");
            for i in &infos {
                let uses: Vec<&str> = i.skill.uses.iter().map(|g| group_label(*g)).collect();
                let state = if i.available {
                    "available".to_string()
                } else {
                    let off: Vec<&str> = i.missing.iter().map(|g| group_label(*g)).collect();
                    format!("off: needs {}", off.join(", "))
                };
                let trust = trust_mark(&Market::new(&ctx.home).verification(Kind::Skill, &i.skill.name));
                println!("{:<18} {:<9} {:<28} {:<22} {:<15} {}", i.skill.name, i.skill.version, clip(&i.skill.title, 28), clip(&uses.join(","), 22), trust, state);
            }
            println!(
                "\nAgents connected with `plonix connect claude` see these as prompts and through the get_skill tool. \
                 More in `plonix market --kind skill`."
            );
            for p in &loaded.problems {
                eprintln!("warning: {p}");
            }
        }
        SkillsCmd::Show { name, args } => {
            let loaded = lib.load();
            let (s, builtin, source) = loaded.get(&name).ok_or_else(|| anyhow!("no skill named `{name}` (see `plonix skills`)"))?;
            let mut map = serde_json::Map::new();
            for a in args {
                let (k, v) = a.split_once('=').ok_or_else(|| anyhow!("--arg takes name=value"))?;
                map.insert(k.trim().to_string(), json!(v));
            }
            let missing = s.missing(&settings);
            if ctx.json {
                return ctx.print_json(&json!({ "skill": skill::info(s, *builtin, source, &settings, true), "rendered": s.render(&map).ok() }));
            }
            if !missing.is_empty() {
                let off: Vec<&str> = missing.iter().map(|g| group_label(*g)).collect();
                eprintln!("note: agents are not offered this skill: it uses {}, which is switched off in Settings › AI agents.", off.join(", "));
            }
            let text = if s.arguments.iter().any(|a| a.required && !map.contains_key(&a.name)) {
                s.instructions.clone()
            } else {
                s.render(&map).map_err(|e| anyhow!(e))?
            };
            println!("{text}");
        }
        SkillsCmd::Add { source, sha256 } => {
            let loc = registry::location(&source).map_err(|e| anyhow!(e))?;
            let bytes = registry::fetch(&loc, skill::MAX_SKILL_BYTES)?;
            let label = match &loc {
                registry::Location::File(p) => std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()).display().to_string(),
                registry::Location::Url(u) => u.clone(),
            };
            let (s, previous) = lib.install(&bytes, &label, sha256.as_deref())?;
            if ctx.json {
                return ctx.print_json(&json!({ "installed": s, "replaced": previous }));
            }
            match previous {
                Some(v) if v != s.version => println!("Updated skill {} {v} → {}.", s.name, s.version),
                Some(_) => println!("Reinstalled skill {} {}.", s.name, s.version),
                None => println!("Installed skill {} {}: {}.", s.name, s.version, s.title),
            }
            if !s.missing(&settings).is_empty() {
                println!("Agents will not be offered it until the capabilities it uses are switched on in Settings › AI agents.");
            }
        }
        SkillsCmd::Remove { names } => {
            for name in names {
                if lib.remove(&name)? {
                    println!("Removed skill {name}.");
                } else {
                    bail!("no installed skill named `{}` (see `plonix skills`)", plonix_core::detect::clean(&name, 64));
                }
            }
        }
        SkillsCmd::Check { source } => {
            let loc = registry::location(&source).map_err(|e| anyhow!(e))?;
            let bytes = registry::fetch(&loc, skill::MAX_SKILL_BYTES)?;
            let s = skill::parse(&bytes).map_err(|e| anyhow!(e))?;
            if ctx.json {
                return ctx.print_json(&json!({ "valid": true, "skill": s }));
            }
            println!("{} {} is valid: {}.", s.name, s.version, s.title);
            println!("sha256 {}", s.sha256);
        }
    }
    Ok(())
}
