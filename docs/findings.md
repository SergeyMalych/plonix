# Findings

**Findings** (⌘5) are what you found, each with the requests that prove it attached as evidence. Plonix suggests; you decide what becomes a finding.

## Recording one

- When something stands out in the Lens (a leaked secret, a token that still works unsigned or expired, a card number, an open cross-origin policy, a stack trace, a server error), a **+ Finding** button records it with that request as evidence and a title already suggested.
- **Finding** on a Bench send, and **+ Finding** on a [callback](callbacks.md), start one with those requests as evidence.
- **New finding** on the Findings screen starts an empty one.
- Scans add the findings their checks prove, with the requests attached, and [extensions](extensions.md) can propose findings that stay open until you confirm them.

**✦ Write it with Claude** drafts the finding from its evidence: a plain-words title, a severity with the reason, what happens, why it matters, steps to reproduce and the request as curl. Nothing is saved until you press Save.

## Working with findings

Each finding has a title, a severity (info, low, medium, high or critical), a description and a status: **open**, **confirmed**, **false positive** or **fixed**. The list shows open findings first, most severe first. **Edit** changes any of it, and **Delete…** removes a finding after asking (the requests it points to stay in Traffic).

## Reports

**Export ▾** saves the findings as a report in Markdown, a self-contained HTML page or JSON, each finding with its evidence requests and responses (bodies clipped to 4,000 characters). Reports leave false positives out unless you ask for them.

## From the terminal

```sh
plonix findings                            # most severe first
plonix findings add 'IDOR on /api/users' -s high -r 42,43
plonix findings show 1                     # one finding with its evidence requests
plonix findings edit 1 -d @notes.md        # or --title, --severity
plonix findings status 1 confirmed         # open, confirmed, false-positive, fixed
plonix findings export -o report.html      # or .md, .json
```

Agents can read findings and export a report, but cannot record, edit or delete them.
