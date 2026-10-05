//! js-endpoints: a small Plonix analyzer extension.
//!
//! For every in-scope JavaScript response the engine hands it, it reads the
//! string literals in the code and pulls out the ones that look like API
//! paths and URLs the app talks to (`/api/...`, `https://host/...`). Those
//! are the endpoints a page reaches at run time but that may never show up
//! in captured traffic until something triggers them, so surfacing them is a
//! map of where to look next. It notes what it found on each script, so the
//! endpoints show in the Lens next to that response.
//!
//! It reads only responses it is given and sends nothing. It uses the Plonix
//! host API, version 1 (`plonix:extension@1`): the engine writes a JSON batch
//! into a buffer from `plonix_alloc` and calls `plonix_analyze`; the
//! extension answers through `note`. It has no other way to reach anything.

use std::collections::BTreeSet;

use serde::Deserialize;

#[link(wasm_import_module = "plonix:extension@1")]
unsafe extern "C" {
    fn log(ptr: *const u8, len: usize);
    fn note(exchange_id: i64, tag_ptr: *const u8, tag_len: usize, text_ptr: *const u8, text_len: usize) -> i32;
}

#[derive(Deserialize)]
struct Batch {
    exchanges: Vec<Exchange>,
}

#[derive(Deserialize)]
struct Exchange {
    id: i64,
    path: String,
    mime: String,
    in_scope: bool,
    #[serde(default)]
    response_body: String,
}

/// Most endpoints reported on one script.
const MAX_PER_SCRIPT: usize = 60;
/// Longest literal considered; longer strings are almost never endpoints.
const MAX_LITERAL: usize = 256;

fn say(text: &str) {
    unsafe { log(text.as_ptr(), text.len()) }
}

fn add_note(id: i64, tag: &str, text: &str) {
    unsafe { note(id, tag.as_ptr(), tag.len(), text.as_ptr(), text.len()) };
}

/// Whether the response is JavaScript, by MIME or by the path's ending.
fn is_javascript(ex: &Exchange) -> bool {
    let m = ex.mime.to_ascii_lowercase();
    m.contains("javascript") || m.contains("ecmascript") || ex.path.to_ascii_lowercase().ends_with(".js") || ex.path.to_ascii_lowercase().ends_with(".mjs")
}

/// Pulls the endpoint-looking string literals out of JavaScript source. Only
/// what is between quotes is considered, so this never reads code as data; a
/// literal counts when it is an absolute URL or a rooted path with a plausible
/// shape, which keeps CSS selectors, dates and regex fragments out.
fn endpoints(src: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let q = bytes[i];
        if q == b'"' || q == b'\'' || q == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != q {
                // A backslash escapes the next byte, so an escaped quote does not end the literal.
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if j <= bytes.len() {
                let end = j.min(bytes.len());
                if let Ok(lit) = std::str::from_utf8(&bytes[start..end]) {
                    if let Some(e) = endpoint_of(lit) {
                        out.insert(e);
                    }
                }
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out.into_iter().take(MAX_PER_SCRIPT).collect()
}

/// Normalises one literal to an endpoint, or rejects it.
fn endpoint_of(lit: &str) -> Option<String> {
    if lit.len() < 2 || lit.len() > MAX_LITERAL || lit.contains(char::is_whitespace) {
        return None;
    }
    let url = lit.starts_with("http://") || lit.starts_with("https://");
    let rooted = lit.starts_with('/') && !lit.starts_with("//");
    if !url && !rooted {
        return None;
    }
    // The part that must look like a path: after the authority for a URL.
    let path = if url {
        let rest = lit.splitn(2, "://").nth(1)?;
        match rest.find('/') {
            Some(p) => &rest[p..],
            None => return None,
        }
    } else {
        lit
    };
    // Reject what is rooted but is not really an endpoint: a bare "/", data
    // and blob URLs, image and style assets, and strings that are clearly a
    // selector or template (`${...}`) rather than a path.
    if path.len() < 2 || path.contains("${") || path.contains("//") {
        return None;
    }
    let lower = lit.to_ascii_lowercase();
    if lower.starts_with("data:") || lower.starts_with("blob:") {
        return None;
    }
    let asset = [".css", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".woff", ".woff2", ".ttf", ".ico", ".map", ".webp", ".mp4"];
    let bare = path.split(['?', '#']).next().unwrap_or(path);
    if asset.iter().any(|a| bare.to_ascii_lowercase().ends_with(a)) {
        return None;
    }
    // Must contain only characters that belong in a URL path or query.
    if !lit.bytes().all(|b| b.is_ascii_graphic() && b != b'\\' && b != b'<' && b != b'>') {
        return None;
    }
    Some(lit.chars().take(MAX_LITERAL).collect())
}

/// A buffer of `len` bytes for the engine to write the batch into. It is
/// taken back by `plonix_analyze`.
#[unsafe(no_mangle)]
pub extern "C" fn plonix_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn plonix_analyze(ptr: *mut u8, len: usize) -> i32 {
    // SAFETY: the engine wrote `len` bytes into the buffer `plonix_alloc(len)` returned.
    let input = unsafe { Vec::from_raw_parts(ptr, len, len) };
    let Ok(batch) = serde_json::from_slice::<Batch>(&input) else {
        say("could not read the batch");
        return 1;
    };
    for ex in &batch.exchanges {
        if !ex.in_scope || !is_javascript(ex) || ex.response_body.is_empty() {
            continue;
        }
        let found = endpoints(&ex.response_body);
        if found.is_empty() {
            continue;
        }
        let shown = if found.len() == MAX_PER_SCRIPT { format!("{} or more endpoints", MAX_PER_SCRIPT) } else { format!("{} endpoint{}", found.len(), if found.len() == 1 { "" } else { "s" }) };
        let text: String = format!("{shown}: {}", found.join("  ")).chars().take(4000).collect();
        add_note(ex.id, "endpoints in script", &text);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_paths_and_urls_out_of_source() {
        let src = r#"
            const base = "/api/v2/users";
            fetch(`https://api.example.com/orders/${id}`);
            axios.get('/internal/admin/flags?all=1');
            const css = "/assets/app.css";
            const sel = ".btn-primary";
            const img = "https://cdn.example.com/logo.png";
            const proto = "//evil.example.com/x";
            const data = "data:text/html,<b>x</b>";
            const tmpl = "/user/${uid}/profile";
            const ok = "https://h.example.com/v1/search";
        "#;
        let got = endpoints(src);
        assert!(got.contains(&"/api/v2/users".to_string()));
        assert!(got.contains(&"/internal/admin/flags?all=1".to_string()));
        assert!(got.contains(&"https://h.example.com/v1/search".to_string()));
        // An interpolated URL keeps only the literal part before the template.
        assert!(!got.iter().any(|e| e.contains("${")), "{got:?}");
        assert!(!got.contains(&"/assets/app.css".to_string()), "assets are not endpoints");
        assert!(!got.contains(&".btn-primary".to_string()), "a selector is not an endpoint");
        assert!(!got.iter().any(|e| e.ends_with(".png")));
        assert!(!got.iter().any(|e| e.starts_with("//")));
        assert!(!got.iter().any(|e| e.starts_with("data:")));
    }

    #[test]
    fn javascript_is_recognised_by_mime_or_name() {
        let js = Exchange { id: 1, path: "/app.js".into(), mime: "text/plain".into(), in_scope: true, response_body: String::new() };
        assert!(is_javascript(&js));
        let m = Exchange { id: 2, path: "/x".into(), mime: "application/javascript".into(), in_scope: true, response_body: String::new() };
        assert!(is_javascript(&m));
        let html = Exchange { id: 3, path: "/x".into(), mime: "text/html".into(), in_scope: true, response_body: String::new() };
        assert!(!is_javascript(&html));
    }
}
