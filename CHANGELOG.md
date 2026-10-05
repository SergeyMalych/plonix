# Changelog

What changed in each Plonix release. The newest release is at the top.

## [Unreleased]

## [0.1.0] - 2026-10-05

The first public release of Plonix for Mac.

### Capture
- An intercepting HTTP and HTTPS proxy with its own certificate authority, created on first run.
- One step from nothing to captured traffic: open a target and Plonix starts the proxy, sets the scope and opens a browser with its own profile that trusts Plonix.
- HTTP/2 on both sides, WebSocket messages, and bodies that stream through as the server sends them.
- Intercept: hold requests and responses in flight to edit, forward or drop them. Match and replace rules change traffic as it passes.

### Investigate
- Traffic with a search language (`host:api.example.com method:POST status:5xx`), include and exclude filters, filter packs and suggestions drawn from your traffic.
- The Lens decodes what it finds in a request or response: tokens, URL-encoded and Base64 values, personal data, leaked secrets and internal addresses.
- Adaptive scope suggests related domains as you browse, with the evidence for each, to accept or reject one by one or in bulk.
- The Map shows hosts, the technologies behind them, and their endpoints and parameters.

### Experiment
- The Bench: edit a request, send it, branch it and compare responses side by side. Edit decoded values in place.
- Bench runs send a request many times with values from payload lists, including lists from the Market.
- Crawl a host to find endpoints, parameters and forms, and run scope-gated active scans from the Scans screen.

### Validate
- Findings with the requests that prove them, which you can edit, confirm, close and export as Markdown, HTML or JSON.

### AI and automation
- A read-only MCP server so Claude Code can see traffic, the map, scope and findings, and Ask Claude from any request, finding or host.
- Skills: playbooks agents follow for a job in Plonix.
- The `plonix` command and a token-protected local API for everything the window does.

### Projects and Market
- Several projects open at once, each in a folder you choose, with a Start screen and Settings.
- The Market: a signed catalog of skills, rule packs, filter packs, payload lists and bundles.
- Updates you approve: Plonix asks before it downloads or installs anything.
- A demo project to try every tool without a target.

[Unreleased]: https://github.com/SergeyMalych/plonix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/SergeyMalych/plonix/releases/tag/v0.1.0
