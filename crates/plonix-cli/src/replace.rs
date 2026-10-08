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

#[derive(Clone, Copy, ValueEnum)]
pub enum HeaderKind {
    /// Add the header where it is missing
    Add,
    /// Give the header this value, adding it where it is missing
    Set,
    /// Give the header this value, only where it is already sent
    Change,
    /// Take the header out
    Remove,
}

impl HeaderKind {
    fn api(self) -> &'static str {
        match self {
            HeaderKind::Add => "add_header",
            HeaderKind::Set => "set_header",
            HeaderKind::Change => "change_header",
            HeaderKind::Remove => "remove_header",
        }
    }
}

/// Where a rule applies and when.
#[derive(clap::Args)]
pub struct Reach {
    /// Also change requests sent from the Bench, Run and Access check
    #[arg(long)]
    bench: bool,
    /// Also change requests sent by Scans, crawls and extensions
    #[arg(long)]
    scans: bool,
    /// Leave traffic from the browser alone (use with --bench or --scans)
    #[arg(long)]
    no_browser: bool,
    /// Only when the exchange matches this traffic search, e.g. "host:api.example.com method:POST"
    #[arg(long, value_name = "SEARCH")]
    when: Option<String>,
}

impl Reach {
    fn fields(&self) -> Value {
        json!({ "browser": !self.no_browser, "bench": self.bench, "scans": self.scans, "when": self.when.clone().unwrap_or_default() })
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
        #[command(flatten)]
        reach: Reach,
    },
    /// Add, set, change or remove one header on every request (or response, with --response)
    Header {
        #[arg(value_enum)]
        kind: HeaderKind,
        /// The header's name
        name: String,
        /// Its value (not needed to remove it)
        #[arg(default_value = "", allow_hyphen_values = true)]
        value: String,
        /// Change response headers instead of request headers
        #[arg(long)]
        response: bool,
        /// Only change traffic to in-scope hosts
        #[arg(long)]
        in_scope: bool,
        #[command(flatten)]
        reach: Reach,
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
        ReplaceCmd::Add { target, pattern, replacement, regex, in_scope, note, reach } => {
            let mut body = json!({ "target": target.api(), "match": pattern, "replace": replacement, "regex": regex, "in_scope_only": in_scope, "note": note.unwrap_or_default() });
            merge(&mut body, reach.fields());
            let v = c.post("/api/replace", body)?;
            if ctx.json {
                return ctx.print_json(&v);
            }
            println!("Added rule {}. It applies to traffic from now on.", v["id"]);
        }
        ReplaceCmd::Header { kind, name, value, response, in_scope, reach } => {
            let target = if response { "response_header" } else { "request_header" };
            let mut body = json!({ "kind": kind.api(), "target": target, "match": name, "replace": value, "in_scope_only": in_scope });
            merge(&mut body, reach.fields());
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

fn merge(into: &mut Value, from: Value) {
    if let (Some(a), Value::Object(b)) = (into.as_object_mut(), from) {
        a.extend(b);
    }
}

fn describe(r: &Value) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
    let mut flags = vec![];
    if r["enabled"] == false {
        flags.push("off".to_string());
    }
    if r["regex"] == true {
        flags.push("regex".to_string());
    }
    if r["in_scope_only"] == true {
        flags.push("in scope only".to_string());
    }
    let reach: Vec<&str> = ["browser", "bench", "scans"].into_iter().filter(|k| r[*k] == true).collect();
    if reach != ["browser"] {
        flags.push(reach.join(" + "));
    }
    let flags = if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join(", ")) };
    let when = if s("when").is_empty() { String::new() } else { format!("  when {}", s("when")) };
    let note = if s("note").is_empty() { String::new() } else { format!("  # {}", s("note")) };
    let id = r["id"].as_i64().unwrap_or(0);
    let side = if s("target") == "response_header" { "response" } else { "request" };
    let what = match s("kind").as_str() {
        "add_header" => format!("add {side} header {}: {}", s("pattern"), s("replace")),
        "set_header" => format!("set {side} header {}: {}", s("pattern"), s("replace")),
        "change_header" => format!("change {side} header {} to {}", s("pattern"), s("replace")),
        "remove_header" => format!("remove {side} header {}", s("pattern")),
        _ => format!("{:<16} {:?} → {:?}", s("target").replace('_', "-"), s("pattern"), s("replace")),
    };
    format!("{id:>4}  {what}{flags}{when}{note}")
}
