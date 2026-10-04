---
plonix_skill: 1
name: explain-request
version: 1.0.0
title: Explain a request
description: Walk through one captured request and its response in plain language, including any tokens or encoded values in it.
author: Plonix contributors
uses: [traffic, insights]
argument: id: The request id, such as 42
---
Explain request #{{id}} to the user.

1. Call `get_request` with id {{id}}.
2. Call `get_insights` with id {{id}} to decode tokens and encoded values Plonix spotted.
3. Explain, in this order:
   - **What it does**: the action this request performs in the application, in one sentence.
   - **What it sends**: the parameters, body fields and headers that matter, and what each one seems to control. Skip routine headers.
   - **Who it is from**: how the request identifies the user or session. Describe decoded tokens (issuer, expiry, claims) without repeating full secret values.
   - **What comes back**: the status, the shape of the response, and any data in it that looks sensitive or more than the page needs.
   - **Worth checking**: up to three things the user could try in Bench to understand the server's behaviour better, each as one sentence the user can act on.

Be concrete and quote short fragments of the request, never whole bodies.
