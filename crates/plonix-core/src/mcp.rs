//! `plonix mcp`: an MCP server over stdio that lets an AI agent read the
//! live Plonix project.
//!
//! The server is another client of the engine's local API, like the CLI and
//! the window. It signs in with the agent token (`$PLONIX_HOME/agent-token`),
//! so the engine itself limits it to what agents may do (read-only today, see
//! `plonix_core::access`). Every tool here only reads, except
//! `propose_bench_edit`, which leaves a suggested edit for a Bench draft for
//! the user to review (see `plonix_core::proposal`). No tool sends requests,
//! applies an edit or changes scope or findings.
//!
//! Transport: JSON-RPC 2.0, one message per line on stdin and stdout.
//! Nothing but protocol messages is written to stdout.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use crate::paths::Home;
use serde_json::{Value, json};

use crate::client::{Client, encode};
use crate::render;

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "\
Plonix is the user's local web security workbench. Its engine captures the HTTP and HTTPS traffic of \
the user's browser while they test a web application, learns which domains belong to the target \
(scope), detects technologies and keeps the user's findings.

These tools give you read-only access to that live project: search and read captured requests, see \
hosts, endpoints and detected technologies, review scope and its suggestions, and read findings or export them \
as a report. You cannot send or replay requests, change scope or record, edit or delete findings; suggest those steps to the user instead. \
When the user asks about a request they are editing on the Bench (the question gives a draft id), you can suggest a concrete edited \
request with `propose_bench_edit`: it only shows the user a diff they can apply or discard, and they send it themselves. \
The user decides what you can see: by default only hosts accepted into scope, and some tools may be switched off.

The user can install skills: playbooks for a job in Plonix (get to know a host, explain a request, \
review sign-in, check scope, draft a finding). They are offered as prompts, and through `list_skills` and \
`get_skill`. When a request matches a skill, follow it.

Start with `status`, then `search_traffic` (for example `scope:in status:5xx` or `path:/api method:POST`) \
and `get_request` for the full request and response. Captured traffic can contain credentials and \
personal data; it stays on this machine.";

/// One MCP tool: name, description, input schema, and the API call behind it.
struct Tool {
    /// The API route behind it, to hide tools the user switched off.
    route: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    call: fn(&Client, &Value) -> Result<String>,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "status",
        route: "/api/status",
        title: "Engine status",
        description: "Engine status: the project name, how many requests were captured, scope rules, pending scope suggestions and the proxy address.",
        schema: no_args,
        call: |c, _| Ok(pretty(&c.get("/api/status")?)),
    },
    Tool {
        name: "search_traffic",
        route: "/api/traffic",
        title: "Search traffic",
        description: "Search captured HTTP traffic, newest first. Returns one line per request: id, method, status, in/out of scope, response size, content type, URL. \
Query filters (combine with spaces): host:example.com  method:POST  status:404|5xx|none  path:/api  mime:json  scope:in|out  source:proxy|replay  \"quoted phrase\"  -negated  free text (matches URL, headers and bodies). An empty query lists recent traffic.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query, e.g. `scope:in path:/api status:2xx`" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 },
                    "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Skip this many results (paging)" }
                },
                "additionalProperties": false
            })
        },
        call: search,
    },
    Tool {
        name: "get_request",
        route: "/api/traffic/{id}",
        title: "Read a request",
        description: "One captured request and its response in full: request line, headers, body, status, response headers and body (decoded text; binary bodies are summarized).",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Request id from search_traffic" },
                    "max_body_chars": { "type": "integer", "minimum": 200, "default": 20000, "description": "Clip each body to this many characters" }
                },
                "required": ["id"],
                "additionalProperties": false
            })
        },
        call: |c, a| {
            let id = id_arg(a)?;
            let max = a["max_body_chars"].as_u64().unwrap_or(20_000).max(200) as usize;
            Ok(render::exchange(&c.get(&format!("/api/traffic/{id}"))?, max))
        },
    },
    Tool {
        name: "get_insights",
        route: "/api/traffic/{id}/insights",
        title: "What stands out in a request",
        description: "What stands out in one request: tokens that decode (JWT, base64, URL encoding), personal data and secrets, with where they appear and the decoded value.",
        schema: id_only,
        call: |c, a| Ok(pretty(&c.get(&format!("/api/traffic/{}/insights", id_arg(a)?))?)),
    },
    Tool {
        name: "get_messages",
        route: "/api/traffic/{id}/messages",
        title: "WebSocket messages",
        description: "The WebSocket messages sent over the connection a handshake request (status 101) opened, oldest first: direction (↑ client, ↓ server), kind, size and text.",
        schema: id_only,
        call: |c, a| Ok(render::messages(&c.get(&format!("/api/traffic/{}/messages?limit=1000", id_arg(a)?))?, 4000)),
    },
    Tool {
        name: "list_hosts",
        route: "/api/hosts",
        title: "Hosts",
        description: "Every host seen in captured traffic, busiest first, with its request count and scope decision (in, out, or undecided).",
        schema: no_args,
        call: |c, _| {
            let v = c.get("/api/hosts")?;
            let items = v.as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                return Ok("No hosts captured yet.".into());
            }
            Ok(format!("{:>7}  {:<5}  HOST\n{}", "REQS", "SCOPE", render::hosts(&items)))
        },
    },
    Tool {
        name: "list_endpoints",
        route: "/api/hosts/{host}/endpoints",
        title: "Endpoints on a host",
        description: "The site map of one host: every method and path seen, with hit counts, status codes and parameter names.",
        schema: || {
            json!({
                "type": "object",
                "properties": { "host": { "type": "string", "description": "Host name, e.g. api.example.com" } },
                "required": ["host"],
                "additionalProperties": false
            })
        },
        call: |c, a| {
            let host = a["host"].as_str().map(str::trim).filter(|h| !h.is_empty()).ok_or_else(|| anyhow!("`host` is required"))?;
            Ok(pretty(&c.get(&format!("/api/hosts/{}/endpoints", encode(host)))?))
        },
    },
    Tool {
        name: "detected_tech",
        route: "/api/tech",
        title: "Detected technologies",
        description: "Technologies detected on each host (frameworks, servers, CDNs, libraries) with version, confidence and the request that shows it. Optionally for one host.",
        schema: || {
            json!({
                "type": "object",
                "properties": { "host": { "type": "string", "description": "Only this host" } },
                "additionalProperties": false
            })
        },
        call: |c, a| match a["host"].as_str().map(str::trim).filter(|h| !h.is_empty()) {
            Some(h) => Ok(pretty(&c.get(&format!("/api/tech/{}", encode(h)))?)),
            None => {
                let v = c.get("/api/tech")?;
                let hosts: Vec<Value> =
                    v.as_array().into_iter().flatten().filter(|h| h["tech"].as_array().is_some_and(|t| !t.is_empty())).cloned().collect();
                if hosts.is_empty() {
                    return Ok("No technologies detected yet.".into());
                }
                Ok(pretty(&Value::Array(hosts)))
            }
        },
    },
    Tool {
        name: "get_scope",
        route: "/api/scope",
        title: "Scope",
        description: "The current scope: domains the user accepted or rejected, plus domains Plonix suggests adding, each with the evidence that links it to the target. Only the user can change scope.",
        schema: no_args,
        call: |c, _| Ok(pretty(&c.get("/api/scope")?)),
    },
    Tool {
        name: "list_findings",
        route: "/api/findings",
        title: "Findings",
        description: "Findings the user recorded: title, severity, status, description and the request ids that prove each one.",
        schema: no_args,
        call: |c, _| {
            let v = c.get("/api/findings")?;
            if v.as_array().is_some_and(Vec::is_empty) {
                return Ok("No findings recorded yet.".into());
            }
            Ok(pretty(&v))
        },
    },    Tool {
        name: "list_skills",
        route: "/api/skills",
        title: "Skills",
        description: "Skills the user installed: step-by-step playbooks for jobs in Plonix, such as getting to know a host or explaining a request. \
Each has a name, what it is for and the arguments it takes. Read one with get_skill and follow it.",
        schema: no_args,
        call: |c, _| {
            let v = c.get("/api/skills")?;
            let skills = v["skills"].as_array().cloned().unwrap_or_default();
            if skills.is_empty() {
                return Ok("No skills are available.".into());
            }
            Ok(skills
                .iter()
                .map(|s| {
                    let args: Vec<String> = s["arguments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|a| format!("{}{}", a["name"].as_str().unwrap_or(""), if a["required"] == true { "" } else { "?" }))
                        .collect();
                    format!(
                        "{}({}): {}. {}",
                        s["name"].as_str().unwrap_or(""),
                        args.join(", "),
                        s["title"].as_str().unwrap_or(""),
                        s["description"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        },
    },
    Tool {
        name: "get_skill",
        route: "/api/skills/{name}",
        title: "Read a skill",
        description: "The instructions of one skill from list_skills, with its arguments filled in. Follow them step by step with the other Plonix tools.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Skill name from list_skills" },
                    "arguments": { "type": "object", "description": "The skill's arguments, such as {\"host\": \"api.example.com\"}", "additionalProperties": { "type": "string" } }
                },
                "required": ["name"],
                "additionalProperties": false
            })
        },
        call: |c, a| {
            let name = a["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| anyhow!("`name` is required"))?;
            let v = c.get(&skill_path(name, &a["arguments"]))?;
            match v["prompt"].as_str() {
                Some(p) => Ok(p.to_string()),
                None => Ok(format!(
                    "{}\n\n(Not filled in: {}. Call get_skill again with arguments.)\n\n{}",
                    v["skill"]["title"].as_str().unwrap_or(name),
                    v["needs"].as_str().unwrap_or("arguments missing"),
                    v["skill"]["instructions"].as_str().unwrap_or("")
                )),
            }
        },
    },
    Tool {
        name: "findings_report",
        route: "/api/findings/export",
        title: "Findings report",
        description: "The user's findings as a Markdown report, most severe first, each with its evidence requests and responses (bodies clipped). \
By default false positives are left out; pass ids or statuses (open, confirmed, false_positive, fixed) to choose.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "integer" }, "description": "Only these finding ids" },
                    "status": { "type": "string", "description": "Comma-separated statuses, e.g. open,confirmed" }
                },
                "additionalProperties": false
            })
        },
        call: |c, a| {
            let ids: Vec<String> = a["ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).map(|i| i.to_string()).collect();
            let status = a["status"].as_str().unwrap_or("");
            c.get_text(&format!("/api/findings/export?format=md&ids={}&status={}", encode(&ids.join(",")), encode(status)))
        },
    },
];

/// The route behind `propose_bench_edit`: the one tool that is not a pure
/// read. It stores a suggestion; it cannot send, apply or change anything.
const PROPOSE_ROUTE: &str = "/api/bench/proposals";

const PROPOSE: Tool = Tool {
    name: "propose_bench_edit",
    route: PROPOSE_ROUTE,
    title: "Suggest an edit to a Bench request",
    description: "Suggest a concrete edited version of a request the user is editing on the Plonix Bench. Pass the draft_id from the user's question, \
a short summary of what you changed and why, and the complete edited request: method, url, headers and body (unchanged parts included). \
This sends nothing and changes nothing: Plonix shows the user a diff against their draft with Apply and Discard, and the user sends it \
themselves if they want to. You have no signing key, so a JWT you edit keeps its original signature and is shown as not re-signed.",
    schema: || {
        json!({
            "type": "object",
            "properties": {
                "draft_id": { "type": "string", "description": "The Bench draft id from the user's question" },
                "summary": { "type": "string", "description": "What you changed and why, in a sentence or two" },
                "method": { "type": "string", "description": "HTTP method, e.g. POST" },
                "url": { "type": "string", "description": "Full URL with query string" },
                "headers": {
                    "type": "array",
                    "description": "Every request header, in order",
                    "items": {
                        "type": "object",
                        "properties": { "name": { "type": "string" }, "value": { "type": "string" } },
                        "required": ["name", "value"],
                        "additionalProperties": false
                    }
                },
                "body": { "type": "string", "description": "The request body as text; empty for none" }
            },
            "required": ["draft_id", "method", "url"],
            "additionalProperties": false
        })
    },
    call: |c, a| {
        let p = c.post(PROPOSE_ROUTE, proposal_body(a)?)?;
        Ok(format!(
            "Suggested edit #{} is waiting on the Bench for the user to review as a diff. Nothing was sent: the user decides whether to apply it, and sends it themselves.",
            p["id"]
        ))
    },
};

/// The engine's proposal body from the tool's arguments. Headers may come as
/// `{name, value}` objects or `[name, value]` pairs.
fn proposal_body(a: &Value) -> Result<Value> {
    let s = |k: &str| a[k].as_str().map(str::trim).unwrap_or("").to_string();
    let draft = s("draft_id");
    if draft.is_empty() {
        return Err(anyhow!("`draft_id` is required: use the draft id from the user's question"));
    }
    let mut headers = vec![];
    for h in a["headers"].as_array().into_iter().flatten() {
        let pair = match h {
            Value::Object(o) => (o.get("name").and_then(Value::as_str), o.get("value").and_then(Value::as_str)),
            Value::Array(v) if v.len() == 2 => (v[0].as_str(), v[1].as_str()),
            _ => (None, None),
        };
        match pair {
            (Some(k), Some(v)) => headers.push(json!([k, v])),
            _ => return Err(anyhow!("each header needs a name and a value")),
        }
    }
    Ok(json!({ "draft": draft, "summary": s("summary"), "method": s("method"), "url": s("url"), "headers": headers, "body": a["body"].as_str().unwrap_or("") }))
}

fn all_tools() -> impl Iterator<Item = &'static Tool> {
    TOOLS.iter().chain(std::iter::once(&PROPOSE))
}

/// The API path that reads a skill with its arguments filled in.
fn skill_path(name: &str, args: &Value) -> String {
    let mut path = format!("/api/skills/{}", encode(name));
    let pairs: Vec<String> = args
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            Some(format!("{}={}", encode(k), encode(&v)))
        })
        .collect();
    if !pairs.is_empty() {
        path.push('?');
        path.push_str(&pairs.join("&"));
    }
    path
}

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn id_only() -> Value {
    json!({
        "type": "object",
        "properties": { "id": { "type": "integer", "description": "Request id from search_traffic" } },
        "required": ["id"],
        "additionalProperties": false
    })
}

fn id_arg(a: &Value) -> Result<i64> {
    a["id"]
        .as_i64()
        .or_else(|| a["id"].as_str().and_then(|s| s.trim_start_matches('#').parse().ok()))
        .ok_or_else(|| anyhow!("`id` must be a request id number"))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn search(c: &Client, a: &Value) -> Result<String> {
    let q = a["query"].as_str().unwrap_or("").trim();
    let limit = a["limit"].as_u64().unwrap_or(30).clamp(1, 200);
    let offset = a["offset"].as_u64().unwrap_or(0);
    let v = c.get(&format!("/api/traffic?q={}&limit={limit}&offset={offset}", encode(q)))?;
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let total = v["total"].as_u64().unwrap_or(0);
    if items.is_empty() {
        return Ok(if q.is_empty() { "Nothing captured yet.".into() } else { format!("No traffic matches `{q}`.") });
    }
    let shown = offset + items.len() as u64;
    let more = if shown < total { format!("; next page: offset {shown}") } else { String::new() };
    Ok(format!(
        "{:>6}  {:<7} {:>3}  {}  {:>8}  {:<16} URL\n{}\n{} of {total} match(es){more}. Read one with get_request.",
        "ID",
        "METHOD",
        "ST",
        "SCP",
        "SIZE",
        "TYPE",
        render::traffic_table(&items).trim_end(),
        items.len()
    ))
}

/// The tool names, for `plonix connect` output and tests.
pub fn tool_names() -> Vec<&'static str> {
    all_tools().map(|t| t.name).collect()
}

/// Set by Plonix for the MCP server of an Ask Claude run inside the app:
/// the name the server reports itself under, whatever the client says.
pub const CLIENT_ENV: &str = "PLONIX_MCP_CLIENT";
/// The client name of Ask Claude runs inside the app, before `/<run id>`.
pub const ASK_CLIENT: &str = "plonix-ask";

/// The name this server starts under: [`CLIENT_ENV`] when set.
fn fixed_name() -> Option<String> {
    std::env::var(CLIENT_ENV).ok().map(|n| n.trim().chars().take(64).collect::<String>()).filter(|n| !n.is_empty())
}

struct Server {
    home: Home,
    /// The connected client's name, sent to the engine so the Agents screen
    /// can show who is connected.
    client_name: Arc<Mutex<String>>,
}

impl Server {
    fn client(&self) -> Result<Client> {
        let name = self.client_name.lock().unwrap().clone();
        Client::connect_agent(&self.home, &name)
    }

    /// Tools the user has not switched off in Settings › AI agents. With no
    /// engine to ask, every tool is listed; the engine still refuses calls.
    fn available(&self) -> impl Iterator<Item = &'static Tool> {
        let routes: Option<Vec<String>> = self.client().and_then(|c| c.get("/api/agents")).ok().map(|v| {
            v["capabilities"].as_array().into_iter().flatten().filter_map(|c| c["path"].as_str().map(String::from)).collect()
        });
        all_tools().filter(move |t| routes.as_ref().is_none_or(|r| r.iter().any(|p| p == t.route)))
    }

    fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or("");
        // Notifications (no id) get no response.
        let id = match id {
            Some(id) if !id.is_null() => id,
            _ => {
                if method == "notifications/initialized" {
                    // Say hello so the Agents screen shows the connection right away.
                    let _ = self.client().and_then(|c| c.get("/api/agents"));
                }
                return None;
            }
        };
        let result = match method {
            "initialize" => Ok(self.initialize(&msg["params"])),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.available().map(tool_json).collect::<Vec<_>>() })),
            "tools/call" => self.call(&msg["params"]),
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": self.prompts() })),
            "prompts/get" => self.prompt(&msg["params"]),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        if let Some(name) = params["clientInfo"]["name"].as_str().filter(|n| !n.trim().is_empty() && fixed_name().is_none()) {
            *self.client_name.lock().unwrap() = name.trim().chars().take(32).collect();
        }
        let asked = params["protocolVersion"].as_str().unwrap_or("");
        let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false }, "prompts": { "listChanged": false } },
            "serverInfo": { "name": "plonix", "title": "Plonix (read-only)", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })
    }

    /// Skills the agent may use, as MCP prompts. With no engine, none.
    fn prompts(&self) -> Vec<Value> {
        let Ok(v) = self.client().and_then(|c| c.get("/api/skills")) else { return vec![] };
        v["skills"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let args: Vec<Value> = s["arguments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| json!({ "name": a["name"], "description": a["description"], "required": a["required"] }))
                    .collect();
                json!({ "name": s["name"], "title": s["title"], "description": s["description"], "arguments": args })
            })
            .collect()
    }

    fn prompt(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or("").trim();
        if name.is_empty() {
            return Err((-32602, "`name` is required".into()));
        }
        let v = self.client().and_then(|c| c.get(&skill_path(name, &params["arguments"]))).map_err(|e| (-32602, format!("{e:#}")))?;
        let Some(text) = v["prompt"].as_str() else {
            return Err((-32602, v["needs"].as_str().unwrap_or("missing arguments").to_string()));
        };
        Ok(json!({
            "description": v["skill"]["description"],
            "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
        }))
    }

    fn call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or("");
        let Some(tool) = all_tools().find(|t| t.name == name) else {
            return Err((-32602, format!("unknown tool: {name}. Plonix tools only read (and suggest Bench edits): {}", tool_names().join(", "))));
        };
        let args = if params["arguments"].is_object() { params["arguments"].clone() } else { json!({}) };
        let out = self.client().and_then(|c| (tool.call)(&c, &args));
        Ok(match out {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            Err(e) => {
                json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
            }
        })
    }
}

fn tool_json(t: &Tool) -> Value {
    // Suggesting a Bench edit stores something, so it is not marked
    // read-only, but it never reaches the network or changes the project.
    let reads = t.route != PROPOSE_ROUTE;
    json!({
        "name": t.name,
        "title": t.title,
        "description": t.description,
        "inputSchema": (t.schema)(),
        "annotations": { "title": t.title, "readOnlyHint": reads, "destructiveHint": false, "idempotentHint": reads, "openWorldHint": false },
    })
}

/// Answers one MCP message, as [`serve`] does for each line it reads.
pub fn handle_message(home: &Home, msg: &Value) -> Option<Value> {
    Server { home: home.clone(), client_name: Arc::new(Mutex::new("mcp".into())) }.handle(msg)
}

/// Serves MCP on stdin/stdout until stdin closes.
pub fn serve(home: Home) -> Result<()> {
    crate::usage::init(&home, false);
    crate::usage::record("mcp_session");
    crate::usage::flush();
    let server = Server { home, client_name: Arc::new(Mutex::new(fixed_name().unwrap_or_else(|| "mcp".into()))) };

    // Keep the Agents screen's "connected" status fresh while the agent is idle.
    let (home, name) = (server.home.clone(), server.client_name.clone());
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(20));
            let n = name.lock().unwrap().clone();
            let _ = Client::connect_agent(&home, &n).and_then(|c| c.get("/api/agents"));
        }
    });

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{r}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        let home = Home { root: std::env::temp_dir().join("plonix-mcp-test-no-engine") };
        Server { home, client_name: Arc::new(Mutex::new("mcp".into())) }
    }

    #[test]
    fn initialize_negotiates_version_and_names_client() {
        let s = server();
        let r = s
            .handle(
                &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"test-agent"}}}),
            )
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "plonix");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(*s.client_name.lock().unwrap(), "test-agent");
        let r = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn every_tool_reads_except_the_bench_suggestion() {
        let r = server().handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), TOOLS.len() + 1);
        for t in tools {
            let suggests = t["name"] == "propose_bench_edit";
            assert_eq!(t["annotations"]["readOnlyHint"], !suggests, "{}", t["name"]);
            assert_eq!(t["annotations"]["destructiveHint"], false, "{}", t["name"]);
            assert_eq!(t["annotations"]["openWorldHint"], false, "{}", t["name"]);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        let names = tool_names();
        for forbidden in ["send", "replay", "accept", "reject", "add_finding", "install", "apply", "run"] {
            assert!(!names.iter().any(|n| n.contains(forbidden)), "{forbidden}");
        }
        // Every tool's route is one agents may call, and none of them sends.
        for t in all_tools() {
            let method = if t.route == PROPOSE_ROUTE { "POST" } else { "GET" };
            assert!(crate::access::allowed(crate::access::AgentMode::ReadOnly, method, t.route), "{}", t.name);
            assert!(!["/api/send", "/api/replay", "/api/run", "/api/scan", "/api/crawl"].contains(&t.route), "{}", t.name);
        }
    }

    #[test]
    fn proposal_arguments_become_an_engine_request() {
        let body = proposal_body(&json!({
            "draft_id": " d1 ", "summary": "admin role", "method": "post", "url": "https://a.test/x",
            "headers": [{ "name": "Accept", "value": "*/*" }, ["X-A", "1"]], "body": "{}"
        }))
        .unwrap();
        assert_eq!(body["draft"], "d1");
        assert_eq!(body["headers"], json!([["Accept", "*/*"], ["X-A", "1"]]));
        assert_eq!(body["body"], "{}");
        assert!(proposal_body(&json!({ "method": "GET", "url": "https://a.test/" })).is_err());
        assert!(proposal_body(&json!({ "draft_id": "d1", "headers": [{ "name": "X" }] })).is_err());
    }

    #[test]
    fn proposing_without_an_engine_reports_an_error() {
        let r = server()
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"propose_bench_edit","arguments":{"draft_id":"d1","method":"GET","url":"https://a.test/"}}}))
            .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("not running"));
    }

    #[test]
    fn unknown_tools_and_methods_are_errors() {
        let s = server();
        let r = s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"replay","arguments":{}}})).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        let r = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"nope"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert!(s.handle(&json!({"jsonrpc":"2.0","method":"notifications/cancelled"})).is_none());
    }

    #[test]
    fn tool_errors_are_reported_in_the_result() {
        let r = server().handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"status"}})).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("not running"));
    }
}
