---
plonix_skill: 1
name: check-scope
version: 1.0.0
title: Check scope suggestions
description: Go through the domains Plonix suggests for scope and recommend which belong to the target, with the evidence for each.
author: Plonix contributors
uses: [scope, map]
---
Help the user decide on the domains waiting in Plonix's scope suggestions.

1. Call `get_scope` for the current rules and the suggestions with their evidence.
2. Call `list_hosts` for request counts, and `detected_tech` where it helps tell first-party hosts from third-party services.
3. For each suggestion, recommend one of:
   - **Accept**: it belongs to the target (shared cookies, the target's own certificate, API calls made by the target's pages).
   - **Keep out**: a third-party service such as analytics, ads, fonts or a CDN that only serves static files.
   - **Ask the owner**: it might belong to the target but the evidence is thin.
   Give the evidence in one line for each.
4. End with a short list the user can act on in the Scope screen, accepts first.

You cannot change scope yourself: only the user can accept or reject domains.
