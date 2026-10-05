# The Market and skills

The Market is where Plonix gets everything modular, in one catalog:

| Kind | What it is | Installs into |
| --- | --- | --- |
| **Skill** | A playbook an AI agent follows for one job in Plonix | `$PLONIX_HOME/skills` |
| **Rule pack** | Technology detection rules ([format](detection-rules.md)) | `$PLONIX_HOME/rules` |
| **Filter pack** | Named Traffic filters such as `is:auth` ([format](filters.md)) | `$PLONIX_HOME/filters` |
| **List pack** | Named payload lists for the Bench ([format](bench.md)) | `$PLONIX_HOME/lists` |
| **Bundle** | A set of other packages, installed and removed together | `$PLONIX_HOME/market/bundles.json` |
| **Extension** | Code that runs in the Plonix sandbox: today, passive analyzers ([extensions.md](extensions.md)) | `$PLONIX_HOME/extensions` |

Open it from the sidebar (⌘7 in the app), or use the CLI:

```sh
plonix market                         # everything, with what is installed
plonix market --kind skill            # one kind
plonix market show api-kit            # what a package is and what it includes
plonix market install api-kit         # installs the bundle and everything in it
plonix market update                  # newer versions of what you installed
plonix market remove api-kit          # removes the bundle and what it added
```

## Suggestions for your work

Pick the kind of work you do and the Market suggests a short starter set, with one line on why each item is there:

- **Bug hunter**: public programs for rewards. Access checks, leaked secrets, APIs and clear write-ups. Items that send many requests are suggested but never pre-picked.
- **Red teamer**: an engagement against one organization. Recon, things left open, identities and cloud services. Only items that read captured traffic are pre-picked.
- **Security researcher**: one app, its cloud setup or a protocol, taken apart. Analysis across traffic, cloud services and decoding.

You choose on the Start screen the first time (one tap; skills, packs and bundles install right away, extensions wait in the Market so you see what each may do first), at the top of the Market under *Recommended for you*, or in Settings › Market › Your work. A project can use a different one in its own settings. Changing it never removes anything. It stays on your computer and is not part of usage statistics.

```sh
plonix market profile researcher      # set it (none clears it)
plonix market recommend               # the starter set, what you have, and more
plonix market recommend --profile red-teamer   # look at another without changing yours
plonix market install --starter       # install the starter set (extensions are listed, not installed)
```

Profiles never name items. Each one weighs a fixed list of tags, and each item carries a few tags and a noise level (`passive`, `light` or `active`) in [`store/profiles.json`](../store/profiles.json). An item's score is the sum of the profile's weights for its tags; items scoring 4 or more are suggested, best first. A bundle replaces the items inside it unless one of them scores higher on its own, and items noisier than the profile allows are listed under *Also for you* without being picked. To make a new Market item show up for the right people, add its tags there; `cargo test` fails if a Market item has no entry.

## Validated packages

Nothing is installed unless it can be traced to a signature you trust:

1. The catalog is an `index.json` with a signature file next to it, `index.json.sig`. Plonix checks the Ed25519 signature against the keys it trusts: the Plonix maintainers' key, which is built in, plus any you add.
2. The index lists the SHA-256 of every package file. Each download must match it exactly, validate in full for its kind, and carry the name and version the index lists.
3. A bundle has no file of its own: what it includes is part of the signed index.
4. Every package is downloaded and checked before anything is installed, so a bad file changes nothing.

Packages in the Plonix Market are reviewed by the maintainers before the index is signed. A changed index, a swapped file or a key you do not trust is refused with a message that says which.

Skills are text and packs are data. Extensions are the only packages that run code, and only in the sandbox: no network, files or processes, a CPU, memory and time budget, and only the capabilities you approved. The Market shows what an extension may do before it installs, and sensitive capabilities need their own yes. An extension that needs something this version cannot run yet is listed as *Coming soon*. See [extensions.md](extensions.md).

A copy of the Plonix Market is built into Plonix, signed with the same key. When the online index cannot be fetched or verified, the Market shows that copy and says so, so it works offline.

## Other Markets

A team can host its own Market anywhere: an https address or a folder. Point Plonix at it in **Settings › Market**, or with `--index` or `$PLONIX_MARKET_INDEX`.

```sh
plonix market keygen ~/secrets/acme-market.key     # once; keep the private key secret
plonix market check store/index.json               # every package matches and validates
plonix market sign store/index.json --key ~/secrets/acme-market.key
```

People who use it trust the publisher's public key once:

```sh
plonix market trust ed25519:…        # or add it in Settings › Market
```

`--allow-unsigned` lets a Market author try an index before signing it. The window never installs from an unsigned index.

## Skills

A skill tells an agent how to do one job with Plonix's read-only tools: get to know a host, explain a request, review sign-in and sessions, check scope suggestions, draft a finding, inventory an API. Five are built in; more come from the Market.

Agents connected with `plonix connect claude` (or any MCP client running `plonix mcp`) get skills two ways:

- as **MCP prompts**: in Claude Code, type `/mcp__plonix__triage-host api.example.com`;
- as the **`list_skills`** and **`get_skill`** tools, so the agent can pick a skill itself when a request matches one.

Skills never widen what an agent can do. A skill declares which data it reads (`uses`), and when any of it is switched off in **Settings › AI agents**, agents are not offered the skill and the engine refuses to hand it out. Agents cannot install anything from the Market.

```sh
plonix skills                                    # skills, and which ones agents are offered
plonix skills show explain-request --arg id=42   # exactly what the agent receives
plonix skills add ./my-skill.md                  # your own, from a file or https URL
plonix skills check ./my-skill.md                # validate before sharing
```

### Writing a skill

A skill is Markdown with a short header:

```markdown
---
plonix_skill: 1
name: triage-host
version: 1.0.0
title: Get to know a host
description: Summarize what a host does, how it is built and where to look first.
author: Your name
uses: [map, traffic, scope]
argument: host: The host to look at, such as api.example.com
---
Build a short briefing on {{host}} for the user...
```

- `uses` is what the skill reads: any of `traffic`, `insights`, `map`, `scope`, `findings`, `scan`.
- `argument: name: description` declares a required argument; `optional_argument:` an optional one. At most five. `{{name}}` in the instructions is replaced with the value.
- Name the Plonix tools the agent should call (`search_traffic`, `get_request`, `list_endpoints`…). Agents cannot send requests, change scope or record findings, so a skill asks the user to do those in Plonix.
- Instructions are at most 20,000 characters.

To publish one in the Plonix Market, add it to `store/skills/` and its entry to `store/index.json` (see [store/README.md](../store/README.md)).

## Adding things from outside the Market

You can add a skill, rule pack or filter pack from a file or a link that is not in a signed Market:

```
plonix market add ./my-skill.md            # shows what it is and what it does
plonix market add ./my-skill.md --yes      # adds it
plonix market add https://example.com/pack.json --yes
```

In the app, use **Add from a file or link** on the Market screen. Plonix reads the file in full, shows its name, author, what it does and its checksum, and adds it only after you confirm.

Anything added this way is marked **Not verified**: nobody has signed or reviewed it. It is still checked and cannot run code, and agents cannot add anything themselves. A skill that is not verified carries a note saying so when an agent uses it.

To use a Market list that is not signed, turn on **Allow unsigned Market lists** in Settings › Market. Everything from it is then marked Not verified.

## Item pages

Selecting anything in the Market opens its own page: a description written for people, what it does and what it needs, who made it, its version and checksum, the bundles it is part of, and whether it is verified. Index authors add the description with an `about` list of short paragraphs (at most 8 of 700 characters) on each package.
