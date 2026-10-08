# Projects, sessions and settings

## Projects

A project is a folder. Plonix creates new ones in `~/Plonix/<name>` unless you choose another place (on the Start screen, or `plonix projects new <name> --location <dir>`).

```text
<project folder>/
├── plonix-project.json   name, id and the project's own settings
├── traffic.db            captured traffic, scope rules, evidence and findings (SQLite)
├── browser/              the capture browser's profile for this project
├── .plonix.lock          held while the project is open
└── .plonix-open          present while the project is open
```

`$PLONIX_HOME/projects.json` (default `~/.plonix`) remembers which folders are projects, so they can be listed. The folder is the source of truth: copy or move it, then open it with **Add Existing** on the Start screen or `plonix start -p <folder>`.

Projects that an earlier Plonix kept as `~/.plonix/projects/<name>.db` move into a folder of their own (`~/.plonix/projects/<name>/`) the first time they are opened by name.

`traffic.db` records its schema version. When a newer Plonix opens a project, it upgrades the file in place, one step at a time, each step all or nothing, and keeps everything in it. An older Plonix refuses to open a project a newer one has upgraded, and says so, rather than risk misreading it: update Plonix to open it.

A local folder works best. Captured traffic changes constantly, which syncs poorly, so Plonix warns about folders that a cloud storage service syncs.

## The demo project

**Try the Demo** on the Start screen (or `plonix projects demo`) creates a ready-made project to explore Plonix with, in `plonix-demo` in the projects folder, and opens it. It holds traffic from Brightcart, a made-up online shop on reserved `.example` hosts, as a researcher would have it after a short session: Lens insights (a token, encoded values, a card number, a key in a script), scope suggestions with their evidence, common third parties to exclude, a filters overview (ready-made include and exclude views of the traffic, and the search language at a glance), findings, Bench experiments and what Scans would check. The traffic was written into the project's database, not captured, so opening the demo sends nothing anywhere, and its hosts do not exist.

The first time the demo opens, a short walkthrough offers to show you around: it steps through Traffic, filters, grouping, the Lens and its suggestions, Scope, the Map, the Bench and its runs, Findings, Scans, Rules and adding a rule, the Market, Programs and Agents, ringing each part on screen with a line or two on what it does. Use the arrow keys or Next and Back, and Esc to leave. **Take the tour** in the demo strip starts it again.

Most stops have a **Test now!** button that shows that part working on the demo's data, with a pointer that moves and clicks so you can see where everything is. It searches the traffic for one user's id, pulls out the sign-in flow with a single filter, switches between grouped and every request, decodes the bearer token in the Lens, opens Ask Claude on a request, accepts a suggested domain, lists the API endpoints nobody has visited yet, changes the order id in the Bench and gets another customer's order back, compares two sends side by side, runs through 22 order ids, sends a request through the rules, and checks Maya's order as each saved user and signed out. The demo's stand-in API answers all of it locally.

Although the demo's hosts are not real, its API answers locally, so you can actually try things. The Bench opens on a ready-made run: the order id is marked as a position with a range of ids queued, and pressing **Start run** walks the ids and shows how each one returns a different customer's order — the finding the demo is built around. This stand-in only ever answers the demo's made-up hosts, never a real one.

Change anything you like. **Start demo over…** in the project's **⋯** menu (or `plonix projects demo --fresh`) replaces it with a fresh copy. Only a project marked as the demo can be started over, and only while it is closed.

## Sessions

Each open project runs in a session with an engine of its own:

- its own **proxy**, on the project's port (8080 by default). When it is taken, the next free port is used (8081, 8082, …), so several projects run at once without any setup.
- its own **API** on loopback, on the port it used last time when it is free (so the window keeps its address), else 8090, 8091, …
- its own **database**, scope rules and findings, and its own capture-browser profile.

What all projects share: the certificate authority (trust it once) and the API token.

A session holds the project's lock while it is open, so the same project is never served by two sessions, even from two different processes. Each session announces itself in `$PLONIX_HOME/sessions/<project id>.json`; `engine.json` names the **current** session, the one opened last, which commands without `-p` talk to.

Where sessions run:

- **Plonix.app** runs the Start screen and every project opened from it inside the app. Each project gets a window; closing the window closes the session. Quitting the app closes them all.
- **`plonix start -p <project>`** runs a session in a background process. `plonix stop -p <project>` (or `--all`) closes it.
- **`plonix launcher`** runs the Start screen in a background process and opens it in your browser. Projects opened from it run in that process.

Projects started one way show up everywhere: the Start screen lists every running session and opens a window on it instead of starting a second one.

## Keep only in-scope traffic

Settings › Storage › **Keep only in-scope traffic**, per project. When the project closes cleanly, Plonix:

1. deletes every captured exchange whose host is not in scope: rejected hosts, pending suggestions and hosts never decided;
2. keeps the exchanges that findings point to, whatever their host;
3. rebuilds scope evidence from what remains, and compacts the database (search index included), so the deleted traffic is gone from the file and not only hidden.

Safety rules:

- If no host is in scope yet, nothing is deleted, and the project records why.
- If Plonix quits without closing the project (a crash, a power cut), the clean-up runs the next time the project opens, before any new traffic is recorded.
- The last result (when, how much was deleted and kept) is saved in `plonix-project.json` and shown in Settings.
- **Delete Out-of-Scope Traffic Now** in Settings › Storage does the same on demand, after a confirmation (`POST /api/storage/prune` with `{"confirm": true}`).

## Settings

Settings come in sections. A section is either **global** (one value for all projects, stored in `$PLONIX_HOME/settings.json`) or **per project** (stored in the project's `plonix-project.json`, so it travels with the folder).

| Section | Level | Fields |
| --- | --- | --- |
| Proxy | project | listen address and port, next-free-port fallback, decrypt HTTPS, hosts never decrypted, check server certificates, upstream proxy (`http://` or `socks5://`, with optional login), hosts reached directly, connect and request timeouts, how much of each body to keep (10 MB by default; longer bodies pass through in full and are marked as cut) |
| Intercept | project | hold in-scope hosts only or everything, a Traffic search that narrows what is held, hold responses too, forward unanswered items after (300 seconds by default). Whether Intercept is on is not saved: a project always opens with it off |
| Match and replace | project | apply the project's rules (on by default). The rules themselves are kept in `traffic.db` and managed on the Rules screen, with `plonix replace` or `/api/replace` |
| Client certificates | project | present client certificates (on by default). The certificates themselves are kept in `traffic.db` and managed under this section or with `plonix certs` (see [Client certificates](#client-certificates)) |
| Storage | project | keep only in-scope traffic |
| Interface | global | open projects in a Plonix window or the web browser |
| Appearance | this Mac | theme (match the system, light or dark) and spacing: Dense (the default) fits more on screen, Roomy gives rows and panels more air. Kept by the window itself, not the engine, and applied right away |
| AI agents | global | let agents read projects or not, in-scope hosts only or everything, which kinds of data, and the Ask Claude size limits. Stored in `agents.json` through a section storage hook (`Section::stored_by`) |

Proxy changes apply to a running session right away: the listener moves to the new address (connections already open keep working), and the upstream client is rebuilt. An address that cannot be bound is refused and nothing is saved. `plonix start --port` and `--insecure-upstream` override the settings for one session without changing them.

## HAR files

HAR is the standard format browsers' developer tools use to save network traffic. Plonix reads and writes HAR 1.2.

**Export.** **HAR ▾** in the Traffic toolbar offers:

- **Export all traffic** — every exchange in the project;
- **Export the filtered view** — what the current search shows;
- **Export picked rows** — rows you pick with ⌘-click (Ctrl-click), or Shift-click for a range. Escape or **Clear picked rows** unpicks them.

In the Plonix app the file is saved through a native dialog (also under File › Export Traffic as HAR…); in a web browser it downloads. Each entry carries the request and response headers, cookies, query string, form parameters, the decoded body (base64 when it is not text, with the original encoding noted), the time taken, the protocol, the server address, WebSocket messages (`_webSocketMessages`) and, when one was presented, the client certificate (`_clientCertificate`). Requests that got no response are exported with status 0 and the error in `_error`.

**Import.** **Import HAR file…** (or File › Import HAR… in the app) adds the file's requests to the project:

- they are marked **HAR** in Traffic and the Lens, and `source:import` finds them;
- they go through scope suggestions and detection like captured traffic, so hosts they reveal are suggested and technologies are recognised;
- an entry the project already has (same time, method, URL and status) is skipped, so importing the same file twice adds nothing;
- entries that are not HTTP (such as `chrome-extension:` or `data:` URLs) are skipped and counted; bodies longer than the project's body limit are cut, as in capture.

**From the command line:**

```bash
plonix har export -o all.har                              # everything
plonix har export host:example.com status:5xx -o errors.har  # a Traffic search
plonix har export --ids 12,14,20 -o picked.har            # chosen exchanges
plonix har export -o - | gzip > all.har.gz                # standard output
plonix har import capture.har
```

Files are written and read as a stream, so large files do not need to fit in memory. The API equivalents are `GET /api/har?q=…` or `?ids=…` (a streamed download) and `POST /api/har/import` with the file as the body. Both are for you only: agents cannot export or import.

## Client certificates

Some servers ask the client for a certificate during the TLS handshake (mutual TLS). Plonix presents one when you have added it for that host.

- **For a host or a domain.** `api.example.com` matches that host only; `*.example.com` matches `example.com` and every subdomain. An exact host wins over a wildcard, and a longer wildcard over a shorter one.
- **PEM or PKCS#12.** Add a PEM certificate (the chain, leaf first or in any order) with an unencrypted PEM key, in one file or two, or a `.p12` / `.pfx` file with its password. PKCS#12 files are opened with the `openssl` command, which must be installed (set `PLONIX_OPENSSL` to use another one).
- **Everywhere upstream.** The certificate is used for every connection Plonix makes to that host: proxied browser traffic, the Bench, payload runs, scans and crawls. Hosts listed under Settings › Proxy › hosts never decrypted pass through untouched, so the browser's own certificate is used there instead.
- **Visible where it matters.** The Lens, the Bench response and `plonix show` mark an exchange that presented a certificate (**cert · CN=…**). If a server asks for a certificate you haven't added, or refuses the one Plonix presented, the error says which.
- **Kept private.** Certificates and keys are stored in the project's `traffic.db`. A key is never shown again once added, never written to logs, and never available to agents or MCP tools. Removing a certificate deletes its key.

Settings › Client certificates lists the certificates (host, subject, expiry, chain length, note) and adds or removes them; its switch stops presenting any of them without deleting them. From the command line:

```bash
plonix certs add api.example.com --cert client.pem --key client-key.pem --note staging
plonix certs add '*.example.com' --cert bundle.pem          # key in the same file
plonix certs add api.example.com --p12 client.p12           # asks for the password
plonix certs add api.example.com --p12 client.p12 --password-env P12_PASSWORD
plonix certs list
plonix certs remove 2
```

The API is `GET /api/client-certs`, `POST /api/client-certs` and `DELETE /api/client-certs/{id}`, for you only.

### Adding a settings section

A feature describes its section once; the Settings screen in the project window and the Start screen draw it, check values and store them. No UI code is needed.

```rust
use plonix_core::settings::{self, Field, Level, Section};

settings::register(
    Section::new("my-feature", "My feature", Level::Global)
        .describe("What this section controls.")
        .order(40)
        .field(Field::toggle("enabled", "Turn it on", false).help("One line on what this does."))
        .field(Field::choice("mode", "Mode", "fast", &[("fast", "Fast"), ("thorough", "Thorough")]))
        .field(Field::number("limit", "Limit", 10, 1, 100).unit("requests"))
        .validator(|values| vec![]), // optional cross-field checks
);

// Reading it back:
let values = settings::global(&home, "my-feature");
```

Field types: `text`, `secret` (shown masked), `number` (with limits and a unit), `toggle`, `choice`, and `list` (one entry per line). `group` puts fields under a heading. Values of the wrong type in a file fall back to the default, and sections Plonix does not know are kept as they are.

A global section can keep its values in a file of its own with `.stored_by(load, save)`; the AI agents section does this to keep using `agents.json`.

## The Start screen API

The Start screen is served on its own loopback address (8070 by default; `$PLONIX_HOME/hub.json` names it while it runs) with the same token and one-time-link sign-in as a project window.

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/api/projects` | Known projects, with folder, size and the session serving each one |
| POST | `/api/projects` | Create a project (`{"name": "...", "location": "/parent/folder"}`) |
| POST | `/api/projects/add` | Add an existing project folder (`{"path": "..."}`) |
| POST | `/api/projects/demo` | The demo project, created on first use; `{"fresh": true}` replaces it with a new copy |
| POST | `/api/projects/{id}/open` | Open it (or find its session) and return a one-time link for its window |
| POST | `/api/projects/{id}/close` | Close its session |
| POST | `/api/projects/{id}/forget` | Remove it from the list; the folder is kept |
| GET | `/api/projects/{id}/settings` | Its settings sections and values |
| PUT | `/api/projects/{id}/settings/{section}` | Save a section; `general` renames the project |
| GET / PUT | `/api/settings` · `/api/settings/{section}` | Global settings |
| POST | `/api/pick-folder` | Ask for a folder with the system's own dialog |
