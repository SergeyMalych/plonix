//! `plonix replace`: match-and-replace rules the proxy applies to traffic in flight.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::Ctx;

#[derive(Clone, Copy, ValueEnum)]
pub enum TargetArg {
    /// `METHOD /path?query`
    RequestLine,
    /// Request headers, as `Name: value` lines
    RequestHeader,
    RequestBody,
    /// Response headers, as `Name: value` lines
    ResponseHeader,
    /// Response bodies (compressed ones are matched decoded)
    ResponseBody,
}

impl TargetArg {
    fn api(self) -> &'static str {
        match self {
            TargetArg::RequestLine => "request_line",
            TargetArg::RequestHeader => "request_header",
            TargetArg::RequestBody => "request_body",
            TargetArg::ResponseHeader => "response_header",
            TargetArg::ResponseBody => "response_body",
        }
    }
}

#[derive(Subcommand)]
pub enum ReplaceCmd {
    /// Every rule, in the order they apply (the default)
    #[command(visible_alias = "ls")]
    List,
    /// Add a rule. Header rules see `Name: value` lines: replace a whole line with "" to remove a header
    Add {
        /// What the rule changes
        #[arg(value_enum)]
        target: TargetArg,
        /// Text to find (a regular expression with --regex)
        #[arg(allow_hyphen_values = true)]
        pattern: String,
        /// What replaces it; with --regex, $1 inserts a capture. Leave out to remove the match
        #[arg(default_value = "", allow_hyphen_values = true)]
        replacement: String,
        /// Treat the pattern as a regular expression
        #[arg(long)]
        regex: bool,
        /// Only change traffic to in-scope hosts
        #[arg(long)]
        in_scope: bool,
        /// A short note shown with the rule and in the record of what it changed
        #[arg(long)]
        note: Option<String>,
    },
    /// Switch a rule on
    Enable { id: i64 },
    /// Switch a rule off without removing it
    Disable { id: i64 },
    /// Remove a rule
    Rm { id: i64 },
}

pub fn replace_cmd(ctx: &Ctx, cmd: ReplaceCmd) -> Result<()> {
    let c = ctx.client()?;
    match cmd {
        ReplaceCmd::List => {
            let v = c.get("/api/replace")?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            let rules = v["rules"].as_array().cloned().unwrap_or_default();
            if rules.is_empty() {
                println!("No match-and-replace rules. Add one with `plonix replace add request-header '(?i)^user-agent: .*$' 'User-Agent: test' --regex`.");
                return Ok(());
            }
            if v["enabled"] == false {
                println!("Match and replace is switched off in Settings › Match and replace; these rules change nothing.\n");
            }
            for r in &rules {
                println!("{}", describe(r));
            }
        }
        ReplaceCmd::Add { target, pattern, replacement, regex, in_scope, note } => {
            let body = json!({ "target": target.api(), "match": pattern, "replace": replacement, "regex": regex, "in_scope_only": in_scope, "note": note.unwrap_or_default() });
            let v = c.post("/api/replace", body)?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Added rule {}. It applies to traffic from now on.", v["id"]);
        }
        ReplaceCmd::Enable { id } | ReplaceCmd::Disable { id } => {
            let on = matches!(cmd, ReplaceCmd::Enable { .. });
            let v = c.patch(&format!("/api/replace/{id}"), json!({ "enabled": on }))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Rule {id} is {}.", if on { "on" } else { "off" });
        }
        ReplaceCmd::Rm { id } => {
            let v = c.delete(&format!("/api/replace/{id}"))?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Removed rule {id}.");
        }
    }
    Ok(())
}

fn describe(r: &Value) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
    let mut flags = vec![];
    if r["enabled"] == false {
        flags.push("off");
    }
    if r["regex"] == true {
        flags.push("regex");
    }
    if r["in_scope_only"] == true {
        flags.push("in scope only");
    }
    let flags = if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join(", ")) };
    let note = if s("note").is_empty() { String::new() } else { format!("  # {}", s("note")) };
    format!("{:>4}  {:<16} {:?} → {:?}{flags}{note}", r["id"].as_i64().unwrap_or(0), s("target").replace('_', "-"), s("pattern"), s("replace"))
}
