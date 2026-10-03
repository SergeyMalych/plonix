# Scanning

Scanning in Plonix turns captured traffic and a fingerprinted application
into discovered targets and reproducible findings. It is built for authorized
research on applications the user controls or is permitted to test: **every
request a scan sends goes through the engine's single scope choke point**
(`engine.send`), so a scan can only ever reach a host the user has explicitly
accepted into scope. Nothing fires on its own; a scan runs only when a person
starts it.

Scanning has three layers, each building on infrastructure that already
exists:

```
                 fingerprint (detect.rs tech + rules)
                              │
        ┌─────────────────────┴─────────────────────┐
        ▼                                            ▼
    Detectors  ── relevant? ──►  Tactics  ── run ──► Findings
   (passive,                   (active, in                (existing
    data-only)                  scope only)                Findings screen)
        ▲                           ▲
        │                           │
   Crawl / Crawl-with-browser   Signature packages
   (discover targets)           (store-installable, variants)
```

1. **Discovery** (crawl, crawl-with-browser) finds the endpoints, parameters
   and forms an application exposes.
2. **Relevance** (detectors) decides, from the fingerprint, which classes of
   checks make sense for this application.
3. **Checks** (tactics, shipped in signature packages) run only where their
   detector is active, and record findings.

## Detectors and tactics

The core idea, kept deliberately separate:

- A **detector** is passive. It reads only data Plonix already has — captured
  exchanges, detected technologies (`detect.rs`), discovered endpoints and
  parameters — and emits **signals**: typed, evidence-backed statements such
  as `jwt-present`, `php`, `graphql-endpoint`, `reflected-parameter`,
  `set-cookie-no-secure`. A detector never sends a request. It is the same
  kind of data-only, regex-driven matcher as a detection rule, so detectors
  are declarative and can ship in packs.
- A **tactic** is an active check. It is **gated by one or more detector
  signals** and runs only when all of them are present for a target. A tactic
  describes requests to send (through `engine.send`, so scope-enforced and
  recorded) and how to judge the responses into a finding.

This is what makes scanning *smart*: a JWT tactic is gated on `jwt-present`,
so it never runs against an application with no JWT; a PHP-oriented tactic is
gated on `php`, so it is skipped entirely on a Node.js site. The user never
waits on, and the target never receives, checks that cannot apply.

Signals carry evidence (which exchange, which header/parameter) exactly as
scope suggestions and Lens insights do, so the UI can always show *why* a
check was suggested.

## Scan types

### Crawl (no browser)

Discovers structure from what Plonix can see without rendering:

- seeds from captured in-scope traffic (links, forms, redirects, and the same
  `Location`/CSP/referrer evidence `scope.rs` already extracts);
- fetches in-scope pages through `engine.send` and parses links, forms (method,
  action, fields) and obvious parameters;
- stays within accepted scope, honors a depth/page budget, and dedupes by a
  normalized endpoint signature (path with parameter names, values stripped).

Output: a target set of endpoints and parameters, feeding the Map and the
detectors. Crawl is non-destructive — it only issues the requests a browser
following links would, bounded.

### Crawl with browser

The same, but driven through the existing headless browser (`browser.rs`) so
JavaScript-rendered links, SPA routes and XHR/fetch endpoints are reached.
Discovered requests still flow through the proxy and scope enforcement. This is
the slower, more complete discovery mode; the user chooses it when an app is
JS-heavy.

### Active scan

Runs tactics against discovered targets. For each target:

1. run detectors over the fingerprint + captured data → active signals;
2. select tactics whose gating signals are all present (intersected with the
   user's chosen profile);
3. execute each tactic's bounded request plan through `engine.send`;
4. evaluate responses into findings, recorded with evidence and the exact
   requests/responses so every finding is reproducible.

## Signature packages

Active-scan content lives in **signature packages**, an extension of the
existing data-driven pack model (`rulepack.rs` / `store.rs`): one JSON
document, strictly schema-checked, size-limited, SHA-256-pinned in the lock
file, installable and updatable from the store, community-authorable. A new
format tag `plonix_scanpack: 1` sits alongside `plonix_pack: 1`.

A scan pack contains:

- **detectors**: id, the signal it emits, and conditions (the same condition
  grammar as detection rules — header/cookie/param/path/body/tech regexes,
  plus a `tech:` condition that references a detected technology id);
- **tactics**: id, title, `requires` (the signal ids that gate it), a
  **severity**, an **intrusiveness** level, a **variant** label, a bounded
  **request template** (method, path/param transform relative to a discovered
  target, a small fixed set of payload tokens from the pack), and a
  **match** describing what in the response indicates the finding (status,
  header, body regex, timing, reflection) and what finding to record.

**Variants** let one logical check carry several request shapes (e.g.
encodings or phrasings) that update independently; the user or a profile picks
which variants to run. Packs are versioned, so the community or a scan type
can update signatures without a Plonix release.

### Beyond data: the bounded check interface

Most useful checks are expressible as template-plus-match data. For checks
that genuinely need logic (multi-step, response-derived next request), we reuse
the **extension seam** in `extension.rs`: a WebAssembly component in the
engine's sandbox with a closed capability list. A tactic extension gets
**`ScopedRequests`** (every request still goes through `engine.send`, so still
scope-enforced and recorded) and **`ProposeFindings`** (findings land
unconfirmed until a person confirms). It gets no socket, no filesystem, no
scope-changing capability — the sandbox cannot reach a host outside accepted
scope even if it tries. This is the existing, not-yet-built WASM runtime; data
tactics work today, logic tactics arrive with the sandbox.

## Smart suggestions

When the user opens Scans for a target, Plonix fingerprints it (existing tech
detection over the host's captured traffic) and runs detectors, then proposes a
**suggested scan profile**: the detectors that lit up and the tactics they
gate, ranked, with the irrelevant ones hidden (shown behind "show all" so the
user is never surprised by what's excluded). The user confirms or edits the
selection before anything runs. This is the "don't run PHP checks on a Node
site" behavior, made explicit and reviewable.

## AI-guided (optional)

Both choosing and running scans can be AI-assisted, through the existing
read-only agent layer:

- **Advising** is read-only and available now. New read-only MCP tools let an
  agent such as Claude Code read the fingerprint, the available
  detectors/tactics and a scan's status/findings, and *recommend* a profile.
  It cannot start a scan or send a request.
- **Acting** (an agent starting a scan) stays behind the same line as
  agent send/replay: it requires the user's explicit opt-in to active agent
  mode and is still scope-gated at `engine.send`. Off by default.

AI assistance is always optional and additive; scanning is fully usable
without it.

## UX: the Scans area

A new **Scans** screen (⌘7), with a simple left-to-right flow:

1. **Target** — pick from in-scope hosts/endpoints only (out-of-scope targets
   are not selectable). Shows the fingerprint for context.
2. **Mode** — Crawl, Crawl with browser, or Active scan.
3. **Profile** — for active scan: the suggested detectors/tactics from the
   fingerprint, each toggleable, grouped by detector, with intrusiveness and
   variant visible. An "AI suggest" affordance (optional) that fills the
   selection from the agent's recommendation for the user to review.
4. **Run** — explicit start. A live progress view: targets covered, requests
   sent (and that they stayed in scope), signals found, findings so far.
5. **Review** — findings flow into the existing **Findings** screen, each with
   its reproducing request/response, so there is one place for results.

## Guardrails

- **Scope.** Every scan request goes through `engine.send`; a target outside
  accepted scope is refused there. Discovery and active checks alike cannot
  reach an un-accepted host.
- **Explicit initiation.** Scans never auto-fire on capture. A person starts
  every scan.
- **Intrusiveness.** The foundation (crawl, crawl-with-browser, the
  detector/tactic framework, the pack model, suggestions, and safe/benign
  checks) is non-destructive. Any genuinely intrusive tactic is bounded,
  labeled with its intrusiveness, off by default, and must be turned on
  deliberately per scan.
- **Reproducibility.** Every finding records the exact requests and responses
  that produced it.
- **Auditability.** Every request a scan sends is recorded in traffic with its
  initiator, like any replay.

## Build order

1. Engine: `scan` module — Signal/Detector/Tactic types, the detector pass over
   fingerprint + captured data, the scan runner that drives discovery and
   gated tactics through `engine.send`, findings recording.
2. `scanpack` module + store integration: `plonix_scanpack: 1` format,
   validation, SHA-256 pinning, built-in starter packs (benign checks only),
   `plonix scan ...` CLI and `/api/scan*` routes.
3. Crawl, then crawl-with-browser.
4. Scans UI area + smart suggestions.
5. Read-only MCP tools for advising.
6. (Later, with the WASM sandbox) logic tactics via the bounded check
   interface.

## What is built

- The detector/tactic framework, the `plonix_scanpack: 1` pack model, and
  fingerprint-driven suggestions (`Catalog::suggest`).
- The active-scan runner (`Engine::scan`): it selects gated tactics, plans
  bounded requests, sends each one through `engine::send` (the scope choke
  point) so the scan can only reach accepted hosts, evaluates responses, and
  records findings against the existing Findings store. It never fires on its
  own and is bounded by a request budget.
- Benign built-in content: relevance detectors (`baseline`) and
  non-destructive checks (`probes`: exposed `.git/config` and `.env`,
  `server-status`, and a harmless reflected-parameter marker).
- API: `GET /api/scan/catalog`, `GET /api/scan/suggest/{host}` (both
  read-only, so the advising agent layer may call them), and `POST /api/scan`
  (user-only; agents cannot start scans).
- CLI: `plonix scan suggest|catalog|run`.

- Crawl (no browser): `Engine::crawl` seeds from the host's captured traffic
  and a start path, fetches in-scope pages through `engine::send`, extracts
  links and forms, follows same-host links within a page/depth budget, and
  never leaves accepted scope or submits a form. API `POST /api/crawl`
  (user-only); CLI `plonix crawl`.

Still to come: crawl-with-browser (needs a headless browser driver; the plain
crawl runs meanwhile and the report says so), the Scans UI area, and installed
scan-pack pinning in the store.
