# Extensions

Plonix is meant to be maintained by its community. Extensions are how people add what the core deliberately leaves out: new detections, passive analyzers for a specific framework, helpers for a token format, finding templates. This document is the design for the extension system: what an extension is, how it's found, installed and loaded, and above all **what it is never allowed to do**.

**Status.** Two pieces are live today:

- **Rule packs** are installable from files, URLs and the store, with schema validation and SHA-256 pinning. See [detection-rules.md](detection-rules.md).
- The **store** client (`plonix store`) lists packages from any JSON index and installs rule packs, verifying checksums.

The **extension manifest and capability model** are implemented and tested (`crates/plonix-core/src/extension.rs`), so the contract is fixed before any code runs. The **code runtime is not built**. Store entries of kind `extension` are listed but refused at install time. That is deliberate: an unsandboxed plugin loader in a security tool would be a backdoor with a nice API, and we won't ship one.

## Contents

- [Threat model](#threat-model)
- [The trust boundary](#the-trust-boundary)
- [Invariants](#invariants)
- [Extension kinds](#extension-kinds)
- [Manifest](#manifest)
- [Capabilities](#capabilities)
- [Sandbox: WebAssembly](#sandbox-webassembly)
- [Lifecycle](#lifecycle)
- [Store and distribution](#store-and-distribution)
- [Versioning](#versioning)
- [Roadmap](#roadmap)

## Threat model

Plonix sits on the most sensitive data a researcher has: authenticated traffic, session tokens, API keys, and requests to systems they're authorized to test, and only those. An extension is code or data written by someone the user has never met, installed with one command. We assume:

- **An extension may be malicious** from the start, or become malicious in an update (a compromised maintainer account, a typo-squatted name, a sold project).
- **A download may be tampered with** between the author and the user.
- **An extension may be buggy**: infinite loops, huge allocations, panics.

It must not be able to:

1. **Exfiltrate data**: send captured traffic, tokens or findings anywhere.
2. **Attack out of scope**: send requests to hosts the user has not accepted. That is the difference between authorized testing and an incident.
3. **Change scope** or any other user decision.
4. **Touch the machine**: read `~/.ssh`, write files, spawn processes, open sockets.
5. **Deceive the user**: forge output, impersonate the engine, or pass off a proposed finding as a confirmed one.
6. **Take the engine down**.

## The trust boundary

```text
 ┌──────────────────── trusted ─────────────────────┐   ┌──────── untrusted ─────────┐
 │                                                  │   │                            │
 │  User ── GUI / CLI / MCP ── Local API ── Engine ─┼───┼─▶ host API ─▶ extension   │
 │                                   │              │   │  (capability-checked,      │
 │                         scope enforcement        │   │   metered, no ambient      │
 │                                   │              │   │   authority)               │
 │                              Upstream ── network │   │                            │
 └──────────────────────────────────────────────────┘   │  rule packs (data only)    │
                                                        └────────────────────────────┘
```

Everything to the right of the line is untrusted: rule packs, store indexes, extension manifests, extension code, and anything an extension returns. The engine is the only component that touches the network, the file system or the database. Extensions get **no ambient authority**. They can only call the functions the engine hands them, and only the ones their granted capabilities unlock. Every such call goes through the same code path as the CLI and agents, including **scope enforcement**.

## Invariants

These hold for every extension, whatever capabilities it has. They're enforced by the engine, not by convention, and no manifest field, capability or user setting turns them off.

1. **Scope is absolute.** An extension can never cause a request to a host that isn't accepted. Even with `scoped-requests`, every request goes through `Engine::send`, which refuses out-of-scope hosts exactly as it does for `plonix replay` and AI agents. The request is recorded with the extension as its initiator.
2. **No network.** There is no socket, DNS or HTTP API. The only way out is `scoped-requests`, which is (1).
3. **No file system, no processes, no environment.** Not even read-only. Configuration an extension needs is passed in by the engine.
4. **Read-only decisions.** Scope rules, project settings and the user's findings can't be changed. Extensions can *propose* findings, which are stored as created by the extension and marked unconfirmed until a person confirms them.
5. **Bounded resources.** Each call has a fuel (instruction) budget, a memory cap and a wall-clock timeout. An extension that exceeds them is stopped, and one that keeps doing it is disabled.
6. **Data, not markup.** Everything an extension returns is plain structured data that the engine validates (lengths, character sets, no control characters) before showing it anywhere.
7. **Pinned bytes.** What runs is exactly what was verified at install time (SHA-256 against the store index), and it's re-verified on load.

## Extension kinds

| Kind | Runtime | Can do | Status |
| --- | --- | --- | --- |
| Rule pack | none (data) | Detect technologies | **Shipped** |
| Declarative extension | none (data) | Bundle one or more rule packs under one name and version | Manifest implemented; install planned |
| Lens detector | none (data) | Flag a value in requests and responses: a regex, a label and a category (personal data, secret, decodable, info) | Built-in set shipped; packs planned |
| WASM extension | WebAssembly sandbox | Passive analysis, finding proposals, scoped requests, per its capabilities | **Designed** (this document) |

The guiding rule: **anything that can be data is data.** Most community contributions (detecting a framework, flagging a header, recognising a token format) should be declarative, because data can be fully validated and can't misbehave. Code is for what data can't express.

## Manifest

An extension package is a directory (distributed as a single archive) with a `plonix-extension.json` manifest:

```json
{
  "plonix_extension": 1,
  "name": "jwt-workbench",
  "version": "0.3.1",
  "description": "Decodes JWTs in traffic, flags alg=none and weak HMAC secrets",
  "author": "Jane Researcher",
  "homepage": "https://github.com/jane/plonix-jwt-workbench",
  "runtime": "wasm",
  "entry": "jwt_workbench.wasm",
  "rule_packs": ["rules/jwt.json"],
  "capabilities": ["read-traffic", "passive-analysis", "propose-findings"]
}
```

Validation (implemented in `extension::parse_manifest`):

- Unknown fields are rejected.
- `name` and `version` follow the same rules as rule packs.
- `entry` and `rule_packs` must be relative paths inside the package, without `..`, absolute paths or unusual characters.
- `runtime: declarative` may not have an `entry` and may only ask for `detection-rules`.
- `runtime: wasm` needs an `entry` ending in `.wasm`.
- Capabilities must come from the closed list below. Duplicates and impossible combinations are rejected.

## Capabilities

The capability list is **closed**: the manifest parser rejects anything not on it. There is deliberately no `network`, `filesystem`, `exec`, `modify-scope` or `send-anywhere` capability. A permission that doesn't exist can't be granted by mistake or by social engineering.

| Capability | Lets the extension | Granted |
| --- | --- | --- |
| `read-traffic` | Receive captured exchanges for **in-scope** hosts, delivered by the engine | at install |
| `read-out-of-scope` | Also receive exchanges for hosts outside scope (needs `read-traffic`) | explicit yes |
| `read-scope` | Read scope rules and suggestions (never change them) | at install |
| `detection-rules` | Contribute detection rules, validated like any rule pack | at install |
| `passive-analysis` | Return tags and notes for exchanges it was given | at install |
| `propose-findings` | Propose findings, stored as unconfirmed and attributed to the extension | at install |
| `scoped-requests` | Ask the engine to send requests. **Scope-enforced, rate-limited and recorded**, exactly like agent requests | explicit yes |

At install time the user sees the capabilities in plain words (`Capability::describe`). The two **sensitive** ones (`read-out-of-scope`, `scoped-requests`) each need a separate, explicit yes. An update that asks for **new** capabilities isn't applied silently: it waits for the user to approve the difference.

## Sandbox: WebAssembly

**Recommendation: run extension code as WebAssembly components in an embedded runtime (Wasmtime), with no WASI file system, network or clock imports, and a host API defined in WIT that maps one-to-one to capabilities.**

Why WebAssembly:

- **Deny by default.** A WASM module can't do anything it isn't given an import for. That's what we need: capabilities become the *only* imports. Compare dynamic libraries (`.dylib`), which run with the full rights of the Plonix process, and scripting runtimes, whose standard libraries have to be stripped and audited one function at a time.
- **Resource limits built in.** Wasmtime supports fuel metering (instruction budgets), epoch interruption (wall-clock timeouts) and memory limits per instance. A runaway extension is stopped, and the engine keeps running.
- **Any language.** Authors can write in Rust, Go, AssemblyScript, C, Zig, or Python and JavaScript through componentized interpreters. The community isn't tied to one language.
- **Portable and reproducible.** One `.wasm` file runs on every macOS architecture and on Linux CI, and its SHA-256 identifies the exact code that runs.
- **Mature in exactly this role.** It's the model behind proxy-wasm in Envoy, plus Zed, Extism and Shopify Functions: untrusted third-party code inside a sensitive host.

Alternatives considered:

| Option | Why not (as the primary model) |
| --- | --- |
| Native plugins (`.dylib`) | Full process rights: no isolation at all. Rejected. |
| Out-of-process plugins over IPC | Isolation depends on OS sandboxing (macOS `sandbox-exec` is deprecated, App Sandbox needs separate signed helpers). Heavier, and harder to make deny-by-default. A possible later option for heavyweight tools, behind the same capability API. |
| Embedded Lua or JavaScript | Workable, but the sandbox is "remove dangerous globals", which is easy to get wrong, and resource limits are weaker. One language only. |
| Declarative only | Already the default for everything it can express (rule packs). Not enough for analyzers that need real logic. |

### Host API sketch (WIT)

```wit
package plonix:extension@1.0.0;

interface types {
  record header { name: string, value: string }
  record exchange {
    id: s64, method: string, scheme: string, host: string, port: u16,
    path: string, query: string,
    request-headers: list<header>, request-body: list<u8>,
    status: option<u16>, response-headers: list<header>, response-body: list<u8>,
  }
  record note { exchange-id: s64, tag: string, text: string }
  enum severity { info, low, medium, high, critical }
  record finding-proposal { title: string, severity: severity, description: string, exchange-ids: list<s64> }
}

// Functions the engine provides. Each one is linked only when the
// matching capability was granted; otherwise the import doesn't exist.
interface host {
  use types.{exchange, finding-proposal};
  log: func(message: string);                                       // always; rate-limited
  propose-finding: func(f: finding-proposal) -> result<s64, string>; // propose-findings
  send: func(method: string, url: string, headers: list<tuple<string, string>>, body: list<u8>)
      -> result<exchange, string>;                                  // scoped-requests; scope-enforced
}

// Functions the extension provides.
world analyzer {
  use types.{exchange, note};
  import host;
  export analyze: func(batch: list<exchange>) -> list<note>;        // read-traffic + passive-analysis
}
```

The engine delivers traffic **to** the extension in batches. The extension never queries the database, so the engine decides what it sees: in-scope only unless `read-out-of-scope` was granted, and with bodies size-capped.

## Lifecycle

```text
 discover ──▶ fetch ──▶ verify ──▶ validate ──▶ consent ──▶ install ──▶ load ──▶ run
   store       https     sha256     manifest     show caps   pin bytes   re-verify   sandboxed,
   index       size-cap  vs index   + content    sensitive   lock.json   on every    metered,
                                                 = explicit               load        disable-able
```

1. **Discover**: `plonix store` reads a JSON index (see [Store and distribution](#store-and-distribution)).
2. **Fetch**: over `https://` only (or a local path), size-capped, redirects can't leave https.
3. **Verify**: SHA-256 of the downloaded bytes must equal the index entry. A mismatch stops everything before any write.
4. **Validate**: the manifest is parsed strictly. Bundled rule packs are validated like any other. The WASM module is compiled and its imports are checked against the granted capabilities: a module that imports anything outside the host API is rejected.
5. **Consent**: the user sees what the extension can do in plain words, and sensitive capabilities need an explicit yes.
6. **Install**: bytes go to `~/.plonix/extensions/<name>/`, with name, version, sha256, source and granted capabilities recorded in a lock file.
7. **Load**: on engine start, each extension is re-verified against the lock. Anything that changed on disk is skipped and reported.
8. **Run**: each call gets a fresh, metered instance (or a pooled one, reset between calls). Faults and limit violations are logged. Repeated violations disable the extension until the user re-enables it.

`plonix ext list | info | disable | enable | remove` will manage installed extensions, mirroring `plonix rules`.

## Store and distribution

The store is a static JSON index, already implemented for rule packs (`plonix_core::registry`). Extensions use the same index with `"kind": "extension"`:

- **Hosted anywhere.** The community index lives in this repository (`store/index.json`). Teams can host private indexes on any https server, or a folder, and point Plonix at them with `--index` or `$PLONIX_STORE_INDEX`.
- **Reviewed by pull request.** Adding or updating a community package means a PR that changes `store/index.json`, including the new SHA-256. Review approves specific bytes, and the checksum makes that approval stick.
- **The index is the root of trust.** Whoever controls an index decides which bytes are acceptable. Later hardening: signed indexes (minisign or Sigstore), so a mirror can serve the index without being trusted.
- **No install scripts, ever.** Installing copies verified bytes. Nothing in a package runs at install time.

## Versioning

- Packages use `MAJOR.MINOR.PATCH`. `plonix store update` upgrades to newer versions.
- The **host API** is versioned in its WIT package name (`plonix:extension@1.0.0`). Plonix supports the current major version and links older minor versions. An extension built for a newer API than the engine has is refused with a clear message.
- File formats carry their own version (`plonix_pack`, `plonix_index`, `plonix_extension`). Readers refuse versions they don't know rather than guessing.
- An update that adds capabilities needs fresh consent (see [Lifecycle](#lifecycle)).

## Roadmap

1. ✅ Rule packs: format, validation, install from file or URL, SHA-256 pinning, built-in starter packs.
2. ✅ Store index and client for rule packs (`plonix store list|install|update`).
3. ✅ Extension manifest and closed capability list, validated and tested.
4. Declarative extensions: install bundles of rule packs through the same flow.
   Lens detectors come next in the same format: today's built-in pattern detectors (`crates/plonix-core/src/insight.rs`) are already plain data, so a pack only needs a schema for them. Decoders that need code (JWT, Base64, hex) stay built in.
5. WASM runtime: Wasmtime with fuel, epoch and memory limits; `analyzer` world with `read-traffic` and `passive-analysis`.
6. `propose-findings`, then `scoped-requests`, reusing the engine's scope enforcement and recording.
7. Signed store indexes.
8. GUI: an Extensions pane in the Mac app with the same consent flow.
