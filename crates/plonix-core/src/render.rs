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
    if let Some(c) = v["client_cert"].as_str() {
        out.push_str(&format!("Client certificate: {c}\n\n"));
    }
    let target = match v["query"].as_str().filter(|q| !q.is_empty()) {
        Some(q) => format!("{}?{}", v["path"].as_str().unwrap_or(""), q),
        None => v["path"].as_str().unwrap_or("").to_string(),
    };
    let version = v["http_version"].as_str().filter(|s| !s.is_empty()).unwrap_or("HTTP/1.1");
    out.push_str(&format!("{} {} {version}\n", v["method"].as_str().unwrap_or(""), target));
    out.push_str(&headers(&v["req_headers"]));
    out.push_str(&body(&v["req_text"], &v["req_body"], max_body));
    out.push_str(&cut_note(v, "req"));
    out.push_str("\n――――――――――――――――――――――――――――――――――――――――\n");
    match v["status"].as_u64() {
        Some(s) => {
            out.push_str(&format!("{version} {s}\n"));
            out.push_str(&headers(&v["resp_headers"]));
            out.push_str(&body(&v["resp_text"], &v["resp_body"], max_body));
            out.push_str(&cut_note(v, "resp"));
        }
        None => out.push_str(&format!("error: {}\n", v["error"].as_str().unwrap_or("unknown"))),
    }
    out
}

/// WebSocket messages (as returned by `/api/traffic/{id}/messages`), one per
/// line: ↑ sent by the client, ↓ by the server.
pub fn messages(v: &Value, max: usize) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let total = v["total"].as_i64().unwrap_or(items.len() as i64);
    let mut out = format!("\n――――――――――――――――――――――――――――――――――――――――\n{total} WebSocket message(s)\n");
    for m in &items {
        let arrow = if m["direction"] == "to_server" { "↑" } else { "↓" };
        let size = m["size"].as_i64().unwrap_or(0);
        let cut = if m["truncated"].as_bool() == Some(true) { format!(", first {} kept", human_size(b64_len(&m["payload"]) as i64)) } else { String::new() };
        let shown = match m["text"].as_str() {
            Some(t) if t.len() > max => format!("{}… [{} more bytes]", clip_bytes(t, max), t.len() - max),
            Some(t) => t.to_string(),
            None if size > 0 => format!("<{} of binary data>", human_size(size)),
            None => String::new(),
        };
        out.push_str(&format!("{arrow} {:<6} {:>8}{cut}  {shown}\n", m["opcode"].as_str().unwrap_or(""), human_size(size)));
    }
    if (items.len() as i64) < total {
        out.push_str(&format!("… and {} more\n", total - items.len() as i64));
    }
    out
}

/// A note for a body that was longer than the recording limit (`side` is `req` or `resp`).
fn cut_note(v: &Value, side: &str) -> String {
    if v[format!("{side}_truncated")].as_bool() != Some(true) {
        return String::new();
    }
    let kept = b64_len(&v[format!("{side}_body")]);
    match v[format!("{side}_size")].as_i64() {
        Some(size) => format!("[body cut: the first {} of {} were kept]\n", human_size(kept as i64), human_size(size)),
        None => format!("[body cut: the first {} were kept]\n", human_size(kept as i64)),
    }
}

fn b64_len(raw_b64: &Value) -> usize {
    raw_b64.as_str().and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()).map(|b| b.len()).unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cut_bodies_say_how_much_was_kept() {
        let v = json!({
            "id": 3, "method": "GET", "url": "https://a.test/big", "path": "/big", "status": 200,
            "req_headers": [], "req_body": "", "resp_headers": [], "resp_text": "start",
            "resp_body": "c3RhcnQ=", "resp_truncated": true, "resp_size": 3145728,
        });
        let out = exchange(&v, 4000);
        assert!(out.contains("[body cut: the first 5 B of 3.0 MB were kept]"), "{out}");
        assert_eq!(out.matches("body cut").count(), 1, "the request was not cut");
        assert!(out.contains("GET /big HTTP/1.1\n") && out.contains("HTTP/1.1 200\n"), "older captures read as HTTP/1.1: {out}");
        let out = exchange(&json!({ "method": "GET", "path": "/", "status": 204, "http_version": "HTTP/2" }), 4000);
        assert!(out.contains("GET / HTTP/2\n") && out.contains("HTTP/2 204\n"), "{out}");
    }

    #[test]
    fn websocket_messages_one_per_line() {
        let v = json!({ "total": 3, "items": [
            { "direction": "to_server", "opcode": "text", "size": 5, "payload": "aGVsbG8=", "text": "hello", "truncated": false },
            { "direction": "to_client", "opcode": "binary", "size": 2048, "payload": "AAE=", "truncated": true },
        ]});
        let out = messages(&v, 400);
        assert!(out.contains("3 WebSocket message(s)"), "{out}");
        assert!(out.contains("↑ text        5 B  hello"), "{out}");
        assert!(out.contains("↓ binary   2.0 KB, first 2 B kept  <2.0 KB of binary data>"), "{out}");
        assert!(out.ends_with("… and 1 more\n"), "{out}");
    }
}
