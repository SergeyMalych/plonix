# Programs

Bug bounty and vulnerability disclosure programs say which assets you may test and under which rules. Plonix reads a program, shows you what it will change, and from then on follows it: the program's scope becomes the project's scope, and its rules of engagement apply to every request Plonix sends.

Programs is a tool you switch on from the **Market**: install **Programs** there and the Programs screen appears in the sidebar (⌘9 in the app). A project that already follows a program keeps the screen either way. The CLI works without it:

```sh
plonix program follow hackerone:acme          # read a program and show what would change
plonix program follow hackerone:acme --yes    # follow it
plonix program                                # what this project follows
plonix program stop                           # stop following it (scope rules stay)
```

## Where programs come from

| Source | How |
| --- | --- |
| **HackerOne** | Connect once with your API username and token (HackerOne, Settings, API Token). Plonix then pulls every program you can work on, with each one's assets and rules, and keeps them on this computer. `plonix program connect hackerone --user <name>` reads the token from standard input or `$PLONIX_PLATFORM_TOKEN`. |
| **A pasted policy** | Paste the program's scope or policy text, or its page address. `plonix program follow ./policy.txt` or `plonix program follow https://example.com/security`. |
| **A domain** | For a disclosure program without a platform, Plonix reads the domain's `/.well-known/security.txt` (RFC 9116) and the policy page its `Policy:` line points to. `plonix program follow example.com`. |

More platforms come from the [Market](market.md) as platform packs.

Reading a policy is a careful first pass, not a promise: Plonix picks out hosts under in-scope and out-of-scope headings, the request rate, required headers and banned kinds of testing. You review all of it before anything applies, and can change any of it.

## Every program at once

Once a platform is connected, Plonix pulls all of your programs in the background: each program's in-scope and out-of-scope assets, bounty eligibility, maximum severity and the rules read from its policy. It syncs again when you open Programs and the last sync is more than a day old, or when you click **Sync now**. Requests to the platform are spaced out, and Plonix waits when the platform asks it to slow down.

- **Programs** lists every program, open ones first, with its in-scope assets by type and its rules at a glance. Search finds a program by name or by any of its assets.
- **Assets** lists every in-scope asset across all programs, so you can see which program owns a host.
- **Bounty only** hides programs and assets without a bounty.

The pulled programs are kept in your Plonix home (`platforms/catalog/`), readable only by you, and are deleted when you disconnect the platform. When a program you follow changes its scope on the platform, Programs says so and lets you review the change before it applies.

From the terminal: `plonix program sync hackerone`, `plonix program list hackerone`, and `plonix program assets hackerone --find example.com`.

## What following a program does

| The program says | Plonix does |
| --- | --- |
| In scope: `app.example.com` | Accepts it in Scope. |
| In scope: `*.example.com` | Accepts `example.com` and every subdomain. |
| In scope: `203.0.113.0/24` | Accepts that IP range. A single address or host inside it can still be decided on its own. |
| Out of scope: `status.example.com` | Rejects it. The more specific rule wins over the wildcard, so Plonix never sends to it. |
| A mobile app, source code, hardware | Lists it on the program, outside Plonix's scope rules. |
| At most 5 requests per second | Every request Plonix sends (Bench, Bench runs, scans, crawls) waits its turn. Browsing is not slowed. |
| Add `X-Bug-Bounty: <your username>` | Adds the header to every request to the program's in-scope hosts, browser traffic included. A value with a placeholder is not sent until you fill it in; with a platform account, Plonix fills in your user name. A header you set yourself on a request is kept. |
| No automated scanning | Scans, crawls and Bench runs are off for the project, with the reason shown. Browsing and single Bench requests still work. |
| No denial of service | Intrusive scan checks are off and cannot be picked. Plonix assumes this for every program. |
| Won't accept: missing headers, self-XSS | Kept with the program, so you can check before you report. |

Scope rules a program adds carry the note `program:<platform>/<id>`. Syncing the program again replaces exactly those rules, and stopping can remove them or leave them in place.

## Tokens

Platform tokens are kept in the macOS Keychain (in a private file, readable only by you, on other systems and for other Plonix homes). Plonix sends a token only to the API address its platform pack declares, over HTTPS. AI agents can read neither programs nor tokens, and cannot change either.

## Platform packs

A platform pack is a JSON file that tells Plonix where a platform's API is and where in its answers the programs and their assets are. It runs no code: one fetcher in Plonix reads every platform, and it refuses any address outside the pack's `api` origin, including the next-page links the API returns.

```json
{
  "plonix_platform": 1,
  "name": "acme-bounty",
  "version": "1.0.0",
  "title": "Acme Bounty",
  "description": "Programs from Acme Bounty.",
  "author": "you",
  "api": "https://api.acme-bounty.example",
  "program_url": "https://acme-bounty.example/programs/{handle}",
  "auth": { "kind": "bearer", "secret_label": "API token", "help": "Create a token under Settings, API." },
  "programs": {
    "path": "/v1/programs",
    "items": "/data",
    "next": "/links/next",
    "fields": { "handle": "/slug", "name": "/name", "bounty": "/pays_bounties" }
  },
  "program": { "path": "/v1/programs/{handle}", "policy": "/policy" },
  "scopes": {
    "path": "/v1/programs/{handle}/scopes",
    "items": "/data",
    "fields": { "identifier": "/target", "kind": "/type", "in_scope": "/in_scope", "bounty": "/bounty", "instruction": "/notes" }
  },
  "asset_kinds": { "url": "web", "wildcard": "wildcard", "cidr": "cidr", "android": "mobile", "other": "other" }
}
```

- `auth.kind` is `basic` (a user name and a token; give `user_label`) or `bearer` (a token).
- `items`, `next` and every field are [JSON pointers](https://www.rfc-editor.org/rfc/rfc6901) into the API's answers. `next` may be a full address on the same origin or a path.
- `{handle}` in a path or `program_url` is replaced by the program's handle.
- `asset_kinds` maps the platform's asset types to `web`, `wildcard`, `ip`, `cidr`, `mobile`, `source` or `other`. Only the first four become scope rules.
- `api` must be an `https://` origin with no path. Plain `http://127.0.0.1` is accepted so you can try a pack against a local stand-in.

Check a pack by adding it from a file in the Market (**Add from a file or link**): it is validated in full and shown before it is installed.
