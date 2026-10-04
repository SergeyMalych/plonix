---
plonix_skill: 1
name: triage-host
version: 1.0.0
title: Get to know a host
description: Summarize what a host does, how it is built, how it signs users in, and which areas deserve a closer look first.
author: Plonix contributors
uses: [map, traffic, scope]
argument: host: The host to look at, such as api.example.com
---
Build a short briefing on {{host}} for the user, from what Plonix has already captured.

1. Call `get_scope` and say whether {{host}} is in scope. If it is not, say so first: the user may only see in-scope traffic through you.
2. Call `detected_tech` for {{host}}: list the frameworks, servers and services, with versions when Plonix knows them.
3. Call `list_endpoints` for {{host}}. Group the endpoints by area (pages, API, authentication, uploads, admin, static files) and count each group.
4. Call `search_traffic` with `host:{{host}}` and read two or three representative requests with `get_request`: one page load, one API call, and one request that carries a session or token.
5. Write the briefing:
   - **What it is**: one or two sentences.
   - **How it is built**: the stack, and how the client talks to the server (JSON API, GraphQL, forms).
   - **Sessions**: how a signed-in user is recognized (cookie names, bearer tokens), without copying secret values.
   - **Where to look first**: up to five endpoints or areas, each with one sentence on why, and the request id that shows it.
   - **Gaps**: parts of the app that were never browsed, so the user knows what to open next.

Keep it under 300 words. Refer to requests by their id (`#123`) so the user can open them in Plonix.
