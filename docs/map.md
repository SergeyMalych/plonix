# The Map

The **Map** (⌘4) lists the hosts in scope, the technologies behind them and every endpoint seen on each, with its statuses and parameters. It is how you see the shape of a target after browsing it.

## Hosts

The list on the left shows the hosts you accepted on [Scope](scope.md). Hosts that are not in scope are left off; a **N not in scope** link at the bottom opens Scope to accept them.

- **Order.** **Recent** (newest traffic first, the default), **Requests** (busiest first) or **A–Z** (by domain, then host).
- **Hide a host.** Hover a host and press ✕ to take it off the Map. Its traffic is kept. **N hidden** at the bottom lists the hosts you hid, with **Show on Map** to bring one back.

The order and hidden hosts are saved with the project.

## A host

Select a host to see:

- **Technologies.** Servers, frameworks, content management systems, CDNs and firewalls, identity providers and admin consoles, each with the evidence behind it, down to the request. Detection comes from [rule packs](detection-rules.md) that anyone can write, and new rules apply to traffic you already captured.
- **Endpoints.** Every path seen on the host, with its methods, statuses and parameters. Numbers in paths fold into `{id}` and long opaque segments (signed links, session tokens, JWTs) into `{token}`, so every link to the same resource is one endpoint, and long paths stay on one line with the resource name readable.
- **Not visited yet.** When an API description (OpenAPI or Swagger) shows up in traffic, the Map lists the endpoints it describes that nobody has requested yet, each one click from the [Bench](bench.md).

Sort the endpoint table by clicking a column title, resize a column by dragging its edge, and filter it with plain words or `method:`, `path:`, `status:` and `param:` terms. **Ask Claude** on a host hands Claude its endpoints and technologies to ask what to look at.

Select endpoints to replay them as each saved user with the [Access check](users.md#the-access-check).

## From the terminal

```sh
plonix hosts                  # every host seen, busiest first
plonix tech                   # technologies on each host, with evidence
plonix tech api.example.com   # one host
```
