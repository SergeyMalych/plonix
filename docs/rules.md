# Rules

**Rules** change traffic as it passes through Plonix, in plain steps. Use them to send your bug bounty handle on every request, swap a token, strip `Content-Security-Policy` from responses, or flip a feature flag in a JSON response.

Rules here change traffic. To teach Plonix to recognise technologies, see [Detection rules](detection-rules.md).

## What a rule does

- **Add header:** send a header on every request, such as `X-Bug-Bounty: yourname`.
- **Change header:** set a header to a new value.
- **Remove header:** leave a header out.
- **Replace text** in the request line, headers or body, or in the response headers or body. Match literal text or a regular expression; `$1` puts back what a group matched.

## Where it applies

Each rule says which traffic it changes: traffic from your browser, requests you send from the Bench (and Bench runs and the Access check), and requests Scans send. A rule can be limited to in-scope hosts, and it can have a condition written as a [Traffic search](traffic.md#search), such as `host:api.example.com method:POST`, so it only changes what matches.

## The Rules screen

The Rules screen lists every rule in plain words, with a switch per rule and one switch that pauses them all. **+ Add rule** opens a short form with a preview of what every request will carry. Right-click any header in the Lens to start a rule from it, already filled in. A pill in the Traffic toolbar shows how many rules are on and pauses them in one click.

## What you see afterwards

Each exchange records which rules changed it and the request as it was. Traffic and the Lens mark it **changed**, and the Lens shows it as sent, as it was, or side by side.

## Limits

- Body rules apply to bodies within the body limit (Settings › Proxy › Keep bodies up to). Longer, streaming and event-stream bodies pass unchanged.
- Compressed response bodies are matched decoded and sent uncompressed when changed.
- WebSocket handshakes and messages are not changed.
- Agents cannot read or change rules.

## From the terminal

```sh
plonix replace header add X-Bug-Bounty me --bench --scans
plonix replace add request-header '(?i)^user-agent: .*$' 'User-Agent: plonix' --regex
plonix replace add response-body '"debug":false' '"debug":true' --in-scope
plonix replace                    # the rules in order; or: enable 2, disable 2, rm 2
```

The same rules are at `/api/replace` in the local API.
