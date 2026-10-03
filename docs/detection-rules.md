# Detection rules

Plonix recognises the technologies behind every host you browse (servers, frameworks, CMSs, CDNs, WAFs, identity providers, exposed admin consoles) using **declarative detection rules**. Rules ship in **rule packs** that anyone can write, share and install from a file, a URL or the [store](#the-store). The engine's Discovery layer runs them over captured traffic, and the results show up per host in `plonix tech`, in `GET /api/tech`, and in the Map.

```text
$ plonix tech
shop.acme.test
  web-server     nginx 1.25.3               100%  response header server ~ nginx/1.25.3 (#3)
  language       PHP 8.2.12                 100%  response header x-powered-by ~ PHP/8.2.12 (#3)
  cms            WordPress 6.4.2            100%  path ~ /wp-content/ (#5)
```

Every detection carries its evidence: the condition that matched, a sample of what it matched, and the exchange id, so `plonix show 3` takes you straight to the proof.

## Contents

- [How detection runs](#how-detection-runs)
- [Trust boundary](#trust-boundary)
- [Writing a rule](#writing-a-rule)
- [Conditions](#conditions)
- [Rule packs](#rule-packs)
- [Installing and managing packs](#installing-and-managing-packs)
- [The store](#the-store)
- [Limits](#limits)
- [Contributing a pack](#contributing-a-pack)

## How detection runs

- Detection is **passive**. It only reads traffic that the proxy has already captured. A rule never causes a request to be sent.
- It runs on demand over each host's most recent 300 exchanges, so **rules you install later apply to everything you already captured**. There is nothing to re-browse.
- The engine notices when packs are added or removed (`rules/lock.json` changes) and reloads them. There is no restart.
- Built-in packs are always loaded. Installed packs are loaded after them.
- When two rules or packs detect the same `id`, Plonix shows one entry with the higher confidence and keeps any version either of them found.
- `implies` adds related technologies (WordPress implies PHP), marked `implied by …` and carrying the confidence of the rule that implied them. Direct evidence always wins over an implication.

## Trust boundary

Rule packs come from people you don't know. Plonix treats every pack as **untrusted data** and nothing more:

| Threat | What Plonix does |
| --- | --- |
| A pack tries to run code | Packs are JSON data. There is no field that is executed, evaluated or passed to a shell, and unknown fields are rejected (`"postinstall": …` fails validation). |
| A malicious regex hangs the engine (ReDoS) | Patterns compile with Rust's `regex` crate, which matches in linear time and has no backtracking, look-around or back-references. Compiled size is capped too, so a pattern can't eat memory. |
| A pack swaps its contents after review | Store entries pin the SHA-256 of the exact bytes. Downloads that don't match are refused before anything is written. Installed packs are pinned in `lock.json` and re-verified on every load; a pack edited on disk is skipped with a warning. |
| A downgrade or MITM on download | URLs must be `https://`. Plain `http://` is only accepted for this machine (for testing a store you're writing), and a redirect can't leave https. |
| Terminal injection through names or evidence | All pack text is length-limited and may not contain control characters, so names and descriptions can't carry ANSI escapes. Evidence samples are stripped of control characters before display. |
| Detections leak credentials into the Map or an agent's context | Evidence for `cookie`, `request_header` and `query_param` conditions names the cookie, header or parameter but never repeats its value, since those are often session tokens or API keys. |
| A pack overrides a built-in | Built-in pack names are reserved. |
| A pack exfiltrates traffic or reaches the network | Rules have no way to express either. They can only produce a detection (id, name, category, version, confidence, evidence) for traffic the engine hands them. |
| A pack changes scope | Rules have no access to scope. Scope enforcement on active requests is untouched by detection. |

What a pack *can* do is be wrong: claim a technology that isn't there, or miss one that is. That's why every detection shows its evidence and confidence, and why the store is reviewed (see [Contributing a pack](#contributing-a-pack)).

## Writing a rule

```json
{
  "id": "wordpress",
  "name": "WordPress",
  "category": "cms",
  "website": "https://wordpress.org",
  "confidence": 100,
  "match": "any",
  "implies": ["php"],
  "conditions": [
    { "path": "^/wp-(?:content|includes|admin|json)/" },
    { "body": "<meta name=\"generator\" content=\"WordPress ?([\\d.]+)?", "version": "$1" },
    { "cookie": "wordpress_test_cookie" }
  ]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Stable identifier, `[a-z0-9._-]`, at most 64 characters. Shared across packs: if you detect nginx, use `nginx`. |
| `name` | yes | Display name, at most 64 characters. |
| `category` | yes | One of `web-server`, `language`, `framework`, `cms`, `ecommerce`, `javascript`, `cdn`, `waf`, `hosting`, `load-balancer`, `cache`, `auth`, `api`, `analytics`, `database`, `devops`, `other`. The list is fixed so packs from different authors group the same way. |
| `description` | no | At most 500 characters. Say why it matters to a researcher if it isn't obvious ("management endpoints; env and heapdump can leak secrets"). |
| `website` | no | http(s) URL of the project. |
| `confidence` | no | 1 to 100, default 100. Lower it for weak signals, like a generic cookie name. |
| `match` | no | `any` (default): one condition is enough. `all`: every condition must match somewhere in the host's traffic (not necessarily in the same exchange). |
| `conditions` | yes | 1 to 32 [conditions](#conditions). |
| `implies` | no | Up to 16 ids of technologies this one implies. |

## Conditions

Each condition sets **exactly one** target:

| Target | Matches | `regex` is applied to |
| --- | --- | --- |
| `header` | a response header, by name (case-insensitive) | the header's value |
| `request_header` | a request header, by name | the header's value |
| `cookie` | a cookie, by name, from `Set-Cookie` or `Cookie`. `name*` matches every cookie whose name starts with `name`. | the cookie's value |
| `query_param` | a query string parameter, by name | its value |
| `path` | the request path (pattern) | — |
| `host` | the host name (pattern) | — |
| `body` | the decoded response body, first 512 KiB of text responses (pattern) | — |

For the named targets (`header`, `request_header`, `cookie`, `query_param`), the target's presence alone is a match unless you add `regex`. For `path`, `host` and `body`, the value *is* the pattern, and `regex` isn't allowed.

Patterns use [Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax) and are **case-insensitive**. Remember to escape backslashes in JSON (`"\\d"`).

**Versions.** Add `"version": "$1"` to pull a version out of a capture group. Templates may only contain `$1` to `$9` and the characters `[A-Za-z0-9._-]`, and they must refer to a group the pattern actually has. If one exchange shows `Server: nginx` and a later one shows `Server: nginx/1.25.3`, Plonix reports `1.25.3`.

```json
{ "header": "Server", "regex": "^nginx(?:/([\\d.]+))?", "version": "$1" }
{ "cookie": "incap_ses_*" }
{ "query_param": "response_type", "regex": "id_token" }
{ "request_header": "Authorization", "regex": "^Bearer eyJ[A-Za-z0-9_-]+\\.eyJ" }
```

## Rule packs

A pack is one JSON file:

```json
{
  "plonix_pack": 1,
  "name": "acme-internal",
  "version": "1.0.0",
  "description": "Acme's in-house gateway and auth service",
  "author": "Acme red team",
  "license": "Apache-2.0",
  "homepage": "https://example.com/plonix-rules",
  "rules": [ … ]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `plonix_pack` | yes | Format version. Currently `1`. |
| `name` | yes | `[a-z0-9-]`, at most 64 characters. Also the install name. |
| `version` | yes | `MAJOR.MINOR.PATCH`, optionally `-pre`. Used by `plonix store update`. |
| `description`, `author` | yes | Shown in listings. |
| `license`, `homepage` | no | |
| `rules` | yes | 1 to 2000 rules, with unique ids within the pack. |

A pack's checksum is the SHA-256 of the file's exact bytes. It lives outside the pack (in the store index, or on the command line with `--sha256`), because a file can't vouch for itself.

Validation reports **every** problem at once, with its location:

```text
$ plonix rules check bad.json
error: invalid rule pack:
  - rules[0] (acme-gateway): category: `rootkit` is not one of web-server, language, …
```

## Installing and managing packs

```sh
plonix rules                                   # packs in effect: built-in and installed
plonix rules check ./my-pack.json              # validate without installing; prints the sha256
plonix rules add ./my-pack.json                # install from a file
plonix rules add https://example.com/pack.json --sha256 <hex>   # from a URL, pinned
plonix rules remove my-pack
plonix tech                                    # what the rules found, per host
plonix tech shop.example.com
```

`rules add` without `--sha256` pins whatever it downloaded and prints the checksum, so later tampering is still detected. Pass `--sha256` to make sure you get the exact build you reviewed.

Installed packs live in `~/.plonix/rules/` (or `$PLONIX_HOME/rules/`):

```text
rules/
├── lock.json          name → version, sha256, source, installed_at
└── packs/<name>.json  the exact bytes that were verified
```

Plonix ships six packs, five built in and one in the store:

| Pack | Covers |
| --- | --- |
| `web-servers` (built in) | nginx, OpenResty, Apache, IIS, LiteSpeed, Caddy, Envoy, Traefik, HAProxy, Gunicorn, Uvicorn, Werkzeug, Jetty, Tomcat, Kestrel… |
| `frameworks` (built in) | PHP, ASP.NET, Express, Next.js, Nuxt, React, Vue, Angular, jQuery, Django, Flask, FastAPI, Rails, Laravel, Symfony, Spring… |
| `cms` (built in) | WordPress, Drupal, Joomla, Ghost, Magento, Shopify, WooCommerce, PrestaShop, Contentful, Strapi |
| `edge` (built in) | Cloudflare, Fastly, Akamai, CloudFront, AWS ELB/S3/WAF, Google Cloud, Azure, Vercel, Netlify, Heroku, Varnish, Imperva, Sucuri |
| `api-surface` (built in) | GraphQL, OpenAPI/Swagger docs, gRPC-Web, JSON-RPC, SOAP, Spring Boot Actuator, OIDC, OAuth 2.0, Keycloak, Auth0, Okta, JWTs, Sentry |
| `admin-panels` (store) | Jenkins, GitLab, Grafana, Kibana, Elasticsearch, Prometheus, Argo CD, SonarQube, Jira, Confluence, phpMyAdmin, Kubernetes Dashboard, MinIO |

## The store

The store is a **static JSON index** that can be hosted anywhere: the Plonix repository (the default), a fork, an internal web server, or a folder on disk. There is no server to run and no account.

```json
{
  "plonix_index": 1,
  "name": "Plonix community store",
  "packages": [
    {
      "name": "admin-panels",
      "kind": "rules",
      "version": "1.0.0",
      "description": "DevOps tools and admin consoles that are often exposed by mistake.",
      "author": "Plonix contributors",
      "url": "packs/admin-panels.json",
      "sha256": "…64 hex characters…"
    }
  ]
}
```

- A relative `url` resolves against the index's own location and may not contain `..`. An absolute `url` must be `https://`.
- `kind` is `rules` today. `extension` entries are listed (marked *needs runtime*) but can't be installed until the extension sandbox ships; see [extensions.md](extensions.md).
- Installing downloads the pack, checks its SHA-256 against the index, checks that its name and version match the entry, validates it, and only then writes it to disk.

```sh
plonix store                         # list the community store
plonix store list graphql            # filter by name or description
plonix store install admin-panels
plonix store update                  # upgrade installed packs that have a newer version
plonix store --index https://intranet.example.com/plonix/index.json   # or $PLONIX_STORE_INDEX
plonix store --index ./my-store/index.json                            # a local store
```

The index is the root of trust: whoever controls it decides which bytes are acceptable. Use the default community index, one you host yourself, or one from a team you trust. Teams can run a private store by putting an `index.json` and their packs on any internal https host.

## Limits

| Limit | Value |
| --- | --- |
| Pack file size | 1 MiB |
| Rules per pack | 2000 |
| Conditions per rule | 32 |
| `implies` per rule | 16 |
| Pattern length | 1000 characters |
| Compiled pattern size | 256 KiB |
| Body scanned per response | 512 KiB of decoded text |
| Exchanges examined per host | 300 most recent |
| Installed packs | 200 |
| Store index size | 4 MiB, 5000 packages |

## Contributing a pack

1. Write your pack and run `plonix rules check your-pack.json` until it's clean. Test it against real traffic with `plonix rules add` and `plonix tech`.
2. Prefer specific signals (a distinctive header, a framework-specific cookie) over generic ones, and lower `confidence` for anything that could be a coincidence.
3. Reuse existing ids for existing technologies so detections merge instead of duplicating.
4. Open a pull request that adds `store/packs/<name>.json` and an entry in `store/index.json` with the `sha256` printed by `plonix rules check`. `cargo test` (and CI) checks that every index entry matches its file.

Reviewers check that patterns are specific, categories are right, and nothing in the pack is surprising. The checksum in the index is what makes that review stick: once merged, users install exactly the bytes that were reviewed.
