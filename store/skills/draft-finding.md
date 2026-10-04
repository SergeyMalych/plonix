---
plonix_skill: 1
name: draft-finding
version: 1.0.0
title: Draft a finding report
description: Turn an observed issue into a clear, reproducible finding write-up the user can review and record in Plonix.
author: Plonix contributors
uses: [traffic, findings]
argument: issue: What the user observed, in a sentence
optional_argument: ids: Request ids that show it, such as 12, 15
---
Draft a finding for: {{issue}}
Requests that show it: {{ids}}

1. Read each request with `get_request`. If no ids were given, use `search_traffic` to find the requests that show the issue and confirm them with the user.
2. Call `list_findings` and check this is not already recorded. If it is, say which finding and stop.
3. Write the draft:
   - **Title**: specific and short.
   - **Severity**: low, medium, high or critical, with one sentence on why.
   - **Summary**: what is wrong and who is affected, in two or three sentences.
   - **Steps to reproduce**: numbered, each referring to a request id the reader can open in Plonix.
   - **Evidence**: short quoted fragments of requests and responses. Mask secrets and personal data.
   - **Fix**: what the developers should change.

You cannot record findings yourself. Give the draft to the user so they can add it in the Findings screen.
