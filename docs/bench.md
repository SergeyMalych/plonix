# The Bench

The Bench is where you work on one request at a time and where you run many at
once. Each tab is an experiment. A tab has two panels:

- **Send** — edit a request, send it, branch it, and compare responses.
- **Run** — mark positions in the request and feed lists of values through them.

Everything the Bench sends goes through the engine's single send path, so it can
only ever reach a host that is **accepted into scope**, exactly like a manual
send.

## Payload runs

A *run* sends a batch of requests built from one base request. You mark the
spots that should change (the *positions*) and point one or more *lists* of
values at them.

### Marking positions

Select the part of the URL or the request you want to vary and press **§** (in
the URL) or **+ Mark position** (in the request body/headers). The selection is
wrapped in `§…§`. You can mark several positions. From the command line, type
the markers yourself:

```
plonix bench run 'https://host/api?id=§1§' --list range:1-100
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
plonix bench run 'https://host/item?id=§1§' --list range:1-5 --base

# all combinations of two lists, with a header position and a body
plonix bench run 'https://host/q?a=§x§' -X POST \
  -H 'X-Mode: §m§' --body 'q=test' \
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
