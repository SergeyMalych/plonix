# Privacy and usage statistics

Plonix keeps your work on your computer. Captured traffic, projects, findings, scope and settings are stored only in folders on your disk (`~/.plonix` and your project folders). Plonix never uploads them.

The one thing Plonix can send on its own is a small, anonymous usage report, if you allow it. This page lists exactly what that report holds.

## Your choice

- Statistics are **off** unless you turn them on. On first launch, Plonix shows its license and [terms of use](../TERMS.md) with two checkboxes: **I accept the license and terms** (required) and **Share anonymous usage statistics** (unticked; tick it if you want to help). The CLI asks the same two questions the first time it runs in a terminal, and "no" is the default.
- Change it any time: **Settings › Usage statistics** in the app, or `plonix usage on` / `plonix usage off`.
- **Settings › Usage statistics › Show What Is Sent** and `plonix usage` show the next report exactly as it would be sent.
- **Reset Install ID** (or `plonix usage reset`) deletes everything kept so far; the next count starts with a new random id.
- Setting `PLONIX_NO_ANALYTICS=1` or `DO_NOT_TRACK=1` in the environment turns it off whatever the setting says.
- Turning it off deletes the counts and the install id kept so far (`~/.plonix/usage.json`).

Accepting the terms for a single run (`--accept-terms` or `PLONIX_ACCEPT_TERMS=1`, meant for scripts and CI) records nothing, so a Plonix home used only that way never counts anything. Nothing is sent from CI (`CI` is set) or from development builds.

## What a report holds

At most one report a day, sent by HTTPS POST to `https://plonix.io/api/usage`:

```json
{
  "schema": 2,
  "install_id": "3f9c2a6e0b7d4e1f8a5c6b2d9e0f1a7c",
  "version": "0.2.0",
  "os": "macos",
  "os_version": "15.1",
  "arch": "aarch64",
  "counts": { "app_launched": 4, "project_opened": 3, "screen_bench": 9, "bench_send": 27, "scan_run": 1 },
  "minutes": { "traffic": 41, "bench": 18, "scope": 3 },
  "filters": { "host": 6, "-host": 2, "status": 3, "text": 4 },
  "profile": "bug-hunter",
  "look": { "style": "studio", "theme": "auto", "density": "dense" },
  "projects": "2-5",
  "sizes": {
    "requests": { "1k-10k": 1, "101-1k": 1 },
    "hosts": { "21-100": 1, "6-20": 1 },
    "in_scope": { "1": 1, "2-5": 1 }
  },
  "rejected": { "google-analytics.com": 2, "sentry.io": 1, "other": 2 }
}
```

| Field | What it is |
| --- | --- |
| `schema` | The version of this report's shape. |
| `install_id` | 32 random hex characters made the first time a count is saved. Not derived from your hardware, account or anything else. Deleted when you turn statistics off or reset it. |
| `version` | The Plonix version. |
| `os`, `os_version` | The operating system and its release, major and minor only, such as `macos` and `15.1`. |
| `arch` | The CPU type, such as `aarch64` or `x86_64`. |
| `counts` | How many times each feature below was used since the last report. |
| `minutes` | Active minutes per screen of the project window since the last report. A minute counts only while the window has focus and was used (a click, a key, scrolling) in the last two minutes, and at most one minute counts per minute however many windows are open. Screen names come from a fixed list: `traffic`, `bench`, `scope`, `map`, `users`, `access`, `callbacks`, `findings`, `agents`, `market`, `scans`, `programs`, `rules`, `settings`. |
| `filters` | How many Traffic searches used each kind of term: `host`, `method`, `status`, `path`, `mime`, `scope`, `source`, `ext`, `kind`, `is` or `text` (anything else), with `-` in front for a term that hides. Only the kind: `host:shop.example` counts as `host`, and what you typed is never kept. The same search refreshing counts once. |
| `profile` | The kind of work picked on first launch: `bug-hunter`, `red-teamer`, `researcher`, or `none`. |
| `look` | The window style, theme and spacing picked in Settings › Appearance. |
| `projects` | How many projects Plonix knows about, as a range. |
| `sizes` | For each project opened since the last report: how many requests it holds, how many distinct hosts it has seen, and how many of those are in scope, each as a range. The numbers are how many projects fell in each range. |
| `rejected` | For each project opened since the last report, the out-of-scope hosts that belong to a fixed list of well-known third-party services (the domains in Plonix's built-in exclusion groups, such as `google-analytics.com`, `doubleclick.net` or `sentry.io`, and its list of browser noise). Any other rejected host is counted as `other` and its name never leaves your computer. The numbers are how many projects rejected each one. |

Ranges are always one of `0`, `1`, `2-5`, `6-20`, `21-100`, `101-1k`, `1k-10k`, `10k-100k` or `100k+`. Exact numbers are never sent.

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
| `access_check` | An access check runs a request as each saved user |
| `scan_run`, `crawl_run` | A scan or a crawl runs |
| `program_applied` | A project starts following a bug bounty or disclosure program |
| `callbacks_start` | Callbacks (out-of-band checks) start listening |
| `finding_added`, `report_exported` | A finding is added, findings are exported |
| `market_install` | Something is installed from the Market |
| `ask_claude`, `agent_launch` | Ask Claude is used, Claude Code is opened from Plonix |
| `mcp_session` | An AI agent starts the Plonix MCP server |
| `screen_traffic`, `screen_bench`, `screen_scope`, `screen_map`, `screen_users`, `screen_access`, `screen_callbacks`, `screen_findings`, `screen_agents`, `screen_market`, `screen_scans`, `screen_programs`, `screen_rules`, `screen_settings` | A screen of the project window is opened |

## What is never sent

- URLs, hosts, domains, paths or IP addresses from captured traffic
- Requests, responses, headers, bodies or any other traffic
- Project names, folder or file paths, scope, findings, rules or settings
- Anything you type: searches, payloads, prompts, notes
- Your name, email, user name, computer name or hardware identifiers

## On the server

The endpoint is a small function on the Plonix website (`functions/api/usage.js` in this repository). It accepts only the fields above, keeps only names from the fixed lists above (any other feature, screen, kind, range or domain is dropped), caps every number, and stores one row per install per day: the date, the fields above, and nothing else. It does not store your IP address or request headers. A failed send is silent: Plonix never waits on it or retries more than once a day.

## The public totals

[plonix.io/analytics](https://plonix.io/analytics) shows totals over the last 30 days, built by `functions/api/stats.js`: downloads (from the GitHub release files), active and new installs, hours of use, which features and screens are used, kinds of work, versions, systems, project size ranges, the most rejected well-known domains, and the most used kinds of filter. No group of fewer than five installs is ever shown: it is folded into "other", or left out. Single reports and install ids are never published.
