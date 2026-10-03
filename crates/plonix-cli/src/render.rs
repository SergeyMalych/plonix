//! Plain-text rendering shared by the CLI and MCP tools.

use base64::Engine as _;
use serde_json::Value;

pub fn traffic_table(items: &[Value]) -> String {
    let mut out = String::new();
    for it in items {
        out.push_str(&traffic_line(it));
        out.push('\n');
    }
    out
}

pub fn traffic_line(it: &Value) -> String {
    let status = it["status"].as_u64().map(|s| s.to_string()).unwrap_or_else(|| "ERR".into());
    let mut url = format!("{}://{}", it["scheme"].as_str().unwrap_or(""), it["host"].as_str().unwrap_or(""));
    let port = it["port"].as_u64().unwrap_or(0);
    if !(port == 443 && url.starts_with("https") || port == 80 && url.starts_with("http:")) {
        url.push_str(&format!(":{port}"));
    }
    url.push_str(it["path"].as_str().unwrap_or(""));
    if let Some(q) = it["query"].as_str().filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    let url = clip(&url, 110);
    let scope = if it["in_scope"].as_bool() == Some(true) { "in " } else { "out" };
    let src = if it["source"] == "replay" { " ↻" } else { "" };
    format!(
        "{:>6}  {:<7} {:>3}  {}  {:>8}  {:<16} {}{}",
        it["id"].as_i64().unwrap_or(0),
        it["method"].as_str().unwrap_or(""),
        status,
        scope,
        human_size(it["resp_len"].as_i64().unwrap_or(0)),
        clip(it["mime"].as_str().unwrap_or(""), 16),
        url,
        src
    )
}

/// Raw HTTP-style rendering of one exchange (as returned by `/api/traffic/{id}`).
pub fn exchange(v: &Value, max_body: usize) -> String {
    let mut out = String::new();
    let status = v["status"].as_u64().map(|s| s.to_string()).unwrap_or_else(|| "no response".into());
    out.push_str(&format!(
        "#{} {} {} → {}  ({} ms, {}, {}{})\n\n",
        v["id"],
        v["method"].as_str().unwrap_or(""),
        v["url"].as_str().unwrap_or(""),
        status,
        v["duration_ms"],
        if v["in_scope"].as_bool() == Some(true) { "in scope" } else { "out of scope" },
        v["source"].as_str().unwrap_or("proxy"),
        v["initiator"].as_str().map(|i| format!(" by {i}")).unwrap_or_default()
    ));
    let target = match v["query"].as_str().filter(|q| !q.is_empty()) {
        Some(q) => format!("{}?{}", v["path"].as_str().unwrap_or(""), q),
        None => v["path"].as_str().unwrap_or("").to_string(),
    };
    out.push_str(&format!("{} {} HTTP/1.1\n", v["method"].as_str().unwrap_or(""), target));
    out.push_str(&headers(&v["req_headers"]));
    out.push_str(&body(&v["req_text"], &v["req_body"], max_body));
    out.push_str("\n――――――――――――――――――――――――――――――――――――――――\n");
    match v["status"].as_u64() {
        Some(s) => {
            out.push_str(&format!("HTTP/1.1 {s}\n"));
            out.push_str(&headers(&v["resp_headers"]));
            out.push_str(&body(&v["resp_text"], &v["resp_body"], max_body));
        }
        None => out.push_str(&format!("error: {}\n", v["error"].as_str().unwrap_or("unknown"))),
    }
    out
}

fn headers(h: &Value) -> String {
    h.as_array()
        .map(|a| a.iter().map(|p| format!("{}: {}\n", p[0].as_str().unwrap_or(""), p[1].as_str().unwrap_or(""))).collect())
        .unwrap_or_default()
}

fn body(text: &Value, raw_b64: &Value, max: usize) -> String {
    match text.as_str() {
        Some(t) if t.len() > max => format!("\n{}\n… [{} more bytes; use --full or a larger max]\n", clip_bytes(t, max), t.len() - max),
        Some(t) => format!("\n{t}\n"),
        None => {
            let len = raw_b64
                .as_str()
                .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
                .map(|b| b.len())
                .unwrap_or(0);
            if len == 0 { String::new() } else { format!("\n<{len} bytes of binary data>\n") }
        }
    }
}

pub fn scope(v: &Value) -> String {
    let mut out = String::new();
    let rules = v["rules"].as_array().cloned().unwrap_or_default();
    if rules.is_empty() {
        out.push_str("No scope yet. Seed it with `plonix scope add example.com`.\n");
    } else {
        out.push_str("Scope rules:\n");
        for r in &rules {
            let mark = if r["decision"] == "accepted" { "✓ in " } else { "✗ out" };
            let pat = if r["include_subdomains"].as_bool() == Some(true) {
                format!("{} (+ subdomains)", r["pattern"].as_str().unwrap_or(""))
            } else {
                r["pattern"].as_str().unwrap_or("").to_string()
            };
            out.push_str(&format!("  {mark}  {pat}\n"));
        }
    }
    let sug = v["suggestions"].as_array().cloned().unwrap_or_default();
    if sug.is_empty() {
        out.push_str("\nNo pending suggestions.\n");
    } else {
        out.push_str(&format!("\n{} suggested domain(s), strongest evidence first:\n", sug.len()));
        for s in &sug {
            out.push_str(&suggestion(s));
        }
        out.push_str("\nReview them with `plonix scope review`, or `plonix scope accept|reject <domain>`.\n");
    }
    out
}

pub fn suggestion(s: &Value) -> String {
    let mut out = format!(
        "\n  ? {}  (score {}, {} request(s))\n",
        s["domain"].as_str().unwrap_or(""),
        s["score"],
        s["requests"]
    );
    for e in s["evidence"].as_array().into_iter().flatten() {
        let times = e["count"].as_i64().filter(|c| *c > 1).map(|c| format!(" ×{c}")).unwrap_or_default();
        out.push_str(&format!(
            "      - {}{}  [#{}: {}]\n",
            e["summary"].as_str().unwrap_or(""),
            times,
            e["exchange_id"],
            clip(e["detail"].as_str().unwrap_or(""), 90)
        ));
    }
    out
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn clip_bytes(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn human_size(n: i64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / 1024.0 / 1024.0),
    }
}

/// Engine status (as returned by `/api/status`).
pub fn status(v: &Value, ca_path: &str) -> String {
    let fp = v["ca_fingerprint"].as_str().unwrap_or("");
    let pending = v["pending_suggestions"].as_i64().unwrap_or(0);
    let pending = if pending > 0 { format!(", {pending} suggestion(s) to review (`plonix scope`)") } else { String::new() };
    format!(
        "Plonix engine running (pid {})\n  Project    {}\n  Proxy      {}\n  API        {}\n  Captured   {} exchange(s)\n  Scope      {} rule(s){}\n  CA         {}  (SHA-256 {}…)\n",
        v["pid"],
        v["project"].as_str().unwrap_or(""),
        v["proxy"].as_str().unwrap_or(""),
        v["api"].as_str().unwrap_or(""),
        v["exchanges"],
        v["scope_rules"],
        pending,
        ca_path,
        clip(fp, 24).trim_end_matches('…'),
    )
}

pub fn hosts(items: &[Value]) -> String {
    let mut out = String::new();
    for h in items {
        let scope = match h["scope"].as_str() {
            Some("accepted") => "in",
            Some("rejected") => "out",
            _ => "?",
        };
        out.push_str(&format!("{:>7}  {:<5}  {}\n", h["requests"].as_i64().unwrap_or(0), scope, h["host"].as_str().unwrap_or("")));
    }
    out
}
