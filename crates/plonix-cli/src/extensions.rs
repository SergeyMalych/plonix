//! `plonix extensions`: install, switch on and off, run and remove sandboxed
//! extensions, and pack one for publishing.
//!
//! The work happens in `plonix_core::extension` and the engine; this is
//! presentation. Everything but `run` works without a running engine; an
//! engine picks changes up on its own.

use anyhow::{Result, anyhow, bail};
use clap::Subcommand;
use plonix_core::extension::{self, Capability, Consent, ExtensionLibrary};
use plonix_core::market::{self, Market};
use plonix_core::registry::Kind;
use serde_json::json;

use crate::Ctx;
use crate::client::encode;
use crate::render::clip;

#[derive(Subcommand)]
pub enum ExtensionsCmd {
    /// Installed extensions, whether each is on, and why one was switched off (the default)
    List,
    /// What an installed extension is and what it is allowed to do
    Show { name: String },
    /// Install from a package file, an extension's folder, or an https:// address (marked Your own)
    Add {
        /// A .plonixext package, a folder with plonix-extension.json, github:owner/repo[@tag], or an https:// address
        source: String,
        /// Install without asking again, approving what it asks for
        #[arg(long)]
        yes: bool,
        /// Also grant a sensitive capability it asks for (read-out-of-scope, run-program); repeatable
        #[arg(long, value_name = "CAPABILITY")]
        grant: Vec<String>,
    },
    /// Allow what an installed extension asks for that needs your yes, such as
    /// running its program (all of it, or the capabilities you name)
    Allow {
        name: String,
        /// Only these, e.g. run-program or read-out-of-scope [default: every sensitive one it asks for]
        capabilities: Vec<String>,
        /// Take them back instead
        #[arg(long)]
        revoke: bool,
    },
    /// Switch an extension on (clears why Plonix switched it off)
    Enable { name: String },
    /// Switch an extension off; it stays installed
    Disable { name: String },
    /// Uninstall an extension
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Run an extension over the traffic captured so far, or (for a subdomain
    /// finder) over your accepted scope domains (needs a running engine)
    Run { name: String },
    /// Probe one in-scope endpoint for undocumented query parameters, sending
    /// every request through Plonix's scope-gated path (needs a running engine)
    Probe {
        name: String,
        /// The in-scope endpoint, e.g. https://api.example.com/v1/users
        url: String,
    },
    /// For authors: check a package or folder without installing it
    Check { source: String },
    /// For authors: pack a folder (plonix-extension.json and its module) into one .plonixext file
    Pack {
        /// The extension's folder
        dir: std::path::PathBuf,
        /// Where to write the package [default: <name>.plonixext in the current folder]
        #[arg(long, short = 'o', value_name = "FILE")]
        out: Option<std::path::PathBuf>,
    },
}

fn caps_line(caps: &[Capability]) -> String {
    caps.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
}

fn parse_grants(grant: &[String]) -> Result<Vec<Capability>> {
    grant.iter().map(|g| Capability::parse(g).map_err(|e| anyhow!(e))).collect()
}

pub fn extensions_cmd(ctx: &Ctx, cmd: ExtensionsCmd) -> Result<()> {
    let lib = ExtensionLibrary::new(&ctx.home);
    match cmd {
        ExtensionsCmd::List => {
            let list = lib.list();
            if ctx.json {
                return ctx.print_json(&json!({ "extensions": list }));
            }
            if list.is_empty() {
                println!("No extensions installed. Find some with `plonix market --kind extension`, or add one with `plonix extensions add <file>`.");
                return Ok(());
            }
            let m = Market::new(&ctx.home);
            println!("{:<20} {:<9} {:<10} {:<15} CAN", "NAME", "VERSION", "STATE", "TRUST");
            for i in &list {
                let state = match (i.intact, i.state.enabled, &i.state.disabled_reason) {
                    (false, ..) => "changed",
                    (true, true, _) => "on",
                    (true, false, Some(_)) => "stopped",
                    (true, false, None) => "off",
                };
                let trust = m.verification(Kind::Extension, &i.name).label;
                println!("{:<20} {:<9} {:<10} {:<15} {}", i.name, i.version, state, clip(&trust, 15), caps_line(&i.state.granted));
            }
            for i in list.iter().filter(|i| i.state.disabled_reason.is_some()) {
                println!("\n! {} was switched off. {}", i.name, i.state.disabled_reason.as_deref().unwrap_or(""));
                println!("  Turn it back on with `plonix extensions enable {}`.", i.name);
            }
            if list.iter().any(|i| !i.intact) {
                println!("\nA changed extension is not loaded: its file is no longer the one that was checked. Remove it and install it again.");
            }
        }
        ExtensionsCmd::Show { name } => {
            let i = lib.info(&name).ok_or_else(|| anyhow!("no extension named `{}` is installed (see `plonix extensions`)", clip(&name, 64)))?;
            let v = Market::new(&ctx.home).verification(Kind::Extension, &name);
            if ctx.json {
                return ctx.print_json(&json!({ "extension": i, "verification": v }));
            }
            println!("{} {} · extension by {}", i.name, i.version, i.author);
            println!("{}", i.description);
            println!("State: {}", if i.state.enabled { "on" } else { "off" });
            if let Some(why) = &i.state.disabled_reason {
                println!("  {why}");
            }
            println!("Trust: {} - {}", v.label, v.detail);
            println!("Allowed to:");
            for c in &i.requested {
                let granted = i.state.granted.contains(c);
                println!("  {} {}{}", if granted { "✓" } else { "✗" }, c.describe_for(i.program.as_ref().map(|p| p.id.as_str())), if granted { "" } else { " (not granted)" });
            }
            match &i.program {
                Some(p) => {
                    println!("{}", market::program_note(Some(&p.id)));
                    if p.found {
                        println!("It runs {}, which is installed.", p.id);
                    } else {
                        println!("It runs {}, which is not installed yet: {}", p.id, p.install);
                    }
                }
                None => println!("{}", market::SANDBOX_NOTE),
            }
            println!("Source: {}\nsha256 {}", i.source, i.sha256);
        }
        ExtensionsCmd::Add { source, yes, grant } => {
            let (bytes, label) = market::read_external(&source)?;
            let m = Market::new(&ctx.home);
            let ext = m.inspect_external(bytes, &label).map_err(|e| anyhow!(e))?;
            if ext.kind != Kind::Extension {
                bail!("that is a {}, not an extension; add it with `plonix market add`", ext.kind.noun());
            }
            let grant = parse_grants(&grant)?;
            if !ctx.json {
                println!("{} {} · extension by {}", ext.name, ext.version, ext.author);
                println!("{}", ext.description);
                println!("It asks to:");
                for c in &ext.capabilities {
                    let note = if !c.sensitive {
                        ""
                    } else if grant.contains(&c.id) {
                        " (sensitive; you grant it with --grant)"
                    } else {
                        " (sensitive; not granted unless you add --grant)"
                    };
                    println!("  - {}{note}", c.what);
                }
                println!("{}", ext.effects.last().map(String::as_str).unwrap_or(market::SANDBOX_NOTE));
                println!("sha256 {}", ext.sha256);
                println!("! YOUR OWN: it comes from {}, not a signed Market, so nobody has reviewed its code.", market::describe_source(&ext.source));
            }
            if !yes {
                if ctx.json {
                    return ctx.print_json(&json!({ "added": false, "file": ext }));
                }
                bail!("not installed yet. Read the above, then run it again with --yes to install it");
            }
            let change = m.add_external(&ext, &Consent { grant, approve_new: true })?;
            if ctx.json {
                return ctx.print_json(&json!({ "added": true, "file": ext, "change": change }));
            }
            println!("Installed {} {} and switched it on. A running engine starts using it right away.", change.name, change.version);
        }
        ExtensionsCmd::Allow { name, capabilities, revoke } => {
            let i = lib.info(&name).ok_or_else(|| anyhow!("no extension named `{}` is installed (see `plonix extensions`)", clip(&name, 64)))?;
            let caps = if capabilities.is_empty() { i.requested.iter().copied().filter(|c| c.sensitive()).collect() } else { parse_grants(&capabilities)? };
            if caps.is_empty() {
                println!("{name} asks for nothing that needs a separate yes.");
                return Ok(());
            }
            let program = i.program.as_ref().map(|p| p.id.as_str());
            for c in caps {
                lib.set_granted(&name, c, !revoke)?;
                println!("{} {name} to {}.", if revoke { "No longer allowing" } else { "Allowed" }, c.describe_for(program));
            }
        }
        ExtensionsCmd::Enable { name } => {
            lib.set_enabled(&name, true)?;
            println!("{name} is on.");
        }
        ExtensionsCmd::Disable { name } => {
            lib.set_enabled(&name, false)?;
            println!("{name} is off. It stays installed; `plonix extensions enable {name}` turns it back on.");
        }
        ExtensionsCmd::Remove { names } => {
            let m = Market::new(&ctx.home);
            for name in names {
                if m.extensions.installed_version(&name).is_none() {
                    bail!("no extension named `{}` is installed", clip(&name, 64));
                }
                m.remove(&name)?;
                println!("Removed extension {name}.");
            }
        }
        ExtensionsCmd::Run { name } => {
            let c = ctx.client()?;
            let r = c.post(&format!("/api/extensions/{}/run", encode(&name)), json!({}))?;
            if ctx.json {
                return ctx.print_json(&r);
            }
            if let Some(why) = r["stopped"].as_str().or(r["problem"].as_str()) {
                bail!("{name}: {why}");
            }
            let n = |k: &str| r[k].as_u64().unwrap_or(0);
            let program = lib.info(&name).and_then(|i| i.program).and_then(|p| plonix_core::program::get(&p.id)).map(|p| p.kind);
            match program {
                Some(plonix_core::program::Kind::Enumerate) => {
                    println!("{name} added {} new subdomain(s) to Scope as suggestions. Review them with `plonix scope`.", n("suggested"))
                }
                Some(_) => println!("{name} checked {} new request(s) and found {} secret(s). They show in the Lens.", n("exchanges"), n("notes")),
                None => println!("{name} looked at {} request(s): {} note(s), {} new finding(s) proposed.", n("exchanges"), n("notes"), n("proposed")),
            }
            for l in r["logs"].as_array().into_iter().flatten().filter_map(|l| l.as_str()) {
                println!("  log: {l}");
            }
            if n("skipped") > 0 {
                println!("It skipped {} request(s) to hosts outside scope. Accept their hosts in Scope to include them.", n("skipped"));
            }
            if r["proposed"].as_u64().unwrap_or(0) > 0 {
                println!("Proposed findings are open until you confirm them: `plonix findings`.");
            }
        }
        ExtensionsCmd::Probe { name, url } => {
            let c = ctx.client()?;
            let r = c.post(&format!("/api/extensions/{}/probe", encode(&name)), json!({ "url": url }))?;
            if ctx.json {
                return ctx.print_json(&r);
            }
            let influential = r["influential"].as_array().map(|a| a.len()).unwrap_or(0);
            println!(
                "{} probed {}: sent {} candidate parameter(s); {} changed the response.",
                name,
                r["target"].as_str().unwrap_or(&url),
                r["sent"].as_u64().unwrap_or(0),
                influential
            );
            for p in r["influential"].as_array().into_iter().flatten().filter_map(|p| p.as_str()) {
                println!("  parameter: {p}");
            }
            if r["proposed"].as_bool().unwrap_or(false) {
                println!("Proposed an unconfirmed finding; review it with `plonix findings`.");
            } else if influential > 0 {
                println!("A finding for these was already open; see `plonix findings`.");
            }
        }
        ExtensionsCmd::Check { source } => {
            let (bytes, _) = market::read_external(&source)?;
            let p = extension::parse_package(&bytes).map_err(|e| anyhow!(e))?;
            if ctx.json {
                return ctx.print_json(&json!({ "valid": true, "manifest": p.manifest, "sha256": p.sha256, "module_bytes": p.module.len() }));
            }
            println!("{} {} is valid: it asks for {}.", p.manifest.name, p.manifest.version, caps_line(&p.manifest.capabilities));
            println!("Module {} bytes; package sha256 {}", p.module.len(), p.sha256);
        }
        ExtensionsCmd::Pack { dir, out } => {
            let bytes = extension::read_source(&dir)?;
            let p = extension::parse_package(&bytes).map_err(|e| anyhow!(e))?;
            let out = out.unwrap_or_else(|| format!("{}{}", p.manifest.name, extension::PACKAGE_SUFFIX).into());
            std::fs::write(&out, &bytes)?;
            if ctx.json {
                return ctx.print_json(&json!({ "file": out, "name": p.manifest.name, "version": p.manifest.version, "sha256": p.sha256 }));
            }
            println!("Wrote {} ({} {}).", out.display(), p.manifest.name, p.manifest.version);
            println!("sha256 {}", p.sha256);
        }
    }
    Ok(())
}
