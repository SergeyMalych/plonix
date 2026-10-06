# Agents and MCP

Plonix lets an AI agent work with your live project: the traffic you captured, the map of the target, detected technologies, scope and findings. It does this through MCP, the protocol coding agents such as Claude Code use for tools.

Agents get **read-only** access. They can look at everything the project holds and cannot send requests, change scope or record, edit or delete findings. The one thing they can leave behind is a *suggested edit* to a request you are editing on the Bench, which you review and apply yourself (see [Suggested Bench edits](#suggested-bench-edits)). The engine enforces this, not the agent.

## Connect Claude Code

```sh
cargo install --path crates/plonix-cli   # once, if you don't have the `plonix` command yet
plonix connect claude
```

```text
Plonix · connect Claude Code

  ✓ Agent token  ~/.plonix/agent-token  (read-only)
  ✓ Claude Code  added MCP server "plonix" for all your projects: /Users/you/.cargo/bin/plonix mcp

What the agent can do (read-only; a Bench edit is only suggested, for you to apply):
  status · search_traffic · get_request · get_insights · get_messages · list_hosts · list_endpoints · detected_tech · get_scope · list_findings · list_skills · get_skill · findings_report · propose_bench_edit
Not allowed:
  ✗ Send or replay requests
  ✗ Accept, reject or remove scope rules
  ✗ Record, edit or delete findings
  ✗ Open browsers, sign in to the window or stop the engine
  ✗ Install, update or remove anything from the Market
  ✗ See, edit, forward or drop requests held in Intercept
  ✗ Apply a suggested edit to a Bench draft, or start a payload run
```

`plonix connect claude` runs `claude mcp add-json` for you. Options:

- `--scope user` (default) makes Plonix available in every project, `--scope local` only in the current directory, `--scope project` writes a shared `.mcp.json` in the current directory.
- `--print` changes nothing and prints the command and the `.mcp.json` entry to add yourself. This is also what you get when the `claude` command isn't installed.

Running it again replaces the earlier entry, so it is safe after moving or upgrading Plonix.

Then start capturing (`plonix open <target>`, or open Plonix.app) and ask Claude Code, for example:

> Use Plonix to find in-scope API endpoints that returned errors, then read the most interesting request and tell me what stands out.

> Using Plonix, list every endpoint on the target that takes an id parameter and group them by host.

> Look at Plonix scope suggestions and explain which ones really belong to the target and why.

The **Agents** screen in the Plonix window (⌘6) shows what each agent reads as it works, and its **Setup** panel lists which agents are connected and what they are allowed to do (see [The Agents screen](#the-agents-screen)).

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
| `get_messages` | The WebSocket messages sent over the connection a handshake opened, oldest first |
| `list_hosts` | Every host seen, with request counts and scope decision |
| `list_endpoints` | Methods, paths, statuses and parameter names seen on a host |
| `detected_tech` | Technologies detected per host, with confidence and evidence |
| `get_scope` | Scope rules and suggested domains with their evidence |
| `list_findings` | Findings with severity, status, description and evidence request ids |
| `findings_report` | The findings as a Markdown report with their evidence requests and responses (bodies clipped); false positives left out unless asked for. Evidence on hosts outside scope is left out when agents see in-scope traffic only |
| `propose_bench_edit` | Leaves a suggested edit to a Bench draft for you to review: the draft id, a summary and the complete edited request. Sends nothing and changes nothing |

Every tool is marked read-only in its MCP annotations, except `propose_bench_edit`: it stores a suggestion, so it is marked as not read-only, but it is not destructive and never reaches the network.

## Suggested Bench edits

When you ask Claude about a request on the Bench, the prompt carries that draft's id and invites Claude to propose a concrete edit with `propose_bench_edit`. The tool takes the draft id, a short summary and the complete edited request (method, URL, headers, body).

- The engine only **stores** the proposal, in memory, for that draft. It checks that it is a request the Bench could hold (an `http`/`https` URL, a valid method and header names, no line breaks in header values, sane sizes) and keeps a few per draft. It never sends it and never touches the draft.
- The Bench shows it as a diff against your draft as it is now, with **Apply to draft** and **Discard** ([bench.md](bench.md#claudes-suggested-edits)). Applying only changes the draft; sending stays your click on **Send**, through the usual scope check.
- JWTs: Claude has no signing key, so an edited token keeps its original signature and is marked unsigned until you re-sign it in the Lens. A signature Claude made up is replaced with the original.
- Agents can only add a proposal (`POST /api/bench/proposals`). Listing, comparing, applying and discarding are yours (those routes are refused to the agent token), and so are sending and runs.
- Switch it off in **Settings › AI agents** (*Suggest edits to a Bench request*): the tool disappears and the route is refused with `capability_off`.

In the in-app conversation, Claude's proposal shows up as *Suggested an edit to the request*, with **Review on the Bench** once the answer is done. A Claude Code session in a terminal can propose too, as long as you pass it the draft id from the prompt (Copy prompt and Open in Terminal include it).

## Ask Claude Code from the app

The Plonix window has an **Ask Claude** button on a request (the Lens), a finding, a host (the Map), a scope suggestion and a request you are editing on the Bench (where Claude can also [suggest an edit](#suggested-bench-edits)). It opens a sheet that:

- writes a question suited to that spot, which you can edit;
- shows exactly what will be shared, split into named parts (request, response, what Plonix spotted, technologies, endpoints, scope evidence), each with its size, and lets you untick any part;
- clips each request and response body, and estimates the total size against your limit;
- warns, and makes you confirm, when the context is larger than your limit, so a huge payload is never sent silently.

**Ask Claude** runs Claude Code on this Mac and shows the answer in the sheet as it is written, formatted with headings, lists, tables and code blocks you can copy. While it works, a line under the conversation says what Claude is doing (thinking, looking at traffic, writing the answer), how many tokens it has read and written, and how long it has taken. If Claude Code goes quiet, the sheet says so; after two minutes of silence, or five minutes in all, the run is stopped with a message saying why.

**Copy prompt** puts the prompt on your clipboard. **Open in Claude Code** writes the prompt to a private file under `$PLONIX_HOME/claude/` and opens Claude Code in a new Terminal window reading that file, so captured text never goes on a command line. If the Plonix MCP server is connected in that session, Claude Code can follow up with the read-only tools.

## The Agents screen

The Agents screen is where you work with Claude on the project as a whole:

- **Ask box.** Ask about the whole project, such as what to look at next or which hosts belong to the target. Under the box, skills start with one click (a skill that needs a host or a request id asks for it first), and a few starter questions sit next to them.
- **Conversations.** Every question you ask here, or with **Ask Claude** anywhere in the app, is saved with the project. Open a conversation to read it again, or ask a follow-up: Claude picks up the same session, so it remembers what was said. The newest 60 conversations are kept, and you can delete any of them.
- **Activity.** Each request an agent makes appears in a feed in plain words, such as "Request #32", "Endpoints on api.example.com" or "Searched traffic for login". Click an entry to open it in Traffic, the Map, Scope or Findings. Requests an Ask Claude conversation makes appear under that conversation's title. Refused requests are marked. The feed is kept while the project is open.
- **Setup** holds the rest: who is connected, the agent settings, how to connect Claude Code or another MCP client, what agents may do, and the skills. It opens on its own when nothing has been set up yet.

Saved conversations and the activity feed are for you only. Agents cannot read either one.

## Settings

**Settings › AI agents** holds these choices, for all projects (stored in `$PLONIX_HOME/agents.json`, so a change made in one window reaches every open project). The Agents screen shows a summary and a link there:

- **On/off.** Turn agent access off and every agent request is refused (`agents_disabled`).
- **What agents can see.** *In-scope hosts only* (the default) limits traffic, hosts, endpoints and technologies to hosts you accepted into scope; *Everything captured* includes out-of-scope and third-party traffic.
- **Tools agents get.** Switch off groups of capabilities (captured requests, insights, the map, scope, findings, scan advice, suggesting Bench edits). A switched-off capability is refused (`capability_off`) and its MCP tools disappear from `tools/list`.
- **Ask Claude.** The context-size limit that triggers the warning, and how far each body is clipped.

Agents can read this policy (to explain a refusal) but can never change it: the settings route is in no mode's capability list.

## How access is enforced

- `plonix mcp` signs in to the engine's local API with its own token, `~/.plonix/agent-token` (mode `0600`). It never reads the full API token.
- The engine checks every request made with the agent token against a fixed list of allowed routes (`crates/plonix-core/src/access.rs`). Today that list contains only reads, plus `POST /api/bench/proposals`, which stores a suggested Bench edit and nothing else. Anything else (`/api/send`, `/api/replay`, `/api/scope/*`, `POST /api/findings`, `/api/ui/launch`, `/api/browser/open`, `/api/shutdown`, every `/api/intercept` and `/api/replace` route) is refused with `403 agent_not_allowed`, and the refusal is shown on the Agents screen.
- The API stays loopback-only. Plonix never sends captured traffic anywhere; the agent reads it on your machine.
- The data-scope and capability settings are applied in the same middleware, so an agent sees only what you allow whatever it asks for.

Captured traffic can include passwords, session cookies and API keys. Whatever the agent reads becomes part of its conversation, so connect only agents you trust with that data.

## Later: an opt-in active mode

The access model is built to grow one step, without loosening anything that exists today:

1. A new `AgentMode::Active`, switched on by you in Settings › AI agents or the CLI, stored per project. An agent can never switch it on itself.
2. In that mode only, `POST /api/send` and `POST /api/replay` join the allowed routes. Both already go through scope enforcement, so an agent could only reach hosts you accepted into scope.
3. Matching MCP tools appear only when the engine reports that mode.

Changing scope, recording findings and controlling the engine stay with you in every mode. Active mode is not built yet.

## Skills

Skills are playbooks for one job in Plonix (get to know a host, explain a request, draft a finding). `plonix mcp` offers them as MCP prompts, so Claude Code lists them as `/mcp__plonix__<name>` commands, and as the `list_skills` and `get_skill` tools. A skill can only use what these settings allow: when a capability it reads is switched off, agents are not offered it. More skills come from the Market. See [market.md](market.md#skills).
