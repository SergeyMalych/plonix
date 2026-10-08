# The Market and skills

The Market is where Plonix gets everything modular, in one catalog:

| Kind | What it is | Installs into |
| --- | --- | --- |
| **Skill** | A playbook an AI agent follows for one job in Plonix | `$PLONIX_HOME/skills` |
| **Rule pack** | Technology detection rules ([format](detection-rules.md)) | `$PLONIX_HOME/rules` |
| **Filter pack** | Named Traffic filters such as `is:auth` ([format](filters.md)) | `$PLONIX_HOME/filters` |
| **List pack** | Named payload lists for the Bench ([format](bench.md)) | `$PLONIX_HOME/lists` |
| **Platform** | Where bug bounty programs come from: a platform's API address, sign-in and data shape ([format](programs.md#platform-packs)) | `$PLONIX_HOME/platforms` |
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

Every Plonix item also has a **How to use it** guide in [`store/guides.json`](../store/guides.json): where it shows up in Plonix, the steps to get it working, and optionally a screenshot from `store/guides/`. Guides ship inside the app, not in the signed list, so improving one never needs a new signature. `cargo test` fails if a Market item has no guide.

## Three shelves

Every package in the Market carries a badge that says who stands behind it:

| Badge | Where it comes from | Who looked at it |
| --- | --- | --- |
| **Official** (or **Built in**) | The Plonix Market, signed with the Plonix key | The Plonix maintainers reviewed it before signing |
| **Community** | The [community Market](#the-community-market), signed with the community key | Checked automatically, and a reviewer read what it asks for. Its author wrote and maintains it; the maintainers have not reviewed its code |
| **Your own** | Added by you from a GitHub repository, a folder, a file or a link ([below](#adding-your-own)) | Nobody |

A Market you host yourself and trust by its key shows **Verified by** its publisher. Something whose file changed on disk after it was installed shows **Changed** and does not load.

Every shelf can do the same things. An extension from any of them can ask for any capability, and every sensitive one needs its own yes when it installs. On Community and Your own extensions that yes sits next to a red line saying Plonix has not reviewed the code. What no extension can do, from any shelf, is listed in [Invariants](extensions.md#invariants): no network, files or processes outside the sandbox, and no change of scope you did not click.

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

## Adding your own

You can add a skill, a pack or an extension that is in no Market: from a GitHub repository, an extension's folder, a file or a link.

```
plonix market add github:jsmith/graphql-notes          # the package attached to the latest release
plonix market add github:jsmith/graphql-notes@v1.2.0   # one release
plonix market add github:jsmith/kit#notes.plonixext    # one file, when a release has several
plonix market add ./my-extension                       # an extension's folder, while you write it
plonix market add https://example.com/skill.md --yes
plonix extensions add github:jsmith/graphql-notes --grant scoped-requests   # an extension, with a sensitive capability
```

In the app, use **Add your own** on the Market screen. Paste a repository, a path or an address, or choose a file or a folder. Plonix reads the file in full, shows its name, author, where it came from, what it may do and its checksum, and adds it only after you confirm.

- **From GitHub**, Plonix reads the repository's latest release (or the one you name with `@tag`) through GitHub's public API and downloads the package file attached to it: a `.plonixext` extension, a skill (`.md`) or a pack (`.json`). It never builds or runs the repository's code. `github.com/owner/repo` and the repository's https address work too.
- **From a folder**, Plonix packs the extension in it (its `plonix-extension.json` and the module it names). The item shows **From a folder** with a **Read again** button for after you rebuild it.
- **New releases.** For something added from GitHub, the Market checks the repository for a newer release and shows **Release v1.3.0 is out** on its row. It never updates it on its own: **Look at v1.3.0** opens the same sheet, so you see what the new release asks for, and anything new needs your yes again. `plonix market update` lists them too.
- **Removing** works like any other package: **Remove** on its page, or `plonix market remove <name>`.

Anything added this way is marked **Your own**. It is still checked in full, and a skill that is your own carries a note saying so when an agent uses it. Agents cannot add anything themselves, and a name that belongs to something built in cannot be taken.

To use a Market list that is not signed, turn on **Allow Markets that are not signed** in Settings › Market. Everything from it is then marked Not verified.

## The community Market

The community Market is a second list, next to the Plonix Market, for packages their authors write and maintain. The Market screen shows them with a **Community** badge and a **Community** filter; switch them off in **Settings › Market › Show the community Market**.

- The list is `community/index.json` in the Plonix repository, signed with its own key. That key is trusted for the community list only, never for the Plonix Market.
- A package's file stays where its author published it: the list pins a GitHub release file by its SHA-256, so a replaced file is refused.
- The Plonix Market wins a name both use. Community bundles, tools and packages that require others are left out, so a community package never pulls in anything else by name.

### Publishing to the community Market

1. Publish your package in your own GitHub repository, as a file attached to a release. Check it first with `plonix extensions check ./my-extension` (or `plonix skills check`, `plonix rules check`…). People can already add it as their own with `plonix market add github:you/your-repo`.
2. Open a pull request to the Plonix repository that adds one entry to `community/index.json`: the package's `name`, `kind`, `version`, `description`, `author` (your GitHub name), `homepage` (your repository), `url` (the release file's download address) and its `sha256`. The repository must have an open-source license.
3. The **Market lists** workflow downloads the file, matches the checksum and validates it in full. A reviewer reads what it asks for and checks that the release was built from the repository's source.
4. When the pull request merges, the community list is signed and the package shows up in everyone's Market. A new version is a new entry with the new release's file and checksum.

See [community/README.md](../community/README.md) for an example entry.

### From Community to Official

A community package that many people use and that has stayed stable can be proposed for the Plonix Market. A maintainer reviews its source, it moves into `store/`, and it is signed with the Plonix key. Only then does it carry the Official badge.

## The block list

When something on the Market turns out to be harmful, the maintainers add it to `store/blocked.json`, signed with the Plonix Market key. Plonix reads it every time it opens the Market and keeps the last verified copy, so it also applies offline and when Plonix starts.

- A blocked package cannot be installed or added, from any shelf: a Market, a repository, a folder, a file or a link.
- One that is already installed is switched off with the reason (an extension) or removed (anything else). An extension that is blocked cannot be switched back on.
- An entry names a package, one file of it (by SHA-256), or both:

```json
{ "plonix_blocked": 1,
  "entries": [ { "name": "bad-ext", "kind": "extension", "reason": "Sends tokens to its author." } ] }
```

## How the Markets are signed

The **Market lists** workflow (`.github/workflows/market.yml`) runs on every pull request that changes `store/` or `community/`. It checks every package, then signs `store/index.json`, `store/blocked.json` and `community/index.json` with keys kept as repository secrets (`PLONIX_MARKET_KEY` and `PLONIX_COMMUNITY_KEY`), commits the signatures to the pull request and runs CI again. The keys only ever reach a signer built from `main`. Merging the pull request is the review. Pull requests from forks get the checks but not the keys; a maintainer brings the change into a branch of the repository to have it signed.

## Item pages

Selecting anything in the Market opens its own page: a description written for people, what it does and what it needs, who made it, its version and checksum, the bundles it is part of, and whether it is verified. Index authors add the description with an `about` list of short paragraphs (at most 8 of 700 characters) on each package.
