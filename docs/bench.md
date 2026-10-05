# The Bench

The Bench is where you work on one request at a time and where you run many at
once. Each tab is an experiment. A tab has two panels:

- **Send** — edit a request, send it, branch it, and compare responses.
- **Run** — mark positions in the request and feed lists of values through them.

Everything the Bench sends goes through the engine's single send path, so it can
only ever reach a host that is **accepted into scope**, exactly like a manual
send. When the host has a [client certificate](projects.md#client-certificates),
the Bench presents it and marks the response **cert · CN=…**.

## Claude's suggested edits

Under the request on the Send panel, **Ask about this request** (or **✦ Ask
Claude** on selected text, or on a value the Lens spotted) asks Claude Code
about the draft you are editing, before it is sent.

Besides answering, Claude can propose a concrete edited request: say, the same
call with a different id, an extra header, a changed JSON field or a JWT with a
raised role. The proposal appears above the request as a card:

- a one-line **summary** of what Claude changed and why;
- a **diff against your draft as it is now**: the request line, headers that
  were added, removed or changed, and the body line by line. Query strings and
  form bodies are compared field by field with their values decoded, JSON
  bodies are compared pretty-printed, and a JWT is compared as its decoded
  header and claims;
- **Apply to draft** and **Discard**.

Nothing changes until you press **Apply to draft**, and applying only rewrites
the draft: the request is not sent. Read it over, edit it further if you like,
and press **Send** yourself. Right after applying, **Undo** puts your previous
draft back. If you keep editing while a proposal is open, its diff follows
your edits. A proposal Claude makes while you are on another screen, or from a
Claude Code session in a terminal, shows up when you come back to the tab.

JWTs follow the same rule as editing one in the Lens. Claude has no signing
key, so a token whose header or claims it changed **keeps its original
signature** and is marked **unsigned**: that is how you test whether the
server checks signatures. If Claude made up a signature, Plonix puts the
original one back and says so. A token Claude deliberately left without a
signature, or with `alg: none`, is kept as proposed. To send a validly signed
token, apply the edit and re-sign the token in the Lens with the key.

Claude can only *propose*. It cannot apply a proposal, send it, or start a
run; see [agents.md](agents.md#suggested-bench-edits).

## Payload runs

A *run* sends a batch of requests built from one base request. You mark the
spots that should change (the *positions*) and point one or more *lists* of
values at them.

### Marking positions

Select the part of the URL or the request you want to vary and press **•** (in
the URL) or **+ Mark position** (in the request body/headers). The selection is
wrapped in `•…•`. You can mark several positions. From the command line, type
the markers yourself:

```
plonix bench run 'https://host/api?id=•1•' --list range:1-100
```

The Run panel shows the request with each marked position highlighted, so it is
always clear where the values go. Below it, each position gets a card naming
what it is (a number, an identifier, a path segment, a username, a JWT, and so
on) and where it sits, with its own list picker when a mode needs one.

### Modes

- **One at a time** (single-position) — one position changes per request while
  the others keep their base value. One list is reused for every position. Good
  for walking a single parameter through a list.
- **Lockstep** (multi-position) — every position changes together, each stepping
  through its own list by index. The run stops at the shortest list. Good for
  paired values, like a username and its matching token.
- **All combinations** (multi-position) — every combination of values across the
  positions. Each position gets its own list.

### Lists

For each position, pick a list, a **number range**, or **type your own** values.
Every picker shows a live preview of the first values and how many there are, so
you can see what a list holds before you run it. Plonix also **suggests** lists
that suit each position — identifier formats for a numeric id, path lists for a
path segment, usernames for a sign-in field — as one-click chips.

The built-in lists ship with Plonix:

```
plonix bench lists
```

More lists can be installed from the Market.

### Running

Every request is a scope-gated send, recorded in Traffic like any other (search
`source:replay`). A run is bounded by a **request budget** (default 1000, with a
hard ceiling) and paced by a small **delay** between requests. Tick **Send
original first** to include an unmodified baseline to compare against. In the
results table, sort by status, length or time to spot the response that stands
out, open a row to read its response, and turn any row into a finding.

Runs never start on their own, and AI agents cannot start one — the run route is
in no agent mode's capabilities.

### From the command line

```
# single-position sweep over a range, with a baseline
plonix bench run 'https://host/item?id=•1•' --list range:1-5 --base

# all combinations of two lists, with a header position and a body
plonix bench run 'https://host/q?a=•x•' -X POST \
  -H 'X-Mode: •m•' --body 'q=test' \
  --mode matrix --list values:1,2 --list builtin:http-methods
```

Lists are given as `builtin:<id>`, `range:A-B[:step]`, `values:a,b,c`, `@file`
(one value per line) or `-` for stdin. `builtin:<id>` names any list in the
library — built-in or installed from the Market.

## List packs

Payload lists are shared through the [Market](market.md) as *list packs*, the
same signed-and-checked path as rule packs and filter packs. A list pack is a
JSON file:

```json
{
  "plonix_lists": 1,
  "name": "extra-wordlists",
  "version": "1.0.0",
  "description": "More starting-point lists for the Bench.",
  "author": "you",
  "lists": [
    { "id": "id-formats", "title": "Identifier formats", "values": ["0", "-1", "00000000-0000-0000-0000-000000000000"] }
  ]
}
```

Each list has a lowercase `id` (how the Bench and `--list builtin:<id>` name it),
a `title`, an optional `description`, and its `values`. A pack is data only — it
supplies values you then choose to send through the scope-gated send path; it
cannot run code, send anything, or change scope. Install one with
`plonix market install <name>`, or in the Market screen, and its lists appear in
the Bench list picker alongside the built-in ones. The lists that ship with
Plonix are the built-in `starter-lists` pack.
