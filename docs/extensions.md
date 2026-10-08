# Extensions

Plonix is meant to be maintained by its community. Extensions are how people add what the core deliberately leaves out: new detections, filters, tabs and panels, tweaks to how Plonix behaves, passive analyzers for a specific framework, helpers for a token format, finding templates. This document is the design for the extension system: what an extension is, how it's found, installed and loaded, and above all **what it is never allowed to do**.

**Status.** Four pieces are live today:

- **Rule packs** are installable from files, URLs and the store, with schema validation and SHA-256 pinning. See [detection-rules.md](detection-rules.md).
- **Filter packs** add named Traffic filters (`is:graphql`, `-is:trackers`) to search and to the window's **+ Filter** builder. See [filters.md](filters.md).
- The **Market** (`plonix market`, and the Market screen) lists skills, rule packs, filter packs, bundles and extensions from a signed index, verifying the signature and every checksum. See [market.md](market.md).
- **Code extensions, first slice: passive analyzers.** A WebAssembly module runs in a sandbox with no network, files, processes or clock, under a CPU, memory and time budget. It reads the traffic the engine hands it (in-scope only, unless you grant more), adds notes that show in the Lens under its own name, and proposes findings that stay open until you confirm them. Install one from a file, a folder or the signed Market; switch it on and off; a crash or a runaway loop stops it and switches it off with a message, and the engine carries on. See [Running extensions today](#running-extensions-today).

Tabs, panels and tweaks are designed below ([UI contributions](#ui-contributions-tabs-panels-tweaks-and-filters)) and not built yet. Neither are `read-scope`, `detection-rules` and `named-filters` from code, nor `scoped-requests`: an extension asking for any of them is listed in the Market but not installable yet.

## Contents

- [Running extensions today](#running-extensions-today)
- [Threat model](#threat-model)
- [The trust boundary](#the-trust-boundary)
- [Invariants](#invariants)
- [Extension kinds](#extension-kinds)
- [Manifest](#manifest)
- [Capabilities](#capabilities)
- [UI contributions: tabs, panels, tweaks and filters](#ui-contributions-tabs-panels-tweaks-and-filters)
- [Sandbox: WebAssembly](#sandbox-webassembly)
- [Lifecycle](#lifecycle)
- [Store and distribution](#store-and-distribution)
- [Versioning](#versioning)
- [Roadmap](#roadmap)

## Running extensions today

```sh
plonix extensions                              # installed ones: on, off, or stopped and why
plonix extensions add ./my-extension           # a folder with plonix-extension.json, or a .plonixext file
plonix extensions add ./x.plonixext --yes --grant read-out-of-scope
plonix market install security-headers         # from the signed Market: checksum and signature verified
plonix extensions show security-headers        # what it is allowed to do
plonix extensions allow subdomain-discovery     # give a sensitive yes skipped at install (--revoke takes it back)
plonix extensions run security-headers         # hand it everything captured so far
plonix extensions disable security-headers     # or enable; enable clears a "stopped" state
plonix extensions remove security-headers
plonix extensions pack ./my-extension          # for authors: one .plonixext file to publish
plonix extensions check ./my-extension         # for authors: validate without installing
```

In the window, the **Market** shows each extension's capabilities before it installs, with a separate tick box for each sensitive one. The boxes start ticked for Plonix's own extensions and unticked for anything from the community or added by you. A sensitive capability can be allowed or taken back later on the extension's page, with no reinstall. An installed extension's page has **Switch on / Switch off**, a run button that fits how it runs (**Check captured traffic**, **Read captured traffic**, **Find subdomains**, or an address box with **Probe**), and the reason when Plonix stopped it. Every Plonix item's page also has a **How to use it** section: where it shows up, the steps, and a screenshot. The Market's **Installed** tab lists it with its state.

Extensions also show up where they are used: **Find subdomains** on the Scope screen while a subdomain finder is on, and **Probe for hidden parameters** when you right-click an in-scope request in Traffic while a probe is on.

What happens once it is on:

- **New traffic reaches it on its own**, in small batches, after it is recorded. Captured traffic from before it was installed is analyzed when you click **Run on captured traffic** (`plonix extensions run`).
- **Notes** show in the Lens under **Spotted**, with a dashed outline, labelled with the extension's name and "not from Plonix".
- **Proposed findings** are stored as **open**, created by `extension:<name>`, with a line saying which extension proposed them. A proposal with the same title as one it made before is not added again.
- **Faults are contained.** Running out of its instruction budget, its memory cap or its time, trapping, or breaking the host API stops the call. Plonix switches the extension off right away, records why, and shows it in `plonix extensions` and on its Market page. The engine, the proxy and other extensions carry on. Turning it back on is your call.

An example lives in [`examples/extensions/security-headers`](../examples/extensions/security-headers): a small Rust analyzer that notes HTML pages missing common security headers and cookies without `Secure` or `HttpOnly`, and proposes one finding per host. Its packed form is `store/extensions/security-headers.plonixext`.

### Program extensions

A program extension has no code of its own. Its manifest has `"runtime": "program"` and names one program Plonix knows how to drive. Its package carries no files.

```json
{ "plonix_extension": 1, "name": "secret-sweep", "runtime": "program", "program": "trufflehog",
  "capabilities": ["read-traffic", "passive-analysis", "run-program"], "...": "..." }
```

- **Programs are a closed list** in `crates/plonix-core/src/program.rs`: a manifest names one by id, never a path, command or flags. Plonix looks for an installed one on `PATH` and in the usual install folders, since apps started from the Dock do not get the shell's `PATH`.
- **Running it needs `run-program`**, a sensitive capability you say yes to at install, or later on its page or with `plonix extensions allow <name>`. Until then, running it says exactly that.
- **Each program has a kind** that decides how Plonix drives it and which other capabilities the extension must ask for:
  - **`scan`** reads copies of captured requests and responses in a private temporary folder, deleted when the program finishes, and reads its output back onto the exchanges they came from. It needs `read-traffic` and `passive-analysis` (and may add `read-out-of-scope`). `trufflehog` is one; it stays local (`--no-verification --no-update`), and what it finds shows in the Lens under **Spotted**, labelled with the extension's name. New traffic is checked as it arrives; **Run on captured traffic** checks the rest, once per version.
  - **`enumerate`** takes a domain you have already accepted into scope and runs a subdomain tool over it. It needs `suggest-scope`. The tool reads public sources — Plonix sends nothing to the target — and every host it returns is added to **Scope as a suggestion**, with its evidence, for you to accept or reject. It never changes scope. `subfinder` is one (`brew install subfinder`), with another installed recon tool used as a fallback.
  - **`probe`** is built in: Plonix performs it itself, so you install nothing. It takes one in-scope endpoint and sends a bounded set of candidate inputs through [`Engine::send`](#the-trust-boundary) — the same scope-enforced, recorded path as replay — never letting an outside program send. It needs `scoped-requests` and `propose-findings`. `param-probe` is one: it tries common query-parameter names and proposes one unconfirmed finding for any that change the response.
- **A kind may ask only for the capabilities it uses.** A scanner cannot ask for `scoped-requests`; an enumerator cannot ask for `read-traffic`. The manifest parser enforces this.
- **When an installed program is missing**, nothing is switched off: the extension's Market page says how to install it, and `plonix extensions run` says so too.

The Market ships three: `secret-sweep` (scan), `subdomain-discovery` (enumerate), and `parameter-probe` (probe).

A code extension can also be a passive analyzer with no program, like the Market's `js-endpoints`: it reads captured JavaScript and pulls out the API paths and URLs the code references, so endpoints nothing has visited yet stand out in the Lens. It is a WebAssembly extension in [`examples/extensions/js-endpoints`](../examples/extensions/js-endpoints), built the same way as `security-headers`.

### Package format

A package is one JSON file, `<name>.plonixext`, holding the manifest and the module, so one SHA-256 pins both:

```json
{
  "plonix_extension_package": 1,
  "manifest": { "plonix_extension": 1, "name": "security-headers", "runtime": "wasm", "entry": "security_headers.wasm", "...": "..." },
  "files": { "security_headers.wasm": "<Base64>" }
}
```

`files` holds exactly the entry module. Packages are at most 6 MiB, modules at most 4 MiB. Installing checks everything short of running it: the manifest, that this version can run what it asks for, and every import and export of the module against the host API and the capabilities it asks for.

### Host API, version 1

This first slice uses plain WebAssembly modules (no component model yet) with a small, versioned ABI. Every host function lives in the import module `plonix:extension@1`, and a function is only linked when the capability that unlocks it was granted. Importing anything else, including any WASI function, is refused at install time.

| Import | Signature | Needs |
| --- | --- | --- |
| `log` | `(ptr: i32, len: i32)` | always; at most 50 lines of 300 characters per call |
| `note` | `(exchange_id: i64, tag_ptr, tag_len, text_ptr, text_len: i32) -> i32` | `passive-analysis` |
| `propose_finding` | `(severity: i32, title_ptr, title_len, desc_ptr, desc_len, ids_ptr, ids_count: i32) -> i32` | `propose-findings` |

The module exports its `memory`, `plonix_alloc(len: i32) -> i32` (a buffer for the engine to write into) and `plonix_analyze(ptr: i32, len: i32) -> i32` (0 for success). The engine writes a batch as JSON, `{"api": 1, "exchanges": [...]}`, where each exchange has `id`, `method`, `scheme`, `host`, `port`, `path`, `query`, `in_scope`, `request_headers`, `request_body`, `status`, `mime`, `response_headers` and `response_body` (bodies as text, cut at 64 KiB, with `*_truncated` flags).

Host functions return 0 when they accept, -1 for invalid input (outside memory, not UTF-8, too long, control or invisible characters), -2 when the per-call limit is reached (200 notes, 20 proposals) and -3 for an exchange id that was not in the batch. Severity is 0 info to 4 critical. Tags are up to 40 characters, notes 500, titles 120, descriptions 4000.

### Limits

Each call gets a fresh instance: 400 million units of fuel (roughly instructions), 32 MiB of memory, one memory and one table, and 5 seconds. A start function is not allowed. The runtime is [Wasmi](https://github.com/wasmi-labs/wasmi), a WebAssembly interpreter written in Rust: it meters fuel, caps memory per instance, and adds no native code generation to Plonix, which keeps the app small and the build simple. The wall-clock limit is enforced by running each call on its own thread and abandoning it when time is up; its fuel budget stops it soon after.

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
7. **Pinned bytes.** What runs is exactly what was verified at install time (SHA-256 against the signed Market index), and it's re-verified on load.

## Extension kinds

| Kind | Runtime | Can do | Status |
| --- | --- | --- | --- |
| Rule pack | none (data) | Detect technologies | **Shipped** |
| Filter pack | none (data) | Add named Traffic filters, used as `is:<id>` | **Shipped** |
| Tweak pack | none (data) | Change defaults from an allowlist: default filters, columns, shortcuts, accent colour | **Designed** |
| Declarative extension | none (data) | Bundle one or more rule packs under one name and version | Manifest implemented; install planned |
| Lens detector | none (data) | Flag a value in requests and responses: a regex, a label and a category (personal data, secret, decodable, info) | Built-in set shipped; packs planned |
| WASM extension | WebAssembly sandbox | Passive analysis, finding proposals, scoped requests, panels and tabs, per its capabilities | **Passive analyzers shipped**; the rest designed |

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
| `named-filters` | Contribute named Traffic filters, validated like any filter pack | at install |
| `ui-panels` | Show panels in existing screens (Traffic inspector, a Map host), drawn by Plonix from a view tree | at install |
| `ui-tab` | Add one sidebar tab of its own, drawn the same way | at install |
| `passive-analysis` | Return tags and notes for exchanges it was given | at install |
| `propose-findings` | Propose findings, stored as unconfirmed and attributed to the extension | at install |
| `scoped-requests` | Ask the engine to send requests. **Scope-enforced, rate-limited and recorded**, exactly like agent requests | explicit yes |
| `suggest-scope` | Contribute scope **suggestions** (never decisions): record a domain as a candidate, with evidence, for you to accept or reject | at install |
| `run-program` | For a [program extension](#program-extensions) only: run the program it names (a tool you installed, or a built-in operation) | explicit yes |

At install time the user sees the capabilities in plain words (`Capability::describe`). The **sensitive** ones (`read-out-of-scope`, `scoped-requests`, `run-program`) each need a separate, explicit yes. An update that asks for **new** capabilities isn't applied silently: it waits for the user to approve the difference.

## UI contributions: tabs, panels, tweaks and filters

People want to change the window as well as the engine: a tab for a workflow they repeat, a panel that explains a token, a filter for their target's noise, a different default layout. Each of these is a different kind of risk, so each gets the least powerful mechanism that can do the job.

| Want | Mechanism | Code? | Status |
| --- | --- | --- | --- |
| New filters | Filter pack | No | **Shipped** |
| Tweaks to defaults | Tweak pack | No | Designed |
| A panel in an existing screen | WASM extension with `ui-panels` | Yes, sandboxed | Designed (the sandbox ships; panels do not yet) |
| A new tab | WASM extension with `ui-tab` | Yes, sandboxed | Designed |

### Filters (shipped)

A filter pack is a list of named queries in the normal search language. Each shows up as `is:<id>` in search and the CLI, and as a one-click chip under **+ Filter** in Traffic, in either *Show only* or *Hide* mode. A filter can only narrow what you already see: its query is parsed by the same search engine as anything you type, it can't refer to another named filter, and it has no way to touch scope, send requests or run code. Packs install from a file, a URL or the store and are pinned by SHA-256, like rule packs. See [filters.md](filters.md).

### Tweaks (designed)

A tweak pack is data: a list of `setting = value` pairs from an **allowlist** of cosmetic and workflow settings (default Traffic filters, visible columns, row density, keyboard shortcuts, accent colour, the order of sidebar tabs). Each setting has a type and range, and Plonix shows the diff before applying it. Security settings are never on the allowlist and can't be tweaked by a pack: scope, the CA, upstream certificate checks, agent access, the API token, where data is stored. A tweak pack can be removed in one step and Plonix returns to the values it replaced.

### Panels and tabs (designed)

Panels and tabs need logic, so they come from WASM extensions, with two extra capabilities: `ui-panels` and `ui-tab`.

- **Plonix draws everything.** The extension never ships HTML, CSS or JavaScript and never gets a web view. It returns a **view tree** made of Plonix's own components (heading, text, key/value list, table, code block, request link, badge, button, form fields) as plain data. The window renders it with the same code as built-in screens. That rules out the web's usual attacks: no scripts, no remote images or fonts that could leak data, no fake login prompts styled like Plonix, no clickjacking.
- **Contribution points.** `ui-panels` can add a panel to the Traffic inspector (for the selected exchange) and to a host in the Map. `ui-tab` adds one tab to the sidebar. Each panel and tab is labelled with the extension's name, so it can't pass itself off as part of Plonix.
- **Data comes through capabilities.** A panel for an exchange receives that exchange only if the extension also holds `read-traffic`, and only if it's in scope (unless `read-out-of-scope` was granted). A tab sees what its capabilities allow, through the same host API as everything else.
- **Buttons call the host API, not the network.** A button in a panel can only trigger an action the extension's capabilities allow: propose a finding, filter Traffic, open an exchange, or, with `scoped-requests`, send a request that is scope-enforced and recorded like any agent request.
- **Bounded.** Rendering runs under the same fuel, memory and time limits as any extension call, and view trees are capped in size and depth. A slow or broken panel shows an error in its own box and never freezes the window.

```text
 extension (WASM) ──view tree (data)──▶ Plonix validates ──▶ Plonix components render it
        ▲                                     │
        └──── host API calls (capability-checked, scope-enforced) ◀── button clicks
```

## Sandbox: WebAssembly

**Recommendation: run extension code as WebAssembly in an embedded runtime, with no WASI file system, network or clock imports, and a host API that maps one-to-one to capabilities.**

What shipped first uses the Wasmi interpreter and plain modules with the small ABI in [Host API, version 1](#host-api-version-1). The WIT sketch below is where the API goes as it grows (panels, tabs, scoped requests); the capability rules are the same either way.

Why WebAssembly:

- **Deny by default.** A WASM module can't do anything it isn't given an import for. That's what we need: capabilities become the *only* imports. Compare dynamic libraries (`.dylib`), which run with the full rights of the Plonix process, and scripting runtimes, whose standard libraries have to be stripped and audited one function at a time.
- **Resource limits built in.** WebAssembly runtimes support fuel metering (instruction budgets) and memory limits per instance, and the call can be abandoned on a wall-clock timeout. A runaway extension is stopped, and the engine keeps running.
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
8. **Run**: each call gets a fresh, metered instance. Faults and limit violations are logged, and the first one switches the extension off, with the reason, until the user switches it back on.

`plonix extensions list | show | add | enable | disable | run | remove` manages installed extensions (`plonix ext` for short). Installed packages and their state live in `~/.plonix/extensions/` (`lock.json`, `packs/`, `state.json`).

## Store and distribution

The store is a static JSON index (`plonix_core::registry`). Extensions use the same index with `"kind": "extension"`:

- **Three shelves.** The Plonix Market (`store/index.json`, reviewed by the maintainers), the community Market (`community/index.json`, written by its authors and checked automatically) and what you add yourself from a GitHub release, a folder, a file or a link. Each package is badged Official, Community or Your own. See [market.md](market.md#three-shelves).
- **Reviewed by pull request.** Adding or updating a package in either Market is a pull request that changes its index, including the new SHA-256. Review approves specific bytes, and the checksum makes that approval stick.
- **Signed indexes.** Both indexes are signed with Ed25519 keys, so a mirror can serve them without being trusted. CI signs them when a pull request changes them ([How the Markets are signed](market.md#how-the-markets-are-signed)).
- **A block list.** The maintainers can pull a package from every shelf at once ([The block list](market.md#the-block-list)); an extension on it is switched off and cannot be switched back on.
- **Hosted anywhere.** Teams can host private indexes on any https server, or a folder, and point Plonix at them with `--index` or `$PLONIX_STORE_INDEX`.
- **No install scripts, ever.** Installing copies verified bytes. Nothing in a package runs at install time, and a GitHub repository's source never reaches the user's machine: only the release file does.

## Versioning

- Packages use `MAJOR.MINOR.PATCH`. `plonix store update` upgrades to newer versions.
- The **host API** is versioned in its WIT package name (`plonix:extension@1.0.0`). Plonix supports the current major version and links older minor versions. An extension built for a newer API than the engine has is refused with a clear message.
- File formats carry their own version (`plonix_pack`, `plonix_index`, `plonix_extension`). Readers refuse versions they don't know rather than guessing.
- An update that adds capabilities needs fresh consent (see [Lifecycle](#lifecycle)).

## Roadmap

1. ✅ Rule packs: format, validation, install from file or URL, SHA-256 pinning, built-in starter packs.
2. ✅ Store index and client for rule packs (`plonix store list|install|update`).
3. ✅ Extension manifest and closed capability list, validated and tested.
3a. ✅ Filter packs: named Traffic filters in search, the CLI and the window's filter builder, installable from the store.
4. Declarative extensions: install bundles of rule packs through the same flow.
   Lens detectors come next in the same format: today's built-in pattern detectors (`crates/plonix-core/src/insight.rs`) are already plain data, so a pack only needs a schema for them. Decoders that need code (JWT, Base64, hex) stay built in.
5. ✅ WASM runtime: a sandbox with fuel, time and memory limits; analyzers with `read-traffic`, `read-out-of-scope` and `passive-analysis`; installs from a file, a folder or the signed Market.
6. ✅ `propose-findings`. Next: `scoped-requests`, reusing the engine's scope enforcement and recording.
7. ✅ Signed store indexes, a community Market, adding from GitHub releases and folders, and a block list.
8. ✅ GUI: install with the same consent flow, switch on and off, run, and see why one was stopped, from the Market screen.
9. Tweak packs: allowlisted settings, with a diff before applying and one-step undo.
10. `ui-panels`, then `ui-tab`: view trees rendered with Plonix components.
