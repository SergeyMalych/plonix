---
plonix_skill: 1
name: review-sign-in
version: 1.0.0
title: Review sign-in and sessions
description: Find the sign-in, sign-out and session requests in captured traffic and describe how the application keeps users signed in.
author: Plonix contributors
uses: [traffic, insights, map]
optional_argument: host: Only look at this host
---
Describe how users sign in and stay signed in. Host to focus on: {{host}}.

1. Use `search_traffic` to find the authentication flow: try `is:auth`, then free text such as `login`, `signin`, `token`, `oauth`, `session` and `logout`. Add `host:` when a host was given.
2. Read the key requests with `get_request` and `get_insights`: the sign-in request, its response, the first request that uses the new session, and sign-out if it was captured.
3. Report:
   - **Flow**: the steps from the sign-in page to a signed-in request, each with its request id.
   - **Session**: what carries the session (cookie or token), its lifetime if visible, and cookie attributes (Secure, HttpOnly, SameSite).
   - **Tokens**: for decoded tokens, the algorithm, issuer, audience and expiry, without full secret values.
   - **Sign-out**: whether it was captured, and whether the session appears to end on the server.
   - **Questions to settle**: up to five specific things about this design that the user should confirm by hand in Bench, each with the request id to start from.

If no sign-in was captured, say so and tell the user to sign out and back in while Plonix is capturing.
