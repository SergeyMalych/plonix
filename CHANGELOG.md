# Changelog

What changed in each Plonix release. The newest release is at the top.

## [Unreleased]

## [0.1.1] - 2026-10-05

### Added
- Suggestions for your work: pick bug hunter, red teamer or security researcher on the Start screen and get a starter set from the Market, each item with one line on why. The Market shows *Recommended for you* at the top, with a switch to look at another kind of work; Settings › Market and each project's settings keep the choice. CLI: `plonix market profile`, `plonix market recommend`, `plonix market install --starter`.
- Ask Claude shows its progress while it works: the current step, tokens read and written, elapsed time and the answer as it is written, formatted (headings, lists, tables, code blocks with Copy). A run that goes quiet says so, and is stopped with a clear message instead of hanging.
- Claude writes findings: one click fills in a plain-words title, severity with the reason for it, what happens, why it matters, steps to reproduce and the request as curl. Nothing is saved until you press Save.
- Copy any request as a curl command, from the Lens, the Traffic right-click menu and the Bench.
- The Lens suggests next steps for the open request: record a finding when something stands out (a leaked secret, an unsigned or expired token that still works, a card number, many people's emails, a stack trace, a server error), and ask Claude for ideas on the endpoint.
- The Lens spots stack traces in responses.
- API descriptions (OpenAPI and Swagger) found in traffic: the Map lists the endpoints nobody has visited yet, each one click from the Bench.
- When a Bench send comes back logged out but worked before, Plonix offers the newest login captured for that host, in that tab or in every tab still on the old one.
- Traffic offers to group look-alike requests (the same path with different ids) into one row each, expandable.
- secret-sweep in the Market: finds API keys, tokens and passwords for hundreds of services in captured traffic and shows each one in the Lens. It runs trufflehog, which you install yourself (`brew install trufflehog`), on your Mac only.
- js-endpoints in the Market: reads captured JavaScript and pulls out the API paths and URLs it references, so endpoints nothing has visited yet stand out in the Lens.
- subdomain-discovery in the Market: enumerates an accepted domain's subdomains with a recon tool you install (`subfinder`, with another installed tool as a fallback) and adds the ones it finds to Scope as suggestions to review. It reads public sources, never touches the target, and never changes scope on its own.
- parameter-probe in the Market: probes one in-scope endpoint for undocumented query parameters. Plonix sends a bounded set of candidate names itself, through the same scope-gated, recorded path as replay, and proposes one unconfirmed finding for any that change the response. Nothing is installed and no outside program sends.
- security-headers in the Market: a small passive extension that notes HTML pages missing common security headers and cookies set without Secure or HttpOnly, and proposes one finding per host. A starting point for writing your own.
- Program extensions now come in kinds — scan (over captured traffic), enumerate (subdomains into scope suggestions) and probe (candidate inputs through the scope choke point) — and a new `suggest-scope` capability lets one contribute scope suggestions without ever changing scope.

### Programs
- A Programs screen (⌘9) and `plonix program`: connect HackerOne, paste a program's policy, or look up a domain's security.txt, then review and follow the program.
- With a platform connected, Plonix pulls every program you can work on, with its assets and rules, and lets you search them all by program or by asset. It tells you when a followed program's scope changes.
- Following a program sets the project's scope from its assets: in-scope ones are accepted, listed exclusions rejected, and IP ranges work as scope rules.
- Its rules apply to every request Plonix sends: at most the program's request rate, the headers it asks for (on browser traffic too), and no scans, crawls or Bench runs when it bans automated testing.
- Bug bounty platforms are a new kind of Market package: declarative and signed, so more platforms can be added without a Plonix release.

## [0.1.0] - 2026-10-05

The first public release of Plonix for Mac.

### Capture
- An intercepting HTTP and HTTPS proxy with its own certificate authority, created on first run.
- One step from nothing to captured traffic: open a target and Plonix starts the proxy, sets the scope and opens a browser with its own profile that trusts Plonix.
- HTTP/2 on both sides, WebSocket messages, and bodies that stream through as the server sends them.
- Intercept: hold requests and responses in flight to edit, forward or drop them. Match and replace rules change traffic as it passes.
- Works on every Mac: with no supported browser installed, Plonix offers to download its own, and trusting the certificate for Firefox is one click.
- Client certificates per host for servers that ask for one, and HAR files in and out of a project.

### Investigate
- Traffic with a search language (`host:api.example.com method:POST status:5xx`), include and exclude filters, filter packs and suggestions drawn from your traffic.
- The Lens decodes what it finds in a request or response: tokens, URL-encoded and Base64 values, personal data, leaked secrets and internal addresses.
- Adaptive scope suggests related domains as you browse, with the evidence for each, to accept or reject one by one or in bulk.
- The Map shows hosts, the technologies behind them, and their endpoints and parameters.

### Experiment
- The Bench: edit a request, send it, branch it and compare responses side by side. Edit decoded values in place.
- Bench runs send a request many times with values from payload lists, including lists from the Market.
- Crawl a host to find endpoints, parameters and forms, and run scope-gated active scans from the Scans screen.
- Crawl JavaScript apps in a headless browser that stays within accepted scope.

### Validate
- Findings with the requests that prove them, which you can edit, confirm, close and export as Markdown, HTML or JSON.

### AI and automation
- A read-only MCP server so Claude Code can see traffic, the map, scope and findings, and Ask Claude from any request, finding or host.
- Skills: playbooks agents follow for a job in Plonix.
- Claude can suggest an edited request on the Bench, shown as a diff you apply or discard. Nothing is sent until you click Send.
- The `plonix` command and a token-protected local API for everything the window does.
- Install Command Line Tool… in the app puts the `plonix` command on your PATH.

### Projects and Market
- Several projects open at once, each in a folder you choose, with a Start screen and Settings.
- The Market: a signed catalog of skills, rule packs, filter packs, payload lists and bundles.
- Sandboxed WebAssembly extensions that analyze traffic and propose findings, without network, file or process access.
- Updates you approve: Plonix asks before it downloads or installs anything.
- A demo project to try every tool without a target.

### Trust and privacy
- You accept the license and terms once, on first launch.
- Anonymous usage statistics (counts of features used, never traffic, hosts or anything you type), which you can turn off on first launch, in Settings, or with `PLONIX_NO_ANALYTICS=1`. See docs/privacy.md.
- Crash reports stay on your Mac; Plonix offers to open a prefilled GitHub issue and never sends anything on its own.
- Documentation at plonix.io/docs.

[Unreleased]: https://github.com/SergeyMalych/plonix/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/SergeyMalych/plonix/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/SergeyMalych/plonix/releases/tag/v0.1.0
