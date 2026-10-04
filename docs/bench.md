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

Pick a built-in list, a **number range**, or a **custom list** you type in. The
built-in lists ship with Plonix:

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
(one value per line) or `-` for stdin.
