# Privacy and usage statistics

Plonix keeps your work on your computer. Captured traffic, projects, findings, scope and settings are stored only in folders on your disk (`~/.plonix` and your project folders). Plonix never uploads them.

The one thing Plonix can send on its own is a small, anonymous usage report, if you allow it. This page lists exactly what that report holds.

## Your choice

- On first launch, Plonix shows its license and [terms of use](../TERMS.md) with two checkboxes: **I accept the license and terms** (required) and **Share anonymous usage statistics** (ticked, and yours to untick before you continue). The CLI asks the same two questions the first time it runs in a terminal.
- Change it any time: **Settings › Usage statistics** in the app, or `plonix usage on` / `plonix usage off`.
- Setting `PLONIX_NO_ANALYTICS=1` or `DO_NOT_TRACK=1` in the environment turns it off whatever the setting says.
- Turning it off deletes the counts and the install id kept so far (`~/.plonix/usage.json`).
- `plonix usage` prints the next report exactly as it would be sent.

Accepting the terms for a single run (`--accept-terms` or `PLONIX_ACCEPT_TERMS=1`, meant for scripts and CI) records nothing, so a Plonix home used only that way never counts anything. Nothing is sent from CI (`CI` is set) or from development builds.

## What a report holds

At most one report a day, sent by HTTPS POST to `https://plonix.io/api/usage`:

```json
{
  "schema": 1,
  "install_id": "3f9c2a6e0b7d4e1f8a5c6b2d9e0f1a7c",
  "version": "0.1.0",
  "os": "macos",
  "os_version": "15.1",
  "arch": "aarch64",
  "counts": { "app_launched": 4, "project_opened": 3, "screen_bench": 9, "bench_send": 27, "scan_run": 1 }
}
```

| Field | What it is |
| --- | --- |
| `schema` | The version of this report's shape. |
| `install_id` | 32 random hex characters made the first time a count is saved. Not derived from your hardware, account or anything else. Deleted when you turn statistics off. |
| `version` | The Plonix version. |
| `os`, `os_version` | The operating system and its release number, such as `macos` and `15.1`. |
| `arch` | The CPU type, such as `aarch64` or `x86_64`. |
| `counts` | How many times each feature below was used since the last report. |

The counted features are a fixed list:

| Name | Counted when |
| --- | --- |
| `app_launched` | Plonix.app starts |
| `web_launcher_opened` | `plonix launcher` opens the Start screen in a browser |
| `cli_used` | A `plonix` command runs |
| `project_opened`, `demo_opened` | A project, or the demo project, opens |
| `capture_started` | The capture browser opens |
| `intercept_used` | Intercept is turned on |
| `bench_send` | The Bench (or `plonix replay`) sends a request |
| `bench_run` | The Bench runs payloads through marked positions |
| `scan_run`, `crawl_run` | A scan or a crawl runs |
| `finding_added`, `report_exported` | A finding is added, findings are exported |
| `market_install` | Something is installed from the Market |
| `ask_claude`, `agent_launch` | Ask Claude is used, Claude Code is opened from Plonix |
| `mcp_session` | An AI agent starts the Plonix MCP server |
| `screen_traffic`, `screen_bench`, `screen_scope`, `screen_map`, `screen_findings`, `screen_agents`, `screen_market`, `screen_scans`, `screen_settings` | A screen of the project window is opened |

## What is never sent

- URLs, hosts, domains, paths or IP addresses from captured traffic
- Requests, responses, headers, bodies or any other traffic
- Project names, folder or file paths, scope, findings, rules or settings
- Anything you type: searches, payloads, prompts, notes
- Your name, email, user name, computer name or hardware identifiers

## On the server

The endpoint is a small function on the Plonix website (`functions/api/usage.js` in this repository). It accepts only the fields above, drops any feature name not in the list, and stores one row per install per day: the date, the fields above, and nothing else. It does not store your IP address or request headers. A failed send is silent: Plonix never waits on it or retries more than once a day.
