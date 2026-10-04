---
plonix_skill: 1
name: api-inventory
version: 1.0.0
title: Inventory an API
description: List every API operation seen on a host with its parameters, authentication and data returned, as a table the user can work through.
author: Plonix contributors
uses: [map, traffic]
argument: host: The API host, such as api.example.com
---
Build an inventory of the API on {{host}} from captured traffic.

1. Call `list_endpoints` for {{host}}.
2. Group paths that differ only by an id (`/users/12` and `/users/31` are `/users/{id}`).
3. For each operation, read one example with `get_request` and note:
   - method and path template;
   - parameters (query, path, body fields) with their apparent type;
   - how the caller is identified (cookie, bearer token, none);
   - what the response returns, in a few words;
   - the example request id.
4. Present it as a Markdown table, sorted by path. Below it, list:
   - operations that were seen without any authentication;
   - operations that take an object id, since access to other users' objects is worth checking by hand;
   - API documentation found (OpenAPI, GraphQL introspection), with its request id.

Do not guess operations that were not captured; list the parts of the app the user should browse to fill the gaps.
