# Plonix

**The open-source web security workbench for macOS. Fast, native, and scriptable from day one.**

Plonix captures everything your browser does, learns the real shape of the target as you explore it, and lets you search, replay and prove what you find, from a GUI, a terminal, or an AI agent.

![The Plonix window: live traffic with an adaptive-scope suggestion and the request inspector](docs/images/plonix-window.png)

> **Status: early development.** The core engine (proxy, traffic store, search, adaptive scope, local API), the `plonix` CLI and the Plonix window (`plonix ui`) work today and are covered by tests. The native Mac app and the MCP server are next. See [Roadmap](#roadmap).

---

## Why Plonix

Most of a web assessment is the same loop: capture traffic, figure out what the app actually is, poke at the interesting parts, and write up what's real. The tools for that loop have grown heavy. Plonix keeps the loop and drops the weight.

| You want | Plonix gives you |
| --- | --- |
| A tool you can afford on every machine | Free and open source. No Pro tier holding back the good parts. |
| Large projects that stay responsive | A Rust engine with one SQLite database per project. Search over captured traffic stays quick as projects grow. |
| Something you can see and click | The Plonix window: live traffic, a repeater with branching and side-by-side compare, adaptive scope, a site map and findings. A native macOS app is planned on the same engine. |
| Scope that matches reality | Adaptive scope learns related domains as you browse and shows the evidence for each one. |
| Filters you can type | A small query language: `host:api.acme.com method:POST status:5xx -logout`. |
| Automation in any language | Everything goes through a local HTTP API. Use curl, Python, Go, or whatever you already script in. |
| An AI teammate that can see your project | Built-in MCP (planned) so agents such as Claude Code work with live traffic, scope and findings. |

## Principles

- **Fast.** A native Rust core. Projects with lots of traffic stay quick to search.
- **Native.** Built for macOS, not ported to it.
- **Minimal by design.** A few workflows done well: capture, discover, investigate, experiment, validate. No feature you'll never open.
- **User-friendly first.** From download to captured traffic in under a minute is a release requirement.
- **Programmable everywhere.** GUI, CLI and MCP are equal clients of the same local API. Anything you can click, you can script.
- **AI-native.** Agents get first-class, scoped access to the research environment, and they follow the same scope rules you do.

## What works today

### Intercepting proxy with a local CA
- HTTP and HTTPS interception. Plonix creates its own certificate authority on first run and mints per-host certificates on the fly.
- Trust the CA once (`~/.plonix/ca.pem`) and every HTTPS site you visit through the proxy is captured.
- Compressed bodies (gzip, deflate, brotli) are decoded for display and search.

### Full traffic capture
- Every request and response is recorded into a per-project SQLite database, in scope or not, so nothing you browsed is lost.
- Hosts and the endpoints seen on each host are listed for a quick map of the target.

### Fast search with filters
Field filters narrow by metadata, and anything else is full-text matched (case-insensitive) against URLs, headers and decoded bodies. Prefix any term with `-` to negate it.

```text
host:example.com          host or any subdomain (globs: host:*.cdn.*)
method:POST               HTTP method
status:404  status:5xx    exact code, class, or status:none for no response
path:/api                 path prefix (globs: path:*admin*)
mime:json                 substring of the response content type
scope:in  scope:out       in or out of the current scope
source:proxy              captured traffic, or source:replay for sent requests
"set-cookie: sid"         quoted phrase
passw -logout             plain full-text terms
```

### Adaptive scope
Static whitelists assume you already know every domain an app uses. You don't. You find out by using it. Plonix records everything, then suggests domains to bring into scope, each backed by evidence:

| Evidence | Example |
| --- | --- |
| Called from an in-scope page | `Referer` or `Origin` points at an in-scope host |
| Redirect | An in-scope host redirects here (`Location:`) |
| Linked | Referenced in an in-scope page or its `Content-Security-Policy` |
| Shares a session | Receives a session token that an in-scope host issued |
| Shares a certificate | Appears in the TLS certificate SANs of an in-scope host, or the reverse |

You accept or reject each suggestion (`*.example.com` covers all subdomains). Accepting a domain re-analyzes past traffic, so new suggestions surface right away.

**Enforcement is built in.** Every active request, whether a replay, a crafted request, or one from an agent, passes a single choke point. If the target host isn't accepted, it is refused. Passive capture keeps recording everything.

### Replay and send
- Replay any captured exchange with a different method, path, headers or body.
- Send new requests from scratch. Results are stored alongside captured traffic and tagged with who sent them.

### Technology detection, maintained by the community
- Plonix recognises what runs behind every host (servers, frameworks, CMSs, CDNs and WAFs, identity providers, exposed admin consoles) and shows the evidence for each detection, down to the exchange.
- Detection is driven by declarative **rule packs** that anyone can write and share. Install them from a file, a URL or the **store**, a JSON index hosted anywhere. New rules apply to traffic you already captured.
- Packs are untrusted data: strictly validated, linear-time patterns only, pinned by SHA-256 and re-verified on load. They can't run code, reach the network or touch scope. See [docs/detection-rules.md](docs/detection-rules.md) and the extension design in [docs/extensions.md](docs/extensions.md).

### The Plonix window
- `plonix ui` (and `plonix open`) opens a fast, keyboard-friendly UI in your browser, served by the engine itself. No install, no build step, light and dark.
- **Traffic:** a live-updating request list with the search language, filter chips and an inline request/response viewer that decodes gzip/brotli and pretty-prints JSON. A banner surfaces each new domain adaptive scope suggests, with its evidence and one-click accept or reject.
- **Repeater:** edit any request and send it, keep a history per tab, restore or branch any earlier send into a new tab, and compare two sends side by side (response or request diff). Sends go through the engine's scope enforcement: out-of-scope hosts are refused, and you can accept the host right there.
- **Scope:** every suggested domain with its evidence, accept (with or without subdomains) or reject, and the rule list.
- **Map:** hosts with their scope state, detected technologies with the evidence behind them, and endpoints with statuses and parameters.
- **Findings:** record a finding from any request, with the requests that prove it linked as evidence.
- The page signs in through a one-time link that `plonix ui` creates, so the API token never appears in a URL. It is locked down with a strict Content-Security-Policy, and captured content is only ever rendered as text.

### Local API
- An HTTP API on loopback only, protected by a bearer token stored at `~/.plonix/api-token` (mode `0600`).
- Requests must target the loopback address, which keeps web pages in your browser from reaching it.

## Quickstart

Requirements: Rust (stable, edition 2024) and a C toolchain. On macOS, `xcode-select --install` is enough. SQLite is bundled.

```sh
git clone https://github.com/SergeyMalych/plonix.git
cd plonix
cargo install --path crates/plonix-cli    # installs the `plonix` command
```

### The first 60 seconds

```sh
plonix open example.com
```

That one command creates your local certificate authority (first run only), starts the engine in the background, puts `example.com` and its subdomains in scope, opens a browser at the target with capture running, and opens the Plonix window next to it:

```text
Plonix · https://example.com/

  ✓ Certificate  ~/.plonix/ca.pem  (created)
  ✓ Proxy        127.0.0.1:8080  (started, project example.com)
  ✓ Scope        example.com (+ subdomains)  (more domains are suggested as you browse)
  ✓ Browser      Google Chrome (isolated profile, trusts Plonix)
  ✓ Window       http://127.0.0.1:8090  (opened in your default browser)

Capturing. Browse the site; requests appear below. Ctrl-C stops watching, capture keeps running.
```

The browser is Chrome, Brave, Edge or Chromium with its own isolated profile. It routes through the proxy and trusts the Plonix certificate on its own, so HTTPS works with nothing to install. Firefox is used when no Chromium-based browser is found. Set `PLONIX_BROWSER` to pick a specific browser.

To capture HTTPS from other apps too (Safari, curl, your everyday browser), trust the certificate once with `plonix ca trust`. It is added to your login keychain, and macOS asks you to confirm.

Browse the site and watch requests arrive in the Plonix window. Closed it? `plonix ui` opens it again (and starts the engine if it is not running). `plonix ui --no-open` prints the one-time link instead, and `plonix open --no-ui` skips the window.

### Working from the terminal

```sh
plonix status                              # is it running, what has it captured
plonix search host:example.com status:5xx  # newest first; filters below
plonix show 42                             # one request and its response
plonix watch scope:in                      # print new traffic as it arrives
plonix hosts                               # every host seen, busiest first
plonix tech                                # technologies detected on each host, with evidence

plonix scope                               # rules, plus suggested domains with evidence
plonix scope review                        # decide on suggestions one by one
plonix scope accept '*.example-cdn.com'    # or: reject, remove

plonix replay 42 -H 'Authorization: Bearer other-user' -t '/api/users/2'

plonix rules                               # detection rule packs in effect
plonix rules add ./my-pack.json            # or an https:// URL, optionally --sha256
plonix store                               # browse community packs
plonix store install admin-panels          # verified against the store's sha256
plonix stop                                # captured traffic is kept
```

Replays only go to accepted hosts. Anything else is refused with exit code 4 and a hint to accept the host first. Every command takes `--json` for scripting. Exit codes are `0` ok, `1` error, `2` bad usage or query, `3` engine not running, `4` refused by scope, `5` not found.

`plonix start` runs the engine without opening a browser (`--project`, `--port`, `--insecure-upstream` for self-signed staging hosts). Data lives in `~/.plonix`, or `$PLONIX_HOME`.

### Using the local API directly

Everything the CLI does goes through the local API, so any language can drive Plonix:

```sh
TOKEN=$(cat ~/.plonix/api-token)
API=http://127.0.0.1:8090/api

curl -H "Authorization: Bearer $TOKEN" "$API/status"
curl -H "Authorization: Bearer $TOKEN" -G "$API/traffic" --data-urlencode 'q=host:example.com status:2xx'
curl -H "Authorization: Bearer $TOKEN" "$API/scope"     # rules plus suggestions with evidence

curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"domain":"*.example.com"}' "$API/scope/accept"
```

The API listens on port 8090 when it is free; `plonix status` shows the actual address.

### API at a glance

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/` | The Plonix window (static page; signs in with a one-time link) |
| POST | `/api/ui/launch` | Create a one-time link that opens the window signed in |
| GET | `/api/status` | Engine, project and CA info, counts |
| GET | `/api/traffic?q=&limit=&offset=` | Search captured traffic |
| GET | `/api/traffic/{id}` | One exchange, with decoded bodies |
| GET | `/api/hosts` | Hosts seen |
| GET | `/api/hosts/{host}/endpoints` | Endpoints seen on a host |
| GET | `/api/tech` · `/api/tech/{host}` | Detected technologies per host, with evidence |
| GET | `/api/rules` | Detection rule packs in effect |
| GET | `/api/scope` | Scope rules and pending suggestions |
| POST | `/api/scope/accept` · `reject` · `remove` | Decide on a domain |
| POST | `/api/send` | Send a new request (in-scope hosts only) |
| POST | `/api/replay` | Replay a captured exchange, optionally modified |
| GET / POST | `/api/findings` | List or record findings |
| POST | `/api/shutdown` | Stop the engine |

## Architecture

```text
        GUI        CLI        MCP          equal clients
          \         |         /
           └──── Local API ──┘             loopback + token
                    │
               Core Engine                 Rust, headless
        ┌───────────┼───────────┐
     Traffic    Discovery    Findings
     (proxy,    (adaptive     (validated,
      store,     scope, tech   reproducible)
      search)    detection)
                    ▲
          rule packs · store       untrusted, declarative, verified
```

The engine is a headless background process. Every front end talks to it the same way, so the Mac app, your shell scripts and your AI agent always see the same project.

```text
crates/
├── plonix-core   engine: proxy, CA, store, search, scope, detection, rule packs, store index, local API
│   └── ui/       the Plonix window: plain HTML, CSS and JavaScript embedded in the binary
└── plonix-cli    the `plonix` command: engine control, onboarding, search, scope, replay, rules, store
store/            community store: index.json and rule packs
docs/             detection rules, extension design
```

## Roadmap

**Built**
- [x] Intercepting HTTP/HTTPS proxy with local CA
- [x] Traffic capture into SQLite, decoded bodies
- [x] Search language with field filters
- [x] Adaptive scope v1: suggestions with evidence, accept/reject, enforcement
- [x] Replay and send
- [x] Token-authenticated, loopback-only local API
- [x] `plonix` CLI: search, inspect, watch, replay and manage scope from the terminal
- [x] `plonix open <target>`: one command from nothing to captured traffic, in a pre-configured browser
- [x] Technology detection from community rule packs, with a store (`plonix tech`, `plonix rules`, `plonix store`)
- [x] The Plonix window (`plonix ui`): live traffic, repeater with branch and compare, adaptive scope review, map with technologies, findings

**Coming**
- [ ] Built-in MCP server and `plonix connect claude`, so Claude Code and other agents can work with live traffic, scope and findings, always inside accepted scope
- [ ] Native macOS app on the same engine, adding an Agents screen once MCP lands
- [ ] Sandboxed WebAssembly extensions with a closed capability list that can never bypass scope ([design](docs/extensions.md))

Deliberately out of scope: an automated scanner and a token sequencer. Plonix stays small on purpose.

## Contributing

Plonix is early, and this is a good time to shape it. Issues and discussions about workflows, pain points and design are as valuable as code.

1. Open an issue describing the problem or idea before large changes.
2. Keep pull requests focused, and include tests for engine behavior.
3. Run `cargo fmt`, `cargo clippy --workspace` and `cargo test --workspace` before pushing.

The easiest way to contribute is a **detection rule pack**: no Rust needed. Write one, check it with `plonix rules check`, and open a pull request that adds it to `store/`. See [docs/detection-rules.md](docs/detection-rules.md#contributing-a-pack).

Use Plonix only against systems you are authorized to test.

## License

To be announced.
