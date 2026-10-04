# Agents and MCP

Plonix lets an AI agent work with your live project: the traffic you captured, the map of the target, detected technologies, scope and findings. It does this through MCP, the protocol coding agents such as Claude Code use for tools.

Agents get **read-only** access. They can look at everything the project holds and cannot send requests, change scope or record, edit or delete findings. The engine enforces this, not the agent.

## Connect Claude Code

```sh
cargo install --path crates/plonix-cli   # once, if you don't have the `plonix` command yet
plonix connect claude
```

```text
Plonix · connect Claude Code

  ✓ Agent token  ~/.plonix/agent-token  (read-only)
  ✓ Claude Code  added MCP server "plonix" for all your projects: /Users/you/.cargo/bin/plonix mcp

What the agent can do (read-only):
  status · search_traffic · get_request · get_insights · list_hosts · list_endpoints · detected_tech · get_scope · list_findings · findings_report
Not allowed:
  ✗ Send or replay requests
  ✗ Accept, reject or remove scope rules
  ✗ Record, edit or delete findings
  ✗ Open browsers, sign in to the window or stop the engine
```

`plonix connect claude` runs `claude mcp add-json` for you. Options:

- `--scope user` (default) makes Plonix available in every project, `--scope local` only in the current directory, `--scope project` writes a shared `.mcp.json` in the current directory.
- `--print` changes nothing and prints the command and the `.mcp.json` entry to add yourself. This is also what you get when the `claude` command isn't installed.

Running it again replaces the earlier entry, so it is safe after moving or upgrading Plonix.

Then start capturing (`plonix open <target>`, or open Plonix.app) and ask Claude Code, for example:

> Use Plonix to find in-scope API endpoints that returned errors, then read the most interesting request and tell me what stands out.

> Using Plonix, list every endpoint on the target that takes an id parameter and group them by host.

> Look at Plonix scope suggestions and explain which ones really belong to the target and why.

The **Agents** screen in the Plonix window (⌘6) shows which agents are connected, what each one has asked for, and what they are allowed to do.

## Other MCP clients

Any MCP client can run Plonix as a stdio server:

```json
{
  "mcpServers": {
    "plonix": { "type": "stdio", "command": "plonix", "args": ["mcp"] }
  }
}
```

Set `PLONIX_HOME` in the server's environment if your data is not in `~/.plonix`.

## Tools

| Tool | What it returns |
| --- | --- |
| `status` | Project, capture counts, scope rules, pending suggestions, proxy address |
| `search_traffic` | Captured requests matching a query (the same search language as the window and `plonix search`), newest first, with paging |
| `get_request` | One request and its response in full, with decoded bodies (clipped to `max_body_chars`) |
| `get_insights` | What stands out in a request: decodable tokens such as JWTs, personal data, secrets |
| `list_hosts` | Every host seen, with request counts and scope decision |
| `list_endpoints` | Methods, paths, statuses and parameter names seen on a host |
| `detected_tech` | Technologies detected per host, with confidence and evidence |
| `get_scope` | Scope rules and suggested domains with their evidence |
| `list_findings` | Findings with severity, status, description and evidence request ids |
| `findings_report` | The findings as a Markdown report with their evidence requests and responses (bodies clipped); false positives left out unless asked for. Evidence on hosts outside scope is left out when agents see in-scope traffic only |

Every tool is marked read-only in its MCP annotations.

## Ask Claude Code from the app

The Plonix window has an **Ask Claude** button on a request (the Lens), a finding, a host (the Map) and a scope suggestion. It opens a sheet that:

- writes a question suited to that spot, which you can edit;
- shows exactly what will be shared, split into named parts (request, response, what Plonix spotted, technologies, endpoints, scope evidence), each with its size, and lets you untick any part;
- clips each request and response body, and estimates the total size against your limit;
- warns, and makes you confirm, when the context is larger than your limit, so a huge payload is never sent silently.

**Copy prompt** puts the prompt on your clipboard. **Open in Claude Code** writes the prompt to a private file under `$PLONIX_HOME/claude/` and opens Claude Code in a new Terminal window reading that file, so captured text never goes on a command line. If the Plonix MCP server is connected in that session, Claude Code can follow up with the read-only tools.

## Settings

The **Agents** screen has a Claude Code settings section (`$PLONIX_HOME/agents.json`):

- **On/off.** Turn agent access off and every agent request is refused (`agents_disabled`).
- **What agents can see.** *In-scope hosts only* (the default) limits traffic, hosts, endpoints and technologies to hosts you accepted into scope; *Everything captured* includes out-of-scope and third-party traffic.
- **Tools agents get.** Switch off groups of capabilities (captured requests, insights, the map, scope, findings). A switched-off capability is refused (`capability_off`) and its MCP tools disappear from `tools/list`.
- **Ask Claude.** The context-size limit that triggers the warning, and how far each body is clipped.

Agents can read this policy (to explain a refusal) but can never change it: the settings route is in no mode's capability list.

## How access is enforced

- `plonix mcp` signs in to the engine's local API with its own token, `~/.plonix/agent-token` (mode `0600`). It never reads the full API token.
- The engine checks every request made with the agent token against a fixed list of allowed routes (`crates/plonix-core/src/access.rs`). Today that list contains only reads. Anything else (`/api/send`, `/api/replay`, `/api/scope/*`, `POST /api/findings`, `/api/ui/launch`, `/api/browser/open`, `/api/shutdown`) is refused with `403 agent_not_allowed`, and the refusal is shown on the Agents screen.
- The API stays loopback-only. Plonix never sends captured traffic anywhere; the agent reads it on your machine.
- The data-scope and capability settings are applied in the same middleware, so an agent sees only what you allow whatever it asks for.

Captured traffic can include passwords, session cookies and API keys. Whatever the agent reads becomes part of its conversation, so connect only agents you trust with that data.

## Later: an opt-in active mode

The access model is built to grow one step, without loosening anything that exists today:

1. A new `AgentMode::Active`, switched on by you in the Agents screen or the CLI, stored per project. An agent can never switch it on itself.
2. In that mode only, `POST /api/send` and `POST /api/replay` join the allowed routes. Both already go through scope enforcement, so an agent could only reach hosts you accepted into scope.
3. Matching MCP tools appear only when the engine reports that mode.

Changing scope, recording findings and controlling the engine stay with you in every mode. Active mode is not built yet.
