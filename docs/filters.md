# Named filters and filter packs

Named filters are shortcuts for searches you'd otherwise type over and over: "auth flows", "API docs", "hide analytics and trackers". Anyone can write them, share them as a **filter pack**, and install them from a file, a URL or the [store](detection-rules.md#the-store).

```text
$ plonix search is:auth -is:trackers
    19  GET     200  out      11 B  text/html        http://ci.acme.test:8001/login
    17  GET     200  out       2 B  application/json http://api.acme.test:8001/oauth/token
    12  GET     200  out      11 B  text/html        http://shop.acme.test:8001/wp-login.php
```

In the window, every named filter appears under **+ Filter** in Traffic. Pick *Show only* or *Hide*, then click a filter. It becomes a chip like any other filter and is saved with the project.

## Typing them

You don't have to remember field names or values. As you type in the Traffic search box, a list opens under it: the filter fields first (host, status, method, path, content type, extension, kind, scope, source, named filters), then the values for the field you picked, taken from this project's own traffic, with request counts and notes such as "in scope" or a named filter's label. After two letters of plain text it also offers matching values from any field, so `upl` offers `host:uploads…` and `is:uploads`.

↑↓ choose, Tab completes the text, Enter adds it as a filter chip and Esc closes the list. A leading `-` carries through (`-host:` suggests hosts to hide), and after a comma (`status:4xx,`) it suggests the remaining values. The value box in **+ Filter** uses the same list.

## Using them

| Query | Means |
| --- | --- |
| `is:graphql` | show only what the `graphql` filter matches |
| `-is:trackers` | hide what the `trackers` filter matches |
| `is:graphql,api-docs` | show what either filter matches |
| `is:auth status:4xx` | combine with any other search term |

`plonix filters` lists every filter in effect, with the pack it came from and the query behind it.

## Built-in and store filters

The `common` pack is built in: `is:api`, `is:graphql`, `is:auth`, `is:admin`, `is:writes`, `is:uploads`, `is:redirects`, `is:failures`, `is:denied`, `is:api-docs`, `is:files` and `is:trackers`.

The store has `leaks` too: AWS keys, private keys, JWTs, stack traces, SQL errors, debug pages, source maps and internal IPs. Install it with `plonix store install leaks`.

## Writing a filter pack

```json
{
  "plonix_filters": 1,
  "name": "acme-filters",
  "version": "1.0.0",
  "description": "Filters for the Acme engagement",
  "author": "Acme red team",
  "filters": [
    {
      "id": "acme-console",
      "label": "Acme console",
      "description": "The internal admin console",
      "query": "host:admin.acme.test path:/console"
    }
  ]
}
```

| Field | Rules |
| --- | --- |
| `id` | `[a-z0-9-]`, at most 40 characters. Used as `is:<id>`. |
| `label` | What the chip shows, at most 40 characters. |
| `description` | Optional, at most 300 characters. Shown as the tooltip. |
| `query` | Any search (see `plonix search --help`), at most 500 characters. It may not use `is:`, so filters can't nest or loop. |

```sh
plonix filters check acme-filters.json    # validate and print the sha256
plonix filters add acme-filters.json --yes # or an https:// URL, optionally --sha256 <hex>
plonix filters remove acme-filters
```

New packs take effect right away, even while the engine runs. If two packs define the same id, the first one wins (built-in packs, then installed packs by name), and `plonix filters` warns about the one that was ignored.

## Safety

Filter packs are untrusted data, handled like [rule packs](detection-rules.md#trust-boundary). They're strictly validated (unknown fields are rejected, text can't contain control characters), pinned by SHA-256 on install and checked again every time they load. A filter can only narrow the traffic list. It can't change scope, send anything, or run code.

To publish a pack in the community store, add it under `store/filterpacks/` and list it in `store/index.json` with `"kind": "filters"` and the sha256 from `plonix filters check`.
