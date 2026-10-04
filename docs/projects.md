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
| Proxy | project | listen address and port, next-free-port fallback, decrypt HTTPS, hosts never decrypted, check server certificates, upstream proxy (`http://` or `socks5://`, with optional login), hosts reached directly, connect and request timeouts |
| Storage | project | keep only in-scope traffic |
| Interface | global | open projects in a Plonix window or the web browser; on launch, show the Start screen or reopen the last project |

Proxy changes apply to a running session right away: the listener moves to the new address (connections already open keep working), and the upstream client is rebuilt. An address that cannot be bound is refused and nothing is saved. `plonix start --port` and `--insecure-upstream` override the settings for one session without changing them.

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

## The Start screen API

The Start screen is served on its own loopback address (8070 by default; `$PLONIX_HOME/hub.json` names it while it runs) with the same token and one-time-link sign-in as a project window.

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/api/projects` | Known projects, with folder, size and the session serving each one |
| POST | `/api/projects` | Create a project (`{"name": "...", "location": "/parent/folder"}`) |
| POST | `/api/projects/add` | Add an existing project folder (`{"path": "..."}`) |
| POST | `/api/projects/{id}/open` | Open it (or find its session) and return a one-time link for its window |
| POST | `/api/projects/{id}/close` | Close its session |
| POST | `/api/projects/{id}/forget` | Remove it from the list; the folder is kept |
| GET | `/api/projects/{id}/settings` | Its settings sections and values |
| PUT | `/api/projects/{id}/settings/{section}` | Save a section; `general` renames the project |
| GET / PUT | `/api/settings` · `/api/settings/{section}` | Global settings |
| POST | `/api/pick-folder` | Ask for a folder with the system's own dialog |
