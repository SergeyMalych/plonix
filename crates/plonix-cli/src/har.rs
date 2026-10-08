//! `plonix har`: export traffic as a HAR file, import one into the project.
//! `plonix certs`: client certificates presented to servers that ask for one.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use clap::Subcommand;
use serde_json::{Value, json};

use crate::Ctx;
use crate::client::encode;

#[derive(Subcommand)]
pub enum HarCmd {
    /// Write captured traffic to a HAR 1.2 file: everything, or what a search matches
    #[command(after_help = "Examples:\n  plonix har export -o all.har\n  plonix har export host:example.com status:5xx -o errors.har\n  plonix har export --ids 12,14,20 -o picked.har")]
    Export {
        /// A Traffic search (as in `plonix search`); everything when left out
        #[arg(allow_hyphen_values = true)]
        query: Vec<String>,
        /// Only these exchanges (comma-separated ids); the search is ignored
        #[arg(long, value_delimiter = ',')]
        ids: Vec<i64>,
        /// The file to write; - writes to standard output (may also follow the search)
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Load a HAR file into the project as imported traffic (entries it has already are skipped)
    Import {
        /// The .har file
        file: PathBuf,
    },
}

pub fn har_cmd(ctx: &Ctx, cmd: HarCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        HarCmd::Export { mut query, ids, output } => {
            // The search takes any words, -exclusions included, so `-o FILE`
            // written after it arrives as part of it.
            let output = match output.or_else(|| take_output(&mut query)) {
                Some(o) => o,
                None => bail!("give the file to write with -o FILE (or -o - for standard output)"),
            };
            let path = if ids.is_empty() {
                format!("/api/har?q={}", encode(&query.join(" ")))
            } else {
                format!("/api/har?ids={}", ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","))
            };
            if output.as_os_str() == "-" {
                c.download(&path, &mut std::io::stdout().lock())?;
                return Ok(());
            }
            // Written next to the target, then moved, so a failed export never leaves half a file.
            let part = output.with_extension("har.part");
            let mut file = std::io::BufWriter::new(std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?);
            let written = c.download(&path, &mut file).and_then(|n| {
                use std::io::Write;
                file.flush()?;
                Ok(n)
            });
            drop(file);
            let bytes = match written {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    return Err(e);
                }
            };
            std::fs::rename(&part, &output).with_context(|| format!("writing {}", output.display()))?;
            let entries = count_entries(&output);
            if ctx.json {
                return ctx.print_json(&json!({ "path": output, "entries": entries, "bytes": bytes }));
            }
            match entries {
                Some(n) => println!("Wrote {n} request(s) to {}.", output.display()),
                None => println!("Wrote {}.", output.display()),
            }
        }
        HarCmd::Import { file } => {
            let abs = std::fs::canonicalize(&file).with_context(|| format!("reading {}", file.display()))?;
            let v = c.post_bytes(&format!("/api/har/import?path={}", encode(&abs.to_string_lossy())), b"")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            print!("{}", import_summary(&v));
        }
    }
    Ok(())
}

/// Takes `-o FILE`, `--output FILE` or `--output=FILE` out of the search words.
fn take_output(query: &mut Vec<String>) -> Option<PathBuf> {
    if let Some(i) = query.iter().position(|w| w.starts_with("--output=")) {
        return Some(PathBuf::from(&query.remove(i)["--output=".len()..]));
    }
    let i = query.iter().position(|w| w == "-o" || w == "--output")?;
    if i + 1 >= query.len() {
        return None;
    }
    let file = query.remove(i + 1);
    query.remove(i);
    Some(PathBuf::from(file))
}

/// How many entries a HAR file holds, read back without loading it whole.
fn count_entries(path: &std::path::Path) -> Option<usize> {
    let file = std::fs::File::open(path).ok()?;
    let mut n = 0;
    plonix_core::har::read_entries(file, |_| {
        n += 1;
        Ok(())
    })
    .ok()?;
    Some(n)
}

pub fn import_summary(v: &Value) -> String {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    let mut out = format!("Imported {} request(s)", n("imported"));
    if n("duplicates") > 0 {
        out.push_str(&format!("; {} already in the project", n("duplicates")));
    }
    if n("skipped") > 0 {
        out.push_str(&format!("; {} could not be read", n("skipped")));
    }
    out.push_str(".\n");
    for p in v["problems"].as_array().into_iter().flatten() {
        out.push_str(&format!("  {}\n", p.as_str().unwrap_or("")));
    }
    if n("imported") > 0 {
        out.push_str("See them with `plonix search source:import`.\n");
    }
    out
}

#[derive(Subcommand)]
pub enum CertsCmd {
    /// The project's client certificates (the default)
    #[command(visible_alias = "ls")]
    List,
    /// Add a certificate for a host (*.example.com covers the domain and its subdomains)
    #[command(after_help = "Examples:\n  plonix certs add api.example.com --cert client.pem --key client-key.pem\n  plonix certs add '*.example.com' --cert bundle.pem\n  plonix certs add api.example.com --p12 client.p12 --password-env P12_PASSWORD")]
    Add {
        /// The host it is for, or *.domain
        host: String,
        /// PEM certificate chain (may hold the key too)
        #[arg(long, value_name = "FILE", conflicts_with = "p12")]
        cert: Option<PathBuf>,
        /// PEM private key, unencrypted
        #[arg(long, value_name = "FILE", requires = "cert")]
        key: Option<PathBuf>,
        /// PKCS#12 file (.p12 or .pfx) holding the certificate and key
        #[arg(long, value_name = "FILE")]
        p12: Option<PathBuf>,
        /// Read the .p12 password from this environment variable [default: ask]
        #[arg(long, value_name = "VAR")]
        password_env: Option<String>,
        /// A short note shown with the certificate
        #[arg(long)]
        note: Option<String>,
    },
    /// Remove a certificate
    #[command(visible_alias = "rm")]
    Remove { id: i64 },
}

pub fn certs_cmd(ctx: &Ctx, cmd: CertsCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        CertsCmd::List => {
            let v = c.get("/api/client-certs")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            let certs = v["certs"].as_array().cloned().unwrap_or_default();
            if certs.is_empty() {
                println!("No client certificates. Add one with `plonix certs add api.example.com --cert client.pem --key client-key.pem`.");
                return Ok(());
            }
            if v["enabled"] == false {
                println!("Client certificates are switched off in Settings › Client certificates; none is presented.\n");
            }
            for cert in &certs {
                println!("{}", describe(cert));
            }
        }
        CertsCmd::Add { host, cert, key, p12, password_env, note } => {
            let mut body = json!({ "host": host, "note": note.unwrap_or_default() });
            match (cert, p12) {
                (Some(cert), _) => {
                    body["cert_pem"] = json!(read_text(&cert)?);
                    if let Some(key) = key {
                        body["key_pem"] = json!(read_text(&key)?);
                    }
                }
                (None, Some(p12)) => {
                    let bytes = std::fs::read(&p12).with_context(|| format!("reading {}", p12.display()))?;
                    let password = match password_env {
                        Some(var) => std::env::var(&var).with_context(|| format!("the environment variable {var} is not set"))?,
                        None => ask_secret(&format!("Password for {}", p12.display()))?,
                    };
                    body["pkcs12_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
                    body["password"] = json!(password);
                }
                (None, None) => bail!("give --cert (and --key), or --p12"),
            }
            let v = c.post("/api/client-certs", body)?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Added certificate {} for {}. Plonix presents it when {} asks for one.", v["id"], v["host"].as_str().unwrap_or(""), v["host"].as_str().unwrap_or(""));
            println!("{}", describe(&v));
        }
        CertsCmd::Remove { id } => {
            let v = c.delete(&format!("/api/client-certs/{id}"))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Removed certificate {id}.");
        }
    }
    Ok(())
}

fn read_text(path: &std::path::Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Asks for the .p12 password on the terminal, without echoing it where possible.
/// Reads a secret from the terminal without echoing it (or a line from a pipe).
pub(crate) fn ask_secret(prompt: &str) -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    eprint!("{prompt}: ");
    std::io::stderr().flush()?;
    let quiet = std::process::Command::new("stty").arg("-echo").stdin(std::process::Stdio::inherit()).status().is_ok_and(|s| s.success());
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if quiet {
        let _ = std::process::Command::new("stty").arg("echo").stdin(std::process::Stdio::inherit()).status();
        eprintln!();
    }
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn describe(c: &Value) -> String {
    let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
    let until = c["not_after"].as_i64().filter(|t| *t > 0).map(|t| plonix_core::har::iso_time(t)[..10].to_string()).unwrap_or_default();
    let mut flags = vec![];
    if c["expired"] == true {
        flags.push("expired".to_string());
    }
    if let Some(p) = c["problem"].as_str() {
        flags.push(format!("cannot be used: {p}"));
    }
    let flags = if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join(", ")) };
    let note = if s("note").is_empty() { String::new() } else { format!("  # {}", s("note")) };
    format!("{:>4}  {:<28} {}  until {until}  {}{flags}{note}", c["id"].as_i64().unwrap_or(0), s("host"), s("subject"), &s("fingerprint").chars().take(23).collect::<String>())
}
