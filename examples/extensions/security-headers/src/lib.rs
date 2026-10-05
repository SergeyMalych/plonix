//! security-headers: a small Plonix analyzer extension.
//!
//! For every in-scope HTML page the engine hands it, it notes the security
//! headers that are missing and the cookies set without `Secure` or
//! `HttpOnly`, then proposes one finding per host. The finding is stored as
//! unconfirmed and attributed to this extension until you confirm it.
//!
//! It uses the Plonix host API, version 1 (`plonix:extension@1`): the engine
//! writes a JSON batch into a buffer from `plonix_alloc` and calls
//! `plonix_analyze`; the extension answers through `note` and
//! `propose_finding`. It has no other way to reach anything.

use std::collections::BTreeMap;

use serde::Deserialize;

#[link(wasm_import_module = "plonix:extension@1")]
unsafe extern "C" {
    fn log(ptr: *const u8, len: usize);
    fn note(exchange_id: i64, tag_ptr: *const u8, tag_len: usize, text_ptr: *const u8, text_len: usize) -> i32;
    fn propose_finding(severity: i32, title_ptr: *const u8, title_len: usize, desc_ptr: *const u8, desc_len: usize, ids_ptr: *const i64, ids_count: usize) -> i32;
}

#[derive(Deserialize)]
struct Batch {
    exchanges: Vec<Exchange>,
}

#[derive(Deserialize)]
struct Exchange {
    id: i64,
    scheme: String,
    host: String,
    in_scope: bool,
    status: Option<u16>,
    mime: String,
    response_headers: Vec<(String, String)>,
}

const LOW: i32 = 1;

fn say(text: &str) {
    unsafe { log(text.as_ptr(), text.len()) }
}

fn add_note(id: i64, tag: &str, text: &str) {
    unsafe { note(id, tag.as_ptr(), tag.len(), text.as_ptr(), text.len()) };
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
    // host -> (missing headers, exchanges that show it)
    let mut hosts: BTreeMap<String, (Vec<&str>, Vec<i64>)> = BTreeMap::new();
    for ex in &batch.exchanges {
        if !ex.in_scope || ex.mime != "text/html" || !ex.status.is_some_and(|s| (200..300).contains(&s)) {
            continue;
        }
        let has = |name: &str| ex.response_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        let mut missing = vec![];
        if !has("content-security-policy") {
            missing.push("Content-Security-Policy");
        }
        if !has("x-content-type-options") {
            missing.push("X-Content-Type-Options");
        }
        if ex.scheme == "https" && !has("strict-transport-security") {
            missing.push("Strict-Transport-Security");
        }
        if !has("content-security-policy") && !has("x-frame-options") {
            missing.push("X-Frame-Options");
        }
        if !missing.is_empty() {
            add_note(ex.id, "missing headers", &missing.join(", "));
            let entry = hosts.entry(ex.host.clone()).or_default();
            for m in &missing {
                if !entry.0.contains(m) {
                    entry.0.push(m);
                }
            }
            if entry.1.len() < 5 {
                entry.1.push(ex.id);
            }
        }
        for (_, v) in ex.response_headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")) {
            let name = v.split('=').next().unwrap_or("").trim();
            let attrs = v.to_ascii_lowercase();
            let mut lacks = vec![];
            if ex.scheme == "https" && !attrs.contains("; secure") {
                lacks.push("Secure");
            }
            if !attrs.contains("; httponly") {
                lacks.push("HttpOnly");
            }
            if !lacks.is_empty() && !name.is_empty() {
                let text: String = format!("{name} is set without {}", lacks.join(" or ")).chars().take(200).collect();
                add_note(ex.id, "cookie flags", &text);
            }
        }
    }
    for (host, (missing, ids)) in hosts {
        let title = format!("Missing security headers on {host}");
        let desc = format!(
            "HTML pages on {host} are served without: {}.\n\nThese headers limit cross-site scripting, clickjacking, MIME sniffing and protocol downgrades. Check whether the pages need them, then confirm or dismiss this finding.",
            missing.join(", ")
        );
        unsafe { propose_finding(LOW, title.as_ptr(), title.len(), desc.as_ptr(), desc.len(), ids.as_ptr(), ids.len()) };
    }
    0
}
