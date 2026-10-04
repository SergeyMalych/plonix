# Security policy

Plonix sits in the middle of your browser's traffic, holds a certificate authority your machine trusts, and lets AI agents read what it captured. A bug in Plonix can expose the people who use it, so we take reports seriously and are grateful for them.

## Reporting a vulnerability

**Please don't report security problems in public issues, discussions or pull requests.**

Report them privately through GitHub's private vulnerability reporting:

1. Go to [github.com/SergeyMalych/plonix/security/advisories/new](https://github.com/SergeyMalych/plonix/security/advisories/new) (or the repository's **Security** tab, then **Report a vulnerability**).
2. Describe the problem, the Plonix version or commit, your macOS version, and the steps to reproduce it. A proof of concept helps a lot.
3. Tell us how you'd like to be credited, if at all.

The report stays private between you and the maintainers. We discuss it, fix it and publish the advisory in the same place.

## What is in scope

Vulnerabilities in Plonix itself, in particular:

- **The engine.** The intercepting proxy and its HTTP/TLS handling, the traffic store and search, body decoding, and scope enforcement: any way for a request Plonix sends (a replay, a send, a scan or crawl request, an agent request) to reach a host that is not accepted into scope.
- **The local API.** The bearer token (`~/.plonix/api-token`) and its file permissions, the loopback-only binding and `Host` check, the one-time sign-in link for the window, and anything that lets a web page, another local user or a captured response drive or read the API.
- **The certificate authority.** How the CA key (`~/.plonix/ca.key`) is created, stored and protected, the per-host certificates minted from it, upstream certificate checks, and the CA's trust in the capture browser and the keychain.
- **The Plonix window and app.** Captured content rendered as anything but text, Content-Security-Policy bypasses, and ways for a target site to run script in the Plonix window or app.
- **MCP and agent access.** The agent token (`~/.plonix/agent-token`) and the engine's read-only enforcement: any way for an agent to send or replay requests, change scope, start scans, record findings or read beyond what its settings allow. Also what "Ask Claude Code" shares compared with what it shows you before sharing.
- **Packs, the store and the Market.** Validation of rule packs and scan packs, SHA-256 pinning and signature checks on install and load, and anything that lets a pack run code, reach the network, touch scope or slip past review.
- **Releases.** The signing and notarization of Plonix.app and the integrity of downloads and updates.

## What is out of scope

- Vulnerabilities in the sites you test with Plonix. Report those to their owners.
- Problems that need an attacker who already controls your user account or your machine.
- Exposing the proxy to your network when you chose to (for example a listen address of `0.0.0.0`), or disabling HTTPS decryption or certificate checks yourself.
- Denial of service that needs a local user to feed Plonix unreasonable input on purpose, unless it corrupts data or crosses a trust boundary.
- Reports from automated tools with no demonstrated impact.

If you are not sure whether something is in scope, report it anyway.

## Supported versions

Plonix is in early development and has not reached 1.0. Security fixes go into `main` and the next release. Only the latest release and `main` are supported; please check that a problem still exists there before reporting it.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| `main` | Yes |
| Older releases | No |

## What to expect

- **Acknowledgement** within 5 working days.
- **A first assessment** (whether we can reproduce it, how severe we think it is, and what happens next) within 14 days.
- **Updates** as the fix progresses, at least every two weeks.
- **A fix and an advisory.** We aim to release a fix within 90 days of the report, sooner for severe problems, and publish a GitHub security advisory crediting you unless you prefer otherwise.

Plonix is a small, volunteer-run project, so these are goals rather than guarantees. We ask that you give us a reasonable chance to fix a problem before you disclose it publicly, and we'll agree a disclosure date with you.

## Testing safely

Please test against your own installation and your own systems. Don't access other people's data, and don't run tests against systems you are not authorized to test.
