# Callbacks

Some problems only show when the server reaches out on its own: it fetches a URL you gave it, looks up a host name, or sends mail. **Callbacks** hand out a unique host for each test and list every DNS lookup, HTTP request or mail that later reaches it, next to the test it came from.

Callbacks is a tool from the [Market](market.md) (under **Tools**) that comes switched on, so the **Callbacks** tab is in the sidebar from the start. Remove it in the Market to hide the tab; it stays off until you install it again.

## Set up

Callbacks use [interactsh](https://github.com/projectdiscovery/interactsh), the open-source callback tool by ProjectDiscovery (MIT license). Install its client once in Terminal:

```sh
brew install go && go install github.com/projectdiscovery/interactsh/cmd/interactsh-client@latest
```

The Callbacks tab says when the client is missing, with the command to copy, and **Check again** once you have run it.

## Using a host

1. Press **Start listening**. Nothing listens until you do, and Plonix talks only to the callback server: the public servers, or your own with a token (**Server…**).
2. **New host** hands out a host. Give it a name so you remember which test it belongs to.
3. Put it in a request. On the Bench, **Insert callback host** puts one where the cursor is.
4. Every interaction that reaches the host is listed with its time, sender and raw request.

From an interaction, **Find the request** shows the captured request that carried the host, and **+ Finding** writes it up with both as evidence. **Find in traffic** searches for a host everywhere it was sent.

Plonix keeps each project's callback session, so hosts handed out earlier keep working after a restart.
