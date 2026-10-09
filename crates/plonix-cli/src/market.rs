//! `plonix market`: browse and install skills, rule packs, filter packs,
//! bundles and extensions from a signed catalog. `plonix skills`: the
//! agent skills in effect.
//!
//! The work happens in `plonix_core::market`; this is presentation.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use plonix_core::access::{AgentSettings, Group};
use plonix_core::profile;
use plonix_core::market::{self, Action, Catalog, Change, Market, OpenOptions, Shelf, Status, Trust, TrustLevel, Verification};
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
        #[arg(required_unless_present = "starter")]
        names: Vec<String>,
        /// Install the starter set for your kind of work (see `plonix market recommend`)
        #[arg(long, conflicts_with = "names")]
        starter: bool,
        /// For an extension: also grant a sensitive capability it asks for (read-out-of-scope, run-program); repeatable
        #[arg(long, value_name = "CAPABILITY")]
        grant: Vec<String>,
        /// For an extension update that asks for more than before: approve it
        #[arg(long)]
        yes: bool,
    },
    /// Items that suit your kind of work: bug hunter, red teamer or security researcher
    Recommend {
        /// Show another profile without saving it
        #[arg(long)]
        profile: Option<String>,
    },
    /// Show or set your kind of work (`none` clears it)
    Profile { id: Option<String> },
    /// Install newer versions of everything installed from the Market
    Update,
    /// Remove installed packages; removing a bundle removes what it added
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Add a package from a GitHub repository, a folder, a file or a link; it is marked Your own
    Add {
        /// github:owner/repo[@tag], a path (an extension's folder too), or an https:// address
        source: String,
        /// Add it without asking (it is still marked Your own)
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
    /// Add a skill of your own from a file or an https:// URL, after showing what it does
    Add {
        /// Path or URL of a skill (.md)
        source: String,
        /// Refuse the skill unless its SHA-256 is exactly this
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
        /// Add it without asking (it is still marked Your own)
        #[arg(long)]
        yes: bool,
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
        Kind::Detectors => "detectors",
        Kind::List => "list",
        Kind::Bundle => "bundle",
        Kind::Extension => "ext",
        Kind::Platform => "platform",
        Kind::Tool => "tool",
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

/// A short mark for a table: who stands behind it.
fn trust_mark(v: &Verification) -> &'static str {
    match (v.level, v.shelf) {
        (TrustLevel::BuiltIn, _) => "built-in",
        (TrustLevel::Changed, _) => "! CHANGED",
        (TrustLevel::Verified, Shelf::Official) => "✓ official",
        (TrustLevel::Verified, Shelf::Community) => "community",
        (TrustLevel::Verified, _) => "✓ verified",
        (TrustLevel::Unverified, _) if v.label == "Your own" => "! your own",
        (TrustLevel::Unverified, _) => "! NOT VERIFIED",
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

fn profile_ids() -> String {
    profile::profiles().iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join("|")
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
                println!("\n{unverified} not reviewed: added by you, from an unsigned Market, or changed since install. `plonix market show <name>` says why.");
            }
            if rows.iter().any(|l| l.verification.shelf == Shelf::Community) {
                println!("Community packages are written by their authors and checked automatically; the Plonix maintainers have not reviewed their code.");
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
                    println!("  - {}{}", c.describe_for(mf.program.as_deref()), if c.sensitive() { " (sensitive: only with --grant)" } else { "" });
                }
                match runnable {
                    Some(Ok(())) => println!("{}", market::runtime_note(&mf)),
                    Some(Err(e)) => println!("Not installable in this version: {e}"),
                    None => {}
                }
            }
            if !p.homepage.is_empty() {
                println!("Homepage: {}", p.homepage);
            }
        }
        MarketCmd::Install { starter: true, .. } => {
            let p = profile::current(&ctx.home, None).ok_or_else(|| anyhow!("pick your kind of work first: `plonix market profile <{}>`", profile_ids()))?;
            let cat = market::open(&ctx.home, &opts)?;
            let r = profile::install_starter(&m, &cat, p);
            if ctx.json {
                return ctx.print_json(&json!({ "profile": p.id, "result": r }));
            }
            println!("Starter set for {}:", p.title);
            print_changes(&r.changes, "");
            for w in &r.waiting {
                println!("{} is an extension: see what it may do with `plonix market show {}`, then install it by name.", w.name, w.name);
            }
            for (name, why) in &r.failed {
                println!("{name} was not installed: {why}");
            }
            if r.changes.iter().all(|c| c.action == Action::Unchanged) && r.waiting.is_empty() && r.failed.is_empty() {
                println!("Everything in it is already installed.");
            }
        }
        MarketCmd::Recommend { profile: peek } => {
            let p = match peek.as_deref() {
                Some(id) => profile::get(id).ok_or_else(|| anyhow!("`{id}` is not a profile ({})", profile_ids()))?,
                None => profile::current(&ctx.home, None).ok_or_else(|| anyhow!("pick your kind of work first: `plonix market profile <{}>`", profile_ids()))?,
            };
            let cat = market::open(&ctx.home, &opts)?;
            let r = profile::recommend_from(&m, &cat, p);
            if ctx.json {
                return ctx.print_json(&json!({ "profile": p.id, "recommendation": r }));
            }
            println!("{} · {}", p.title, p.line);
            let section = |title: &str, picks: &[profile::Pick]| {
                if picks.is_empty() {
                    return;
                }
                println!("\n{title}");
                for k in picks {
                    println!("  {:<18} {:<9} {}", k.name, kind_label(k.kind), clip(&k.why, 90));
                }
            };
            section("Starter set", &r.starter);
            section("You already have", &r.included);
            section("Also for you", &r.also);
            if r.starter.is_empty() {
                println!("\nYou have everything in the starter set.");
            } else if peek.is_none() {
                println!("\nInstall the starter set with `plonix market install --starter`.");
            }
        }
        MarketCmd::Profile { id } => {
            if let Some(id) = id {
                let id = if id == "none" { String::new() } else { id };
                profile::set_global(&ctx.home, &id)?;
            }
            let p = profile::current(&ctx.home, None);
            if ctx.json {
                return ctx.print_json(&json!({ "profile": p.map(|p| &p.id), "profiles": profile::summaries() }));
            }
            match p {
                Some(p) => println!("Your work: {} ({}). {}", p.title, p.id, p.line),
                None => println!("Your work is not set. Choose one with `plonix market profile <{}>`.", profile_ids()),
            }
        }
        MarketCmd::Install { names, grant, yes, .. } => {
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
                // Already installed: --grant still gives the yes it was missing.
                if let Some(info) = m.extensions.info(name).filter(|_| changes.iter().any(|c| c.name == *name && c.action == market::Action::Unchanged)) {
                    for c in consent.grant.iter().filter(|c| info.requested.contains(c) && !info.state.granted.contains(c)) {
                        m.extensions.set_granted(name, *c, true)?;
                        if !ctx.json {
                            println!("Allowed {name} to {}.", c.describe_for(info.program.as_ref().map(|p| p.id.as_str())));
                        }
                    }
                }
                all.extend(changes);
            }
            if ctx.json {
                return ctx.print_json(&json!({ "changes": all }));
            }
        }
        MarketCmd::Update => {
            let cat = market::open(&ctx.home, &opts)?;
            let u = m.update(&cat);
            let added = m.added_updates();
            if ctx.json {
                ctx.print_json(&json!({ "changes": u.changes, "failed": u.failed, "added_updates": added }))?;
            } else {
                if u.changes.is_empty() && u.failed.is_empty() {
                    println!("Everything installed from the Market is up to date.");
                }
                print_changes(&u.changes, "");
                for f in &u.failed {
                    eprintln!("{}: not updated: {}", f.name, f.error);
                }
                // Added by you: never updated on their own, so what a new release asks for is seen first.
                for a in &added {
                    match &a.error {
                        Some(e) => eprintln!("{}: could not check for a newer release: {e}", a.name),
                        None => {
                            let cmd = if a.kind == Kind::Extension { "extensions" } else { "market" };
                            println!("{}: release {} is out (you have {}). Look at it with `plonix {cmd} add {}`.", a.name, a.latest, a.installed, a.source)
                        }
                    }
                }
            }
            if !u.failed.is_empty() {
                bail!("{} package(s) could not be updated", u.failed.len());
            }
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
        MarketCmd::Add { source, yes } => add_own(ctx, &source, yes, None, None)?,
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
            // A Market list, or the block list that sits next to one.
            let is_blocklist = serde_json::from_slice::<Value>(&bytes).ok().is_some_and(|v| v.get("plonix_blocked").is_some());
            if is_blocklist {
                plonix_core::blocklist::parse(&bytes).map_err(|e| anyhow!("{}: {e}", index.display()))?;
            } else {
                registry::parse(&bytes).map_err(|e| anyhow!("{}: {e}", index.display()))?;
            }
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
    let mut cat = market::open(&ctx.home, &opts)?;
    // The community Market's key is trusted for that list only, so check it separately.
    if !cat.verified() && market::open_community(index).is_ok() {
        cat.trust = Trust::Verified { key: registry::COMMUNITY_KEY.into(), publisher: registry::COMMUNITY_PUBLISHER.into() };
    }
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

/// Adds a package of your own after showing what it does and who wrote it,
/// and only with `yes`. `skills add`, `rules add` and `filters add` come
/// through here too, so every way in asks the same way and the Market sees
/// the same thing. `want` refuses another kind of file, `sha256` another build.
pub fn add_own(ctx: &Ctx, source: &str, yes: bool, want: Option<Kind>, sha256: Option<&str>) -> Result<()> {
    let m = Market::new(&ctx.home);
    let (bytes, label) = market::read_external(source)?;
    let ext = m.inspect_external(bytes, &label).map_err(|e| anyhow!(e))?;
    if ext.kind == Kind::Extension {
        bail!("that is an extension: add it with `plonix extensions add {source}`, which shows what it asks to do first");
    }
    if let Some(k) = want
        && ext.kind != k
    {
        bail!("that is a {}, not a {}: add it with `plonix market add {source}`", ext.kind.noun(), k.noun());
    }
    if let Some(h) = sha256
        && !h.trim().eq_ignore_ascii_case(&ext.sha256)
    {
        bail!("checksum mismatch: expected sha256 {}, got {}", h.trim(), ext.sha256);
    }
    if !ctx.json {
        println!("{} {} · {} by {}", ext.name, ext.version, ext.kind.noun(), ext.author);
        println!("{}", ext.description);
        for e in &ext.effects {
            println!("  - {e}");
        }
        println!("sha256 {}", ext.sha256);
        println!("! YOUR OWN: it comes from {}, not a signed Market, so nobody has reviewed it. It is validated and cannot run code.", market::describe_source(&ext.source));
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
    println!("It shows as Your own in the Market.");
    Ok(())
}

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
        SkillsCmd::Add { source, sha256, yes } => add_own(ctx, &source, yes, Some(Kind::Skill), sha256.as_deref())?,
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
