//! Extensions in a running engine: the example analyzer reads traffic and
//! proposes findings attributed to it; a misbehaving one is stopped,
//! switched off with a reason, and the engine carries on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use plonix_core::ca::CertAuthority;
use plonix_core::engine::{Engine, extension_author};
use plonix_core::extension::{self, Capability, Consent, ExtensionLibrary};
use plonix_core::insight::Category;
use plonix_core::model::Exchange;
use plonix_core::sandbox::Limits;
use plonix_core::scope::Decision;
use plonix_core::store::Store;
use plonix_core::upstream::Upstream;

const EXAMPLE: &[u8] = include_bytes!("../../../store/extensions/security-headers.plonixext");

fn engine(dir: &std::path::Path) -> Arc<Engine> {
    let (cert, key) = CertAuthority::generate_pem().unwrap();
    let ca = Arc::new(CertAuthority::from_pem(&cert, &key).unwrap());
    let engine = Engine::new("test", Store::open_in_memory().unwrap(), ca, Upstream::new(false, vec![]).unwrap()).unwrap();
    engine.set_extension_library(ExtensionLibrary::at(dir));
    engine.set_extension_limits(Limits { fuel: 50_000_000, timeout: Duration::from_secs(20), ..Limits::default() });
    engine.decide("shop.test", Decision::Accepted, false, "").unwrap();
    engine
}

fn page(host: &str) -> Exchange {
    Exchange {
        scheme: "https".into(),
        host: host.into(),
        port: 443,
        method: "GET".into(),
        path: "/".into(),
        status: Some(200),
        resp_headers: vec![("Content-Type".into(), "text/html".into())],
        resp_body: b"<html></html>".to_vec(),
        ..Default::default()
    }
}

fn wat_package(name: &str, body: &str) -> Vec<u8> {
    let manifest = format!(
        r#"{{"plonix_extension":1,"name":"{name}","version":"1.0.0","description":"d","author":"a","runtime":"wasm",
            "entry":"x.wasm","capabilities":["read-traffic","passive-analysis"]}}"#
    );
    let wasm = wat::parse_str(format!(
        r#"(module (memory (export "memory") 1)
            (func (export "plonix_alloc") (param i32) (result i32) (i32.const 1024))
            (func (export "plonix_analyze") (param i32 i32) (result i32) {body}))"#
    ))
    .unwrap();
    extension::pack(manifest.as_bytes(), &wasm).unwrap()
}

#[test]
fn the_example_proposes_findings_from_captured_traffic() {
    let dir = tempfile::tempdir().unwrap();
    let lib = ExtensionLibrary::at(dir.path());
    lib.install(EXAMPLE, "test", None, &Consent::default()).unwrap();
    let engine = engine(dir.path());
    let in_scope = engine.record(page("shop.test")).unwrap();
    let outside = engine.record(page("cdn.other.test")).unwrap();

    // Newly captured traffic reaches it in the background.
    let by = extension_author("security-headers");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !engine.store.findings().unwrap().iter().any(|f| f.created_by == by) {
        assert!(Instant::now() < deadline, "no finding was proposed");
        std::thread::sleep(Duration::from_millis(50));
    }
    let findings = engine.store.findings().unwrap();
    let f = findings.iter().find(|f| f.created_by == by).unwrap();
    assert_eq!((f.title.as_str(), f.status.as_str(), f.exchange_ids.as_slice()), ("Missing security headers on shop.test", "open", &[in_scope][..]));
    assert!(f.description.contains("Proposed by the extension security-headers"));

    // Running it again over everything adds nothing new, and it never sees out-of-scope traffic.
    let run = engine.run_extension_on_traffic("security-headers").unwrap();
    assert_eq!((run.exchanges, run.proposed, run.stopped.as_deref()), (1, 0, None));
    assert_eq!(engine.store.findings().unwrap().len(), 1);

    // Its notes show in the Lens, labelled as coming from it.
    let ex = engine.store.get_exchange(in_scope).unwrap().unwrap();
    let notes = engine.extension_insights(&ex);
    assert!(notes.iter().any(|i| i.category == Category::Extension && i.location == "extension security-headers"), "{notes:?}");
    let ex = engine.store.get_exchange(outside).unwrap().unwrap();
    assert!(engine.extension_insights(&ex).is_empty());

    // Switched off, it no longer runs.
    lib.set_enabled("security-headers", false).unwrap();
    assert!(engine.run_extension_on_traffic("security-headers").is_err());
}

#[test]
fn a_misbehaving_extension_is_stopped_and_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let lib = ExtensionLibrary::at(dir.path());
    lib.install(&wat_package("spinner", "(loop $l (br $l)) (i32.const 0)"), "test", None, &Consent::default()).unwrap();
    lib.install(&wat_package("crasher", "(unreachable)"), "test", None, &Consent::default()).unwrap();
    let engine = engine(dir.path());
    engine.record(page("shop.test")).unwrap();

    for name in ["spinner", "crasher"] {
        let run = engine.run_extension_on_traffic(name);
        // Either the background worker or this run stopped it first.
        if let Ok(run) = run {
            assert!(run.stopped.is_some(), "{name}: {run:?}");
        }
        let info = lib.info(name).unwrap();
        assert!(!info.state.enabled, "{name} is still on");
        assert!(info.state.disabled_reason.as_deref().unwrap_or("").contains("switched it off"), "{info:?}");
    }
    assert!(engine.extensions().extensions.is_empty());

    // The engine is unharmed.
    let id = engine.record(page("shop.test")).unwrap();
    assert!(engine.store.get_exchange(id).unwrap().is_some());

    // Turned back on by the user, it is loaded again. The background worker
    // may still hold the page recorded above: then it runs the crasher on it
    // and switches it off again, which shows it was loaded too.
    lib.set_enabled("crasher", true).unwrap();
    let loaded = engine.extensions().extensions.len() == 1;
    let stopped_again = || lib.info("crasher").unwrap().state.disabled_reason.is_some();
    assert!(loaded || stopped_again(), "the crasher was not loaded again");
}

const SUBDOMAIN: &[u8] = include_bytes!("../../../store/extensions/subdomain-discovery.plonixext");
const PARAM_PROBE: &[u8] = include_bytes!("../../../store/extensions/parameter-probe.plonixext");

// A subdomain finder cannot run its tool until the user grants the sensitive
// run-program capability; Consent::default() withholds it.
#[test]
fn subdomain_discovery_needs_permission_to_run_its_tool() {
    let dir = tempfile::tempdir().unwrap();
    let lib = ExtensionLibrary::at(dir.path());
    lib.install(SUBDOMAIN, "test", None, &Consent::default()).unwrap();
    let engine = engine(dir.path());
    let run = engine.run_extension_on_traffic("subdomain-discovery").unwrap();
    let problem = run.problem.as_deref().unwrap_or("");
    assert!(problem.contains("not allowed to run subfinder") && problem.contains("plonix extensions allow subdomain-discovery"), "{run:?}");
    // Allowed later, without reinstalling, it gets past the permission check.
    lib.set_granted("subdomain-discovery", Capability::RunProgram, true).unwrap();
    let run = engine.run_extension_on_traffic("subdomain-discovery").unwrap();
    assert!(!run.problem.as_deref().unwrap_or("").contains("not allowed"), "{run:?}");
    assert_eq!(run.exchanges, 0, "it does not read traffic");
}

// The parameter probe refuses an out-of-scope target: every request it would
// send goes through the same scope choke point as replay.
#[test]
fn parameter_probe_refuses_out_of_scope_and_needs_permission() {
    use plonix_core::engine::SendError;
    let dir = tempfile::tempdir().unwrap();
    let lib = ExtensionLibrary::at(dir.path());
    // Granted its sensitive capabilities so we reach the scope check.
    let consent = Consent { grant: vec![Capability::RunProgram, Capability::ScopedRequests], approve_new: true };
    lib.install(PARAM_PROBE, "test", None, &consent).unwrap();
    let eng = engine(dir.path());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let err = rt.block_on(eng.run_param_probe("parameter-probe", "https://out-of-scope.test/api")).unwrap_err();
    assert!(matches!(err, SendError::OutOfScope { .. }), "{err:?}");

    // Without consent to send scoped requests it does not probe at all.
    let dir2 = tempfile::tempdir().unwrap();
    let lib2 = ExtensionLibrary::at(dir2.path());
    lib2.install(PARAM_PROBE, "test", None, &Consent::default()).unwrap();
    let eng2 = engine(dir2.path());
    let err = rt.block_on(eng2.run_param_probe("parameter-probe", "https://shop.test/api")).unwrap_err();
    assert!(matches!(err, SendError::BadRequest(m) if m.contains("not allowed to send its requests")), "expected a permission error");
}
