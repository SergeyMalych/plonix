//! The WebAssembly sandbox that runs extension code (see `docs/extensions.md`).
//!
//! An extension is a WebAssembly module run by an embedded interpreter
//! (Wasmi, pure Rust). Nothing is linked into it but the host functions
//! below, and each one only when the capability that unlocks it was granted.
//! There is no WASI: no files, sockets, clocks, environment or processes.
//! A module that imports anything else is refused before it ever runs.
//!
//! Every call gets a fresh instance with a fuel budget (instructions), a
//! memory cap and a wall-clock timeout. When an extension runs out of any of
//! them, traps or misbehaves, the call stops with a [`Fault`] and the engine
//! carries on; the caller disables the extension.
//!
//! # Host API, version 1
//!
//! Imports, all from the module `plonix:extension@1`:
//!
//! | Function | Signature | Needs |
//! | --- | --- | --- |
//! | `log` | `(ptr: i32, len: i32)` | always (capped) |
//! | `note` | `(exchange_id: i64, tag_ptr: i32, tag_len: i32, text_ptr: i32, text_len: i32) -> i32` | `passive-analysis` |
//! | `propose_finding` | `(severity: i32, title_ptr: i32, title_len: i32, desc_ptr: i32, desc_len: i32, ids_ptr: i32, ids_count: i32) -> i32` | `propose-findings` |
//!
//! Strings are UTF-8. `severity` is 0 info, 1 low, 2 medium, 3 high,
//! 4 critical. `ids_ptr` points to `ids_count` little-endian i64 exchange
//! ids. Host functions return 0 when accepted, -1 when the input is invalid
//! (outside memory, not UTF-8, too long, control characters), -2 when the
//! per-call limit is reached and -3 when an exchange id was not in the batch.
//!
//! Exports the module must have:
//!
//! - `memory`
//! - `plonix_alloc(len: i32) -> i32`: a buffer of `len` bytes for the engine to write into.
//! - `plonix_analyze(ptr: i32, len: i32) -> i32`: analyze the batch at `ptr` (JSON, see
//!   [`batch_json`]); 0 means success.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use wasmi::{Caller, Config, Engine, Extern, ExternType, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TrapCode, ValType};

use crate::extension::Capability;
use crate::model::{Exchange, SEVERITIES};

/// The import module every host function lives in. The number is the host
/// API's major version.
pub const HOST_MODULE: &str = "plonix:extension@1";

/// Largest module accepted.
pub const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
/// Request and response bodies handed to an extension are cut to this.
pub const MAX_BODY: usize = 64 * 1024;
/// A batch never holds more than this many bytes of JSON.
pub const MAX_BATCH_BYTES: usize = 2 * 1024 * 1024;

const MAX_NOTES: usize = 200;
const MAX_PROPOSALS: usize = 20;
const MAX_LOGS: usize = 50;
const MAX_TAG: usize = 40;
const MAX_NOTE: usize = 500;
const MAX_TITLE: usize = 120;
const MAX_DESCRIPTION: usize = 4000;
const MAX_LOG: usize = 300;
const MAX_IDS: usize = 50;

/// What one call may use.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Instruction budget (Wasmi fuel).
    pub fuel: u64,
    /// Linear memory, in bytes.
    pub memory: usize,
    /// Wall-clock time before the call is abandoned.
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self { fuel: 400_000_000, memory: 32 * 1024 * 1024, timeout: Duration::from_secs(5) }
    }
}

/// Why a call stopped. Any fault disables the extension until the user
/// turns it back on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Used up its instruction budget.
    OutOfFuel,
    /// Did not finish in time.
    Timeout,
    /// Tried to grow its memory past the cap.
    Memory,
    /// Trapped: a panic, `unreachable`, an out-of-bounds access, stack overflow.
    Trap(String),
    /// Broke the host API contract (bad export, bad buffer).
    Contract(String),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::OutOfFuel => write!(f, "it used up its CPU budget for one run (an endless loop, or far too much work)"),
            Fault::Timeout => write!(f, "it did not finish in time"),
            Fault::Memory => write!(f, "it tried to use more memory than extensions are allowed"),
            Fault::Trap(m) => write!(f, "it crashed ({m})"),
            Fault::Contract(m) => write!(f, "it broke the extension API ({m})"),
        }
    }
}

/// A note an extension attached to one exchange.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Note {
    pub exchange_id: i64,
    pub tag: String,
    pub text: String,
}

/// A finding an extension proposes. It is stored unconfirmed and attributed
/// to the extension.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Proposal {
    pub title: String,
    pub severity: String,
    pub description: String,
    pub exchange_ids: Vec<i64>,
}

/// What one call produced.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Output {
    pub notes: Vec<Note>,
    pub proposals: Vec<Proposal>,
    pub logs: Vec<String>,
    /// Calls the host refused (bad input, limits), counted, not fatal.
    pub refused: usize,
    /// Non-zero when `plonix_analyze` reported failure itself.
    pub status: i32,
    pub fuel_used: u64,
}

/// The interpreter, shared by every extension. Fuel metering is on and
/// everything beyond the core WebAssembly features is off.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut c = Config::default();
        c.consume_fuel(true)
            .allow_start_fn(false)
            .wasm_multi_memory(false)
            .wasm_custom_page_sizes(false)
            .set_max_recursion_depth(4096);
        Engine::new(&c)
    })
}

/// A checked, compiled extension module, ready to instantiate.
#[derive(Clone)]
pub struct Compiled {
    module: Module,
}

impl std::fmt::Debug for Compiled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Compiled")
    }
}

/// Compiles a module and checks it against the host API: every import must
/// be a host function, with the right type, that one of `caps` unlocks; the
/// exports the engine calls must exist with the right types.
pub fn compile(wasm: &[u8], caps: &[Capability]) -> Result<Compiled, String> {
    if wasm.len() > MAX_MODULE_BYTES {
        return Err(format!("the module is larger than {} MiB", MAX_MODULE_BYTES / 1024 / 1024));
    }
    if !wasm.starts_with(b"\0asm") {
        return Err("the entry is not a WebAssembly module".into());
    }
    let module = Module::new(engine(), wasm).map_err(|e| format!("not a valid WebAssembly module: {}", crate::detect::clean(&e.to_string(), 200)))?;
    for import in module.imports() {
        let (m, n) = (import.module(), import.name());
        let what = format!("`{}::{}`", crate::detect::clean(m, 60), crate::detect::clean(n, 60));
        if m != HOST_MODULE {
            return Err(format!("it imports {what}, which Plonix does not provide (extensions get no files, network, clock or processes)"));
        }
        let Some((_, params, results, needs)) = HOST_FUNCS.iter().find(|(name, ..)| *name == n) else {
            return Err(format!("it imports {what}, which is not part of the Plonix extension API"));
        };
        let ExternType::Func(ty) = import.ty() else { return Err(format!("{what} must be a function")) };
        if ty.params() != *params || ty.results() != *results {
            return Err(format!("{what} has the wrong signature"));
        }
        if let Some(cap) = needs
            && !caps.contains(cap)
        {
            return Err(format!("it imports {what}, which needs the `{}` capability it does not have", cap_id(*cap)));
        }
    }
    let func = |name: &str, params: &[ValType], results: &[ValType]| match module.get_export(name) {
        Some(ExternType::Func(t)) if t.params() == params && t.results() == results => Ok(()),
        Some(_) => Err(format!("its export `{name}` has the wrong type")),
        None => Err(format!("it does not export `{name}`")),
    };
    if !matches!(module.get_export("memory"), Some(ExternType::Memory(_))) {
        return Err("it does not export its `memory`".into());
    }
    func("plonix_alloc", &[ValType::I32], &[ValType::I32])?;
    func("plonix_analyze", &[ValType::I32, ValType::I32], &[ValType::I32])?;
    Ok(Compiled { module })
}

fn cap_id(c: Capability) -> String {
    serde_json::to_value(c).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

use ValType::{I32, I64};

/// A host function: name, parameters, results, and the capability that links it.
type HostFunc = (&'static str, &'static [ValType], &'static [ValType], Option<Capability>);

const HOST_FUNCS: &[HostFunc] = &[
    ("log", &[I32, I32], &[], None),
    ("note", &[I64, I32, I32, I32, I32], &[I32], Some(Capability::PassiveAnalysis)),
    ("propose_finding", &[I32, I32, I32, I32, I32, I32, I32], &[I32], Some(Capability::ProposeFindings)),
];

struct Host {
    limits: StoreLimits,
    batch: HashSet<i64>,
    out: Output,
}

const INVALID: i32 = -1;
const LIMIT: i32 = -2;
const NOT_IN_BATCH: i32 = -3;

fn read(caller: &Caller<'_, Host>, ptr: i32, len: i32, max: usize) -> Option<Vec<u8>> {
    let mem = caller.get_export("memory").and_then(Extern::into_memory)?;
    let data = mem.data(caller);
    let (ptr, len) = (usize::try_from(ptr).ok()?, usize::try_from(len).ok()?);
    if len > max * 4 {
        return None;
    }
    data.get(ptr..ptr.checked_add(len)?).map(<[u8]>::to_vec)
}

fn read_text(caller: &Caller<'_, Host>, ptr: i32, len: i32, max: usize, multiline: bool) -> Option<String> {
    let s = String::from_utf8(read(caller, ptr, len, max)?).ok()?;
    check_untrusted(&s, max, multiline).ok()?;
    Some(s)
}

/// Text from an extension: bounded, no control characters (newlines and tabs
/// only where asked for) and no invisible or direction-changing characters,
/// so it cannot fake output or hide what it says.
pub fn check_untrusted(s: &str, max: usize, multiline: bool) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err("must not be empty".into());
    }
    if s.chars().count() > max {
        return Err(format!("at most {max} characters"));
    }
    let bad = |c: char| {
        (c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    };
    if s.chars().any(bad) {
        return Err("must not contain control or invisible characters".into());
    }
    Ok(())
}

fn linker(caps: &[Capability]) -> Linker<Host> {
    let mut l = Linker::<Host>::new(engine());
    l.func_wrap(HOST_MODULE, "log", |caller: Caller<'_, Host>, ptr: i32, len: i32| {
        if caller.data().out.logs.len() >= MAX_LOGS {
            return;
        }
        let line = read(&caller, ptr, len, MAX_LOG).map(|b| crate::detect::clean(&String::from_utf8_lossy(&b), MAX_LOG));
        let mut caller = caller;
        if let Some(line) = line.filter(|l| !l.trim().is_empty()) {
            caller.data_mut().out.logs.push(line);
        }
    })
    .expect("log is linked once");
    if caps.contains(&Capability::PassiveAnalysis) {
        l.func_wrap(HOST_MODULE, "note", |mut caller: Caller<'_, Host>, id: i64, tp: i32, tl: i32, xp: i32, xl: i32| -> i32 {
            if caller.data().out.notes.len() >= MAX_NOTES {
                return LIMIT;
            }
            if !caller.data().batch.contains(&id) {
                caller.data_mut().out.refused += 1;
                return NOT_IN_BATCH;
            }
            let (Some(tag), Some(text)) = (read_text(&caller, tp, tl, MAX_TAG, false), read_text(&caller, xp, xl, MAX_NOTE, false)) else {
                caller.data_mut().out.refused += 1;
                return INVALID;
            };
            caller.data_mut().out.notes.push(Note { exchange_id: id, tag, text });
            0
        })
        .expect("note is linked once");
    }
    if caps.contains(&Capability::ProposeFindings) {
        l.func_wrap(
            HOST_MODULE,
            "propose_finding",
            |mut caller: Caller<'_, Host>, sev: i32, tp: i32, tl: i32, dp: i32, dl: i32, ip: i32, ic: i32| -> i32 {
                if caller.data().out.proposals.len() >= MAX_PROPOSALS {
                    return LIMIT;
                }
                let title = read_text(&caller, tp, tl, MAX_TITLE, false);
                let description = if dl == 0 { Some(String::new()) } else { read_text(&caller, dp, dl, MAX_DESCRIPTION, true) };
                let ids = usize::try_from(ic).ok().filter(|n| *n <= MAX_IDS).and_then(|n| read(&caller, ip, (n * 8) as i32, MAX_IDS * 8));
                let severity = usize::try_from(sev).ok().and_then(|i| SEVERITIES.get(i));
                let (Some(title), Some(description), Some(ids), Some(severity)) = (title, description, ids, severity) else {
                    caller.data_mut().out.refused += 1;
                    return INVALID;
                };
                let ids: Vec<i64> = ids.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap_or_default())).collect();
                if ids.iter().any(|i| !caller.data().batch.contains(i)) {
                    caller.data_mut().out.refused += 1;
                    return NOT_IN_BATCH;
                }
                let p = Proposal { title, severity: severity.to_string(), description, exchange_ids: ids };
                caller.data_mut().out.proposals.push(p);
                0
            },
        )
        .expect("propose_finding is linked once");
    }
    l
}

/// The JSON an extension receives: the exchanges it may see, with bodies
/// as text cut to [`MAX_BODY`]. `in_scope` says whether each host is accepted.
pub fn batch_json(exchanges: &[(&Exchange, bool)]) -> Vec<u8> {
    let body = |b: &[u8]| {
        let cut = &b[..b.len().min(MAX_BODY)];
        String::from_utf8_lossy(cut).into_owned()
    };
    let items: Vec<Value> = exchanges
        .iter()
        .map(|(ex, in_scope)| {
            json!({
                "id": ex.id, "method": ex.method, "scheme": ex.scheme, "host": ex.host, "port": ex.port,
                "path": ex.path, "query": ex.query, "in_scope": in_scope,
                "request_headers": ex.req_headers, "request_body": body(&ex.req_body),
                "request_body_truncated": ex.req_truncated || ex.req_body.len() > MAX_BODY,
                "status": ex.status, "mime": ex.mime(), "response_headers": ex.resp_headers, "response_body": body(&ex.resp_body),
                "response_body_truncated": ex.resp_truncated || ex.resp_body.len() > MAX_BODY,
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "api": 1, "exchanges": items })).unwrap_or_default()
}

/// Runs `plonix_analyze` over one batch in a fresh, metered instance.
///
/// The call runs on a thread of its own, so a module that does not finish
/// in time is abandoned and the caller gets [`Fault::Timeout`]; the fuel
/// budget still stops that thread soon after.
pub fn analyze(compiled: &Compiled, caps: &[Capability], batch: &[u8], ids: &[i64], limits: Limits) -> Result<Output, Fault> {
    if batch.len() > MAX_BATCH_BYTES {
        return Err(Fault::Contract("batch too large".into()));
    }
    let module = compiled.module.clone();
    let linker = linker(caps);
    let batch = batch.to_vec();
    let ids: HashSet<i64> = ids.iter().copied().collect();
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let spawned = std::thread::Builder::new().name("plonix-extension".into()).stack_size(8 * 1024 * 1024).spawn(move || {
        let _ = tx.send(run(&module, &linker, &batch, ids, limits));
    });
    if spawned.is_err() {
        return Err(Fault::Contract("could not start the sandbox".into()));
    }
    match rx.recv_timeout(limits.timeout.saturating_sub(started.elapsed())) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Fault::Timeout),
        // The thread ended without an answer: the host side panicked.
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Fault::Trap("the sandbox stopped unexpectedly".into())),
    }
}

fn run(module: &Module, linker: &Linker<Host>, batch: &[u8], ids: HashSet<i64>, limits: Limits) -> Result<Output, Fault> {
    let host = Host {
        limits: StoreLimitsBuilder::new().memory_size(limits.memory).memories(1).tables(1).table_elements(100_000).instances(1).trap_on_grow_failure(true).build(),
        batch: ids,
        out: Output::default(),
    };
    let mut store = Store::new(engine(), host);
    store.limiter(|h| &mut h.limits);
    store.set_fuel(limits.fuel).map_err(|e| Fault::Contract(e.to_string()))?;
    let instance = linker.instantiate_and_start(&mut store, module).map_err(fault)?;
    let memory = instance.get_memory(&store, "memory").ok_or_else(|| Fault::Contract("no memory export".into()))?;
    let alloc = instance.get_typed_func::<i32, i32>(&store, "plonix_alloc").map_err(|e| Fault::Contract(e.to_string()))?;
    let analyze = instance.get_typed_func::<(i32, i32), i32>(&store, "plonix_analyze").map_err(|e| Fault::Contract(e.to_string()))?;
    let len = i32::try_from(batch.len()).map_err(|_| Fault::Contract("batch too large".into()))?;
    let ptr = alloc.call(&mut store, len).map_err(fault)?;
    let start = usize::try_from(ptr).map_err(|_| Fault::Contract("plonix_alloc returned a negative pointer".into()))?;
    memory
        .write(&mut store, start, batch)
        .map_err(|_| Fault::Contract("plonix_alloc returned a buffer outside its memory".into()))?;
    let status = analyze.call(&mut store, (ptr, len)).map_err(fault)?;
    let left = store.get_fuel().unwrap_or(0);
    let mut out = std::mem::take(&mut store.data_mut().out);
    out.status = status;
    out.fuel_used = limits.fuel.saturating_sub(left);
    Ok(out)
}

fn fault(e: wasmi::Error) -> Fault {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => Fault::OutOfFuel,
        Some(TrapCode::GrowthOperationLimited) => Fault::Memory,
        Some(code) => Fault::Trap(code.to_string()),
        None => {
            let msg = e.to_string();
            if msg.contains("fuel") {
                Fault::OutOfFuel
            } else if msg.contains("memory") && (msg.contains("limit") || msg.contains("minimum")) {
                Fault::Memory
            } else {
                Fault::Trap(crate::detect::clean(&msg, 200))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &[u8] = include_bytes!("../../../examples/extensions/security-headers/security_headers.wasm");
    const ALL: &[Capability] = &[Capability::ReadTraffic, Capability::PassiveAnalysis, Capability::ProposeFindings];

    fn quick() -> Limits {
        Limits { fuel: 5_000_000, memory: 4 * 1024 * 1024, timeout: Duration::from_secs(20) }
    }

    fn page(id: i64, headers: Vec<(&str, &str)>) -> Exchange {
        Exchange {
            id,
            scheme: "https".into(),
            host: "shop.test".into(),
            port: 443,
            method: "GET".into(),
            path: "/".into(),
            status: Some(200),
            resp_headers: headers.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
            resp_body: b"<html></html>".to_vec(),
            ..Default::default()
        }
    }

    /// A module from WebAssembly text with the two exports the engine calls;
    /// `body` is the body of `plonix_analyze`.
    fn module(imports: &str, body: &str, memory: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module {imports}
                (memory (export "memory") {memory})
                (func (export "plonix_alloc") (param i32) (result i32) (i32.const 1024))
                (func (export "plonix_analyze") (param i32 i32) (result i32) {body}))"#
        ))
        .unwrap()
    }

    fn run_wat(imports: &str, body: &str, limits: Limits) -> Result<Output, Fault> {
        let compiled = compile(&module(imports, body, "1"), ALL).unwrap();
        let ex = page(7, vec![("Content-Type", "text/html")]);
        analyze(&compiled, ALL, &batch_json(&[(&ex, true)]), &[7], limits)
    }

    #[test]
    fn the_example_extension_runs() {
        let compiled = compile(EXAMPLE, ALL).unwrap();
        let bare = page(1, vec![("Content-Type", "text/html; charset=utf-8"), ("Set-Cookie", "sid=abc; Path=/")]);
        let good = page(
            2,
            vec![
                ("Content-Type", "text/html"),
                ("Content-Security-Policy", "default-src 'self'"),
                ("X-Content-Type-Options", "nosniff"),
                ("Strict-Transport-Security", "max-age=31536000"),
            ],
        );
        let out = analyze(&compiled, ALL, &batch_json(&[(&bare, true), (&good, true)]), &[1, 2], Limits::default()).unwrap();
        assert_eq!(out.status, 0);
        assert!(out.notes.iter().any(|n| n.exchange_id == 1 && n.tag == "missing headers" && n.text.contains("Content-Security-Policy")), "{:?}", out.notes);
        assert!(out.notes.iter().any(|n| n.exchange_id == 1 && n.tag == "cookie flags" && n.text.contains("sid")), "{:?}", out.notes);
        assert!(!out.notes.iter().any(|n| n.exchange_id == 2), "{:?}", out.notes);
        assert_eq!(out.proposals.len(), 1);
        assert_eq!(out.proposals[0].title, "Missing security headers on shop.test");
        assert_eq!((out.proposals[0].severity.as_str(), out.proposals[0].exchange_ids.as_slice()), ("low", &[1][..]));
        assert!(out.fuel_used > 0);
    }

    #[test]
    fn imports_are_only_the_host_api_and_only_what_was_granted() {
        // WASI, or anything else outside the host API, is refused before anything runs.
        let wasi = module(r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#, "(i32.const 0)", "1");
        assert!(compile(&wasi, ALL).unwrap_err().contains("does not provide"));
        let unknown = module(r#"(import "plonix:extension@1" "send" (func (param i32 i32) (result i32)))"#, "(i32.const 0)", "1");
        assert!(compile(&unknown, ALL).unwrap_err().contains("not part of the Plonix extension API"));
        let wrong = module(r#"(import "plonix:extension@1" "note" (func (param i32) (result i32)))"#, "(i32.const 0)", "1");
        assert!(compile(&wrong, ALL).unwrap_err().contains("wrong signature"));
        let host_memory = wat::parse_str(r#"(module (import "plonix:extension@1" "memory" (memory 1)))"#).unwrap();
        assert!(compile(&host_memory, ALL).is_err());
        // `note` exists only for passive-analysis.
        let notes = module(r#"(import "plonix:extension@1" "note" (func (param i64 i32 i32 i32 i32) (result i32)))"#, "(i32.const 0)", "1");
        assert!(compile(&notes, &[Capability::ReadTraffic, Capability::ProposeFindings]).unwrap_err().contains("passive-analysis"));
        assert!(compile(&notes, ALL).is_ok());
        // The engine's entry points must be there.
        let no_exports = wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap();
        assert!(compile(&no_exports, ALL).unwrap_err().contains("plonix_alloc"));
        assert!(compile(b"not wasm at all", ALL).is_err());
        // A start function would run before the engine calls anything.
        let start = wat::parse_str(
            r#"(module (memory (export "memory") 1) (func $s) (start $s)
                (func (export "plonix_alloc") (param i32) (result i32) (i32.const 0))
                (func (export "plonix_analyze") (param i32 i32) (result i32) (i32.const 0)))"#,
        )
        .unwrap();
        assert!(compile(&start, ALL).is_err());
    }

    #[test]
    fn an_endless_loop_runs_out_of_fuel() {
        let r = run_wat("", "(loop $l (br $l)) (i32.const 0)", quick());
        assert_eq!(r.unwrap_err(), Fault::OutOfFuel);
    }

    #[test]
    fn a_slow_call_times_out_and_the_caller_moves_on() {
        let started = Instant::now();
        let limits = Limits { fuel: 50_000_000_000, timeout: Duration::from_millis(200), ..quick() };
        assert_eq!(run_wat("", "(loop $l (br $l)) (i32.const 0)", limits).unwrap_err(), Fault::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn memory_is_capped() {
        // Growing past the cap stops the call.
        let r = run_wat("", "(drop (memory.grow (i32.const 1000))) (i32.const 0)", quick());
        assert_eq!(r.unwrap_err(), Fault::Memory);
        // Growing a little is fine.
        assert!(run_wat("", "(drop (memory.grow (i32.const 2))) (i32.const 0)", quick()).is_ok());
        // Asking for a huge memory up front: the instance is never created.
        let huge = compile(&module("", "(i32.const 0)", "20000"), ALL).unwrap();
        let ex = page(7, vec![]);
        assert_eq!(analyze(&huge, ALL, &batch_json(&[(&ex, true)]), &[7], quick()).unwrap_err(), Fault::Memory);
    }

    #[test]
    fn crashes_are_contained() {
        assert!(matches!(run_wat("", "(unreachable)", quick()), Err(Fault::Trap(_))));
        assert!(matches!(run_wat("", "(i32.load (i32.const -1))", quick()), Err(Fault::Trap(_))));
        let recurse = wat::parse_str(
            r#"(module (memory (export "memory") 1)
                (func $r (result i32) (call $r))
                (func (export "plonix_alloc") (param i32) (result i32) (i32.const 0))
                (func (export "plonix_analyze") (param i32 i32) (result i32) (call $r)))"#,
        )
        .unwrap();
        let compiled = compile(&recurse, ALL).unwrap();
        let ex = page(7, vec![]);
        let deep = Limits { fuel: 500_000_000, ..quick() };
        assert!(matches!(analyze(&compiled, ALL, &batch_json(&[(&ex, true)]), &[7], deep), Err(Fault::Trap(_))));
        // A buffer outside its own memory.
        let bad_alloc = wat::parse_str(
            r#"(module (memory (export "memory") 1)
                (func (export "plonix_alloc") (param i32) (result i32) (i32.const 65530))
                (func (export "plonix_analyze") (param i32 i32) (result i32) (i32.const 0)))"#,
        )
        .unwrap();
        let compiled = compile(&bad_alloc, ALL).unwrap();
        assert!(matches!(analyze(&compiled, ALL, &batch_json(&[(&ex, true)]), &[7], quick()), Err(Fault::Contract(_))));
    }

    #[test]
    fn host_calls_check_what_they_are_given() {
        let note = r#"(import "plonix:extension@1" "note" (func $note (param i64 i32 i32 i32 i32) (result i32)))
            (data (i32.const 0) "tagtext\1b[2Jgone")"#;
        // Accepted; refused for an exchange it was not given, for control characters, and outside memory.
        let body = r#"(i32.store (i32.const 100) (call $note (i64.const 7) (i32.const 0) (i32.const 3) (i32.const 3) (i32.const 4)))
            (i32.store (i32.const 104) (call $note (i64.const 8) (i32.const 0) (i32.const 3) (i32.const 3) (i32.const 4)))
            (i32.store (i32.const 108) (call $note (i64.const 7) (i32.const 0) (i32.const 3) (i32.const 3) (i32.const 11)))
            (i32.store (i32.const 112) (call $note (i64.const 7) (i32.const 0) (i32.const 3) (i32.const 65534) (i32.const 10)))
            (i32.const 0)"#;
        let out = run_wat(note, body, quick()).unwrap();
        assert_eq!(out.notes, vec![Note { exchange_id: 7, tag: "tag".into(), text: "text".into() }]);
        assert_eq!(out.refused, 3);
        assert!(check_untrusted("a\u{202e}b", 10, false).is_err());
        assert!(check_untrusted("line\nline", 20, true).is_ok() && check_untrusted("line\nline", 20, false).is_err());
    }
}
