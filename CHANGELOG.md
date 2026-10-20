# Changelog

What changed in each Plonix release. The newest release is at the top.

## [Unreleased]

### Added
- **Claude suggests requests to try.** Besides editing a Bench draft, Claude can now suggest a request of its own (the `propose_request` MCP tool): it waits in a strip above the Bench tabs with what it tests, **Open on the Bench** and **Discard**. Nothing is sent until you press Send.
- **Agents see saved users and the Access check.** `list_users` names the users you saved, with which headers and cookies each carries but never their values, and `get_access_check` reads your last Access check. Both can be switched off in Settings › AI agents.
- **Users and the Access check in the terminal.** `plonix users`, `plonix access <ids>` or `plonix access --host … --prefix …` (a table with the rows worth a look marked), and `plonix replay <id> --as <user>`.
- **Callbacks without Go.** One click (or `plonix callbacks install`) downloads interactsh-client from its GitHub release, checks its checksum and keeps it with your Plonix data. The new `plonix callbacks` command starts, stops and lists callbacks and makes hosts.
- **Two new walkthrough stops**: the Access check and Callbacks.
- **A one-time "Star Plonix on GitHub" card.** It shows only from your third day of use, right after you save a finding, finish an Access check or save a report (after a week, a Bench send that comes back counts too). Never on first launch, in the demo, or over a dialog. **Maybe later** asks again in two weeks, once; **No thanks** or a star ends it. Counted on your computer only.

### Changed
- **A new era of web assessments.** The website and README lead with what Plonix is for, on macOS and Windows alike, with a phone menu and fresh window screenshots.
- **The Access check leaves out requests that change data** (POST, PUT, PATCH, DELETE) when it checks a branch, unless you tick **Also requests that change data**, and says which it left out. Rows worth a look come first; pages that answered everyone the same are marked Public and go last.
- **Reports mask credentials.** Exported findings show cookie, token and auth header values as `abcd…[redacted]`. Tick **Keep cookie and token values** in the Export menu (or `plonix findings export --keep-secrets`) for a private copy.
- **A calmer sidebar.** A new project shows the everyday screens; **More tools** adds Callbacks, Scans, Market, Programs and Rules (the demo shows everything).
- **Common third parties are kept out of scope** on a new project without asking, with a note saying where to change it.
- The Start screen keeps **Start your first project** and the profile picker until you have a project of your own, with the demo listed under it.
- `plonix market install` shows what an extension may do and asks before switching it on; `--yes` skips the question in scripts.
- Keyboard hints read Ctrl on Windows.

### Fixed
- **The Map, `list_endpoints`, the Access check and Ask Claude keep every endpoint** after a big run or scan; they used to see only a host's newest 20,000 requests.
- **Fewer CORS false alarms.** `Access-Control-Allow-Origin: *` is no longer flagged as reflecting your origin (browsers refuse it with credentials), and origins on the same site are not flagged.
- Ask Claude shows only Plonix steps in its progress line, not Claude Code's own housekeeping.
- The `Proxy-Connection` header is no longer recorded, so replays don't send it.
- A scope rule for an IP address no longer claims subdomains.

## [0.5.1] - 2026-10-14

The release after 0.1.5. The version jumps to 0.5.1; nothing is skipped in between.

### Added
- **Users found in traffic.** The Users screen lists the sessions in captured traffic, one per person, with each cookie and token and the part of the app it is sent to (domain, host, path). Add one to the saved user it belongs to, or save it as a new user; the Lens and the Traffic row menu offer the same. Saved users can now send a header only to one host or path, so one user carries different tokens for different parts of an app.
- **Docs for every tool** on [plonix.io/docs](https://plonix.io/docs): Getting started, the Plonix window, Traffic and the Lens, Scope, the Map, Rules, Saved users and the Access check, Callbacks and Findings, in the Studio look with Try classic. Every page of the website shares one menu: Home, Docs, Analytics and Privacy. Tables fit their content and turn into cards on phones.
- **Is a user still signed in?** Each saved user shows whether its session works, is about to run out, has expired or was refused by the server, read from the expiry its tokens and cookies state and from how the server answered requests sent as that user. It says when it can't tell, and **Check now** asks the server once. **What it may do** lists the roles, scopes and groups the user's tokens and "who am I" answers name, and the endpoints that allowed or refused it. The walkthrough has a stop for it.
- **Stop for the traffic watcher.** While Claude looks at your traffic, the Agents inbox shows Stop: it ends the look at once, Claude Code and everything it started included. Turning off Watch my traffic, or agent access in Settings, stops a look in flight too.
- **Your own daily token limit.** Pick Custom… under the inbox and type a limit such as 350k or 1.5M (10k to 10M). The default stays 200k a day.

### Changed
- **Users, Access check and Callbacks are on from the start.** Their screens are in the sidebar on a new install and after updating, with no trip to the Market. Each is still a Market tool: remove it there and it stays off. Callbacks still asks you to install its helper once before it can listen; nothing is downloaded on its own.
- **Pretty | Raw for requests too.** One switch lays out requests and responses alike: the Lens shows it on both sides (JSON indented, forms one field per line), and the Bench response, run results and Compare follow it. The Bench request has its own Pretty | Raw that lays out a JSON body in the editor.
- **Resizable sidebar.** Drag its right edge to set its width (double-click resets). Dragged narrower than its names, it folds to icons; drag it back out to see them again.
- **Spotted and decoded values** keep their header and buttons (Copy decoded, Copy value, Find in traffic) in view while only the value scrolls, with a handle underneath to drag its height. The same goes for value cards in the Bench Lens.
- **Bench History and Compare** each have a handle underneath to drag their height.
- **Website**: one menu on every page (Home, Docs, Analytics, Privacy), download buttons that offer the Mac or Windows app to match your computer, docs tables that fit on a phone, and a [Code signing policy](https://plonix.io/docs/code-signing.html) page for the coming signed Windows installer.

### Fixed
- **The watcher stays within its daily limit.** Every call Claude makes is counted now, not just the last one, so the meter shows what a look really uses (often two to three times more than before). A look is not started when too few tokens are left for it, and one that would go past the limit is stopped before its next call.
- **Callbacks keep listening.** The listener comes back on when a project it was on for reopens, making a host starts it, and the tab says when callbacks can't be fetched instead of showing Listening.
- **Pretty on the Bench** only re-spaces JSON, so numbers such as `12345678901234567890` or `1.0` stay exactly as written. Form bodies no longer get the switch, since it put line breaks into the values sent. The Lens and Compare keep numbers exactly as captured in Pretty too.
- **Agents** limited to in-scope traffic no longer see other hosts in the Traffic filter counts.
- **Saved users** ignore a Set-Cookie for a domain other than the site that sent it, and accept hosts with `_` in scoped headers and cookie domains. Keeping a session found in traffic no longer fails on a long name or many tokens, and a header row with a path but no host waits for the host instead of showing an error.
- The Lens no longer reads all traffic again to find sessions each time it opens a request.
- **Scans**: a `%` before an accented character in a query no longer stops a scan, and check names are no longer squeezed by the "needs" column.
- **Bench**: sending to `https://user:pw@host:8443/` uses port 8443.
- Saving extension state or settings at the same moment no longer loses one of the changes.
- Saved panel heights (spotted values, Bench History and Compare) shrink to fit a smaller window.
- In small windows, finding titles and your note get room, and the tour's progress dots stay on one line. The walkthrough's report preview is styled.
- Links in text use the accent colour, the Callbacks install command is left-aligned, and the MCP search help shows filter values that work as typed.

## [0.1.5] - 2026-10-11

### Added
- **Text size.** ⌘+ and ⌘− (Ctrl+ and Ctrl− on Windows) make text bigger or smaller, from 80% to 150%, and ⌘0 puts it back to 100%. The same size applies to every window, the Start screen included, and stays for next time. It is also in View and in Settings › Appearance › Text size.
- **Lens in a window of its own**: the Lens can fill the window (press `e`) or open in a separate window.
- **Usage statistics, if you want them**: statistics are now off until you tick them, Settings shows the next report before it is sent and can reset the install id, and the totals everyone shares are public at [plonix.io/analytics](https://plonix.io/analytics).

### Changed
- **Agents**: a lead opens in place on the left, and the right column switches between **Claude** (ask and earlier conversations) and **Traffic** (what agents read). Lead rows show a solid kind tag, the request as a chip, and Unread and Earlier groups, and the token meter fills its row.
- **Map** lists only hosts in scope, with a link to Scope for the rest. Any host can be hidden with ✕ (its traffic is kept, and hidden hosts can be shown again), and the list can be ordered by Recent, Requests or A–Z. Both are saved per project.
- **One look for every window**: theme and spacing are shared by the Start screen and every project window, and the Start screen has its own theme button and Appearance settings.
- **Studio** uses green as a full fourth colour, in the app and on the website.
- **Market** shows a chip for every kind in the catalog, Tools included, and tools and detectors have their own icons.
- Filter suggestions take one row, with **More…** and **Less**. The Map's host list can be resized by dragging (double-click resets).

### Fixed
- **Ask Claude on a fresh Mac install**: when Plonix is opened from the Finder or the Dock it now finds Claude Code installed in `~/.local/bin`, Homebrew or an npm folder, and says why the button is grayed out when Claude Code is missing, with a way to check again.
- **Mac: local network targets.** On macOS 15 and later Plonix now asks for Local Network access when it starts, so routers, LAN servers and devices load through Plonix. If access was turned off, connections to them say where to allow it: System Settings › Privacy & Security › Local Network.
- **Servers that never finish the TLS handshake** fail fast with a message that says where it stalled, instead of waiting out the whole request timeout. Every address a host resolves to is tried in turn, alternating IPv6 and IPv4, and a request timeout names the host and the setting to raise.
- Closing the demo strip no longer hides the walkthrough for good. The strip, with **Take the tour**, comes back the next time the demo opens.
- Market kind shapes sit in the middle of their circles, and the open-project dot on the Start screen is no longer clipped.

## [0.1.4] - 2026-10-10

### Changed
- **Studio is now the default look** for the app and the website. In Studio each kind of Market item has its own flat shape, and the app ships with a red, yellow and blue Studio icon. Classic is one click away in Settings › Appearance › Style, or "Try classic" on the website, and picking a style switches every open window.
- **Small windows**: Open target, Settings and Collapse stay visible in the sidebar, the Lens shrinks to fit, toolbars wrap instead of cutting off buttons, and Traffic gives the Path column room below 1180px.
- **Map**: the host list looks like the Users list.
- **Website**: every app screenshot and the tour video have a Studio version.
- **Demo walkthrough**: the card sits beside the part it explains instead of on top of it, stays put unless that part moves, and can be dragged by its header. **Test now!** does the work for real on every stop that has one: it saves the finding, sends the changed request, opens and sends an endpoint from the Map, runs a Quick scan, previews the report, or installs and runs an extension, then says what came back. The demo strip keeps only **Take the tour**.

### Fixed
- The capture browser no longer shows a yellow "unsupported command-line flag" bar. HTTPS capture works as before.
- **Bench**: Send no longer sends the • position marks along with the request.

## [0.1.3] - 2026-10-09


### Added
- **Plonix for Windows**: `Plonix-Windows-setup.exe` on every release, for 64-bit Windows 10 and 11, with in-app updates like on the Mac. It is not code-signed yet, so Windows SmartScreen may ask before it runs: click **More info**, then **Run anyway**. On Windows Plonix keeps its data in `%USERPROFILE%\.plonix`, opens targets in Chrome, Edge, Brave or Firefox (or downloads the Plonix browser), adds its certificate to your own trusted root certificates when Firefox needs it, and opens Claude Code in PowerShell. Engines started with `plonix start` keep running after you close the terminal, and no console windows pop up.
- **Studio style**: an optional geometric look for the app (Settings › Appearance › Style) and the website. Classic stays the default.
- **Scans: passive checks** over traffic you already captured, with no new requests: insecure session cookies, permissive CORS with credentials, open-redirect reflection, verbose server errors, exposed source maps and version disclosure.
- **Scans: Quick and Thorough** depth presets, and a count of how many requests a scan will send before you run it.
- **Scans: every request a scan sent** is listed in its report and can be opened in the Lens or filtered in Traffic (`source:scan`).
- **Scans: file-exposure checks** for backups, actuator endpoints, keys and dumps, with a soft-404 fingerprint so pages that answer everything do not raise false alarms.
- **Mind Reader**: every suggestion shown on the website now shows up in the app, each with a walkthrough stop.

### Fixed
- **New Project** on the Start screen did nothing.

## [0.1.2] - 2026-10-09

### Added
- **Test now!** on the demo walkthrough's stops: one press shows that part working for real on the demo's data, with a pointer that moves and clicks for you. You get another customer's order back from the Bench, two orders compared side by side, a run through 22 order ids, the same request sent as Maya and then as Dana, an access check as each saved user and signed out, and more. New stops for grouping, Ask Claude and comparing requests. Everything is answered locally by the demo's stand-in API.
- **Open a browser as a saved user**, from the Users screen: a browser window of that user's own, with its own cookies, so you can sign in as Dana there and stay signed in as Maya in your usual capture browser. It starts with the cookies saved for the user, and what you do there is recorded as sent by them. The session it uses is saved for the user as you go: cookies the site sets, cookies set from JavaScript, and tokens such as a bearer header, so the Bench, the Access check and Scans send the same.
- **Act as a saved user** from a pill in the title bar: browser traffic to in-scope hosts, Bench tabs and Scans are then sent with that user's cookies and headers, and cookies the server sets for them stay with the user, so your browser's own session is untouched. A **Users** screen lists every user and cookie to edit, expire, restore or remove, or paste from a Cookie header. Saved users from 0.1.1 keep working.
- Callbacks in the Market: a Callbacks tab that makes a host for each test (or inserts one on the Bench) and lists every DNS lookup, HTTP request or mail that reaches it, so you can see when a server makes a call of its own. It drives `interactsh-client`, which you install yourself: `brew install go && go install github.com/projectdiscovery/interactsh/cmd/interactsh-client@latest`.
- Rules have their own screen: add, change or remove a header, or replace text, in plain steps with a preview. Each rule says where it applies (Browser, Bench, Scans), can be limited to in-scope hosts, and can have an *Only when* condition written as a Traffic search.
- A community Market next to the official one, with badges for **Official**, **Community** and **Your own**. Add your own packages from a GitHub repository (`github:owner/repo`, its latest release or `@tag`), a folder (with **Read again**), a file or a link, and block anything you never want offered.
- Every Market package page says what it does and how to use it, with its actions one click away (such as **Find subdomains**). An extension that needs your yes can be allowed later with **Allow** or `plonix extensions allow <name>`.
- **Full path / Short path** in the Traffic toolbar: Short folds ids and tokens into `{id}` and `{token}` and hides the query behind a dim `?…`.
- Scans can aim at the whole host, **a path** or **a path group** (every endpoint under a prefix such as `/v1/`, with a live count), and crawl results name their host.
- New built-in scan checks, each one read-only request that flags an exposure only when it is there: an enumerable GraphQL schema, an exposed `.git/HEAD`, OpenAPI and Swagger descriptions, directory listings, `phpinfo()` pages, and enumerable WordPress users and XML-RPC. The GraphQL quick action opens the schema check.

### Changed
- Programs is now a tool you install from the Market, like Saved users and the Access check, so the sidebar shows it only once you want it. A project that already follows a program keeps the Programs screen and its rules, and the demo project still shows it. Bug hunters see it in *Recommended for you*.
- A tool's screen appears in the sidebar as soon as you install it.
- Official extensions install ready to run; others ask for a tick first.
- Access check and the parameter probe no longer run under a program that bans automated testing, like scans, crawls and Bench runs.
- Agents limited to in-scope data no longer see findings whose evidence is all out of scope.
- `plonix skills add`, `rules add` and `filters add` now show what the package is and does, and add it only with `--yes`, like `market add`.
- An `agents.json` this build can't read now means agent access is off until you save the settings again; the old file is kept aside.
- The app's code is split by screen and feature (UI scripts, local API and engine), so each part is easier to find and change.

### Fixed
- The demo walkthrough's card and its Back and Next buttons stay inside the window at every stop, also in a small window and after Test now! adds its result. A toast no longer covers the buttons.
- Market downloads work with an empty `HTTPS_PROXY`; every download follows one proxy rule and honours `NO_PROXY`.
- The Callbacks install command now works (there is no Homebrew formula for it).
- The reinstall hint for detector packs, lists and tools points at `plonix market add`.

## [0.1.1] - 2026-10-08

### Added
- Suggestions for your work: pick bug hunter, red teamer or security researcher on the Start screen and get a starter set from the Market, each item with one line on why. The Market shows *Recommended for you* at the top, with a switch to look at another kind of work; Settings › Market and each project's settings keep the choice. CLI: `plonix market profile`, `plonix market recommend`, `plonix market install --starter`.
- Ask Claude shows its progress while it works: the current step, tokens read and written, elapsed time and the answer as it is written, formatted (headings, lists, tables, code blocks with Copy). A run that goes quiet says so, and is stopped with a clear message instead of hanging.
- Claude writes findings: one click fills in a plain-words title, severity with the reason for it, what happens, why it matters, steps to reproduce and the request as curl. Nothing is saved until you press Save.
- Copy any request as a curl command, from the Lens, the Traffic right-click menu and the Bench.
- The Lens suggests next steps for the open request: record a finding when something stands out (a leaked secret, an unsigned or expired token that still works, a card number, many people's emails, a stack trace, a server error), and ask Claude for ideas on the endpoint.
- The Lens spots stack traces in responses.
- API descriptions (OpenAPI and Swagger) found in traffic: the Map lists the endpoints nobody has visited yet, each one click from the Bench.
- When a Bench send comes back logged out but worked before, Plonix offers the newest login captured for that host, in that tab or in every tab still on the old one.
- Traffic folds the same request sent several times in a row into one row, with the count and the time from first to last. Click it to see each one; *Every request* in the toolbar shows each request on its own row.
- Select any text in the Lens to decode it (JWT, URL-encoding, Base64, hex), find it in traffic or ask Claude about it.
- On the Bench, a Spotted value (a decoded JWT, URL or Base64 value) opens large enough to show all of it, and History shows about a dozen sends. Drag the top edge of either to resize; double-click it to fit again.
- secret-sweep in the Market: finds API keys, tokens and passwords for hundreds of services in captured traffic and shows each one in the Lens. It runs trufflehog, which you install yourself (`brew install trufflehog`), on your Mac only.
- js-endpoints in the Market: reads captured JavaScript and pulls out the API paths and URLs it references, so endpoints nothing has visited yet stand out in the Lens.
- subdomain-discovery in the Market: enumerates an accepted domain's subdomains with a recon tool you install (`subfinder`, with another installed tool as a fallback) and adds the ones it finds to Scope as suggestions to review. It reads public sources, never touches the target, and never changes scope on its own.
- parameter-probe in the Market: probes one in-scope endpoint for undocumented query parameters. Plonix sends a bounded set of candidate names itself, through the same scope-gated, recorded path as replay, and proposes one unconfirmed finding for any that change the response. Nothing is installed and no outside program sends.
- security-headers in the Market: a small passive extension that notes HTML pages missing common security headers and cookies set without Secure or HttpOnly, and proposes one finding per host. A starting point for writing your own.
- Program extensions now come in kinds — scan (over captured traffic), enumerate (subdomains into scope suggestions) and probe (candidate inputs through the scope choke point) — and a new `suggest-scope` capability lets one contribute scope suggestions without ever changing scope.
- A walkthrough of the demo project: the first time the demo opens, it offers a short tour that steps through every part of Plonix over the demo's own data, ringing each part on screen. Skip it, or take it again any time from **Take the tour** in the demo strip.
- Quick actions in the Lens *Suggested* row and the Traffic row menu: the one next step that fits the request on screen, such as Save login as a user, Check this id across users, Replay signed out, open a reflected value, redirect or GraphQL call on the Bench, draft a finding for an open CORS policy, or find other errors like this one.
- Hand-offs into Scans focused on one endpoint: a file upload, an input that looks like a host or URL the server fetches, or any endpoint with inputs opens Scans narrowed to that endpoint with the fitting checks picked. You review and press Run.
- The Agents screen is where you work with Claude: ask about the whole project, start a skill in one click, reopen saved conversations and follow up, and see every read an agent made in plain words. Turn on *Watch my traffic* and Claude leaves a digest, notes and leads as you browse, each with one place to act on it, with an unread count in the sidebar and a daily token limit. Off until you turn it on, and read-only.
- Traffic search suggests fields and this project's own values as you type, with request counts. Tab completes, Enter adds a filter chip.
- Map endpoints: long paths stay on one line, long opaque segments fold into `{token}`, and the table resizes, sorts and filters (`method:`, `path:`, `status:`, `param:`).
- Settings › Appearance: theme and spacing. Dense stays the default; Roomy gives rows and panels more air. Ask Claude buttons and Claude's suggestions wear a rainbow frame, and Scope evidence lines up in columns.
- Accept all and Reject all in Scope list each domain with a checkbox, so you can leave some out.

### Changed
- Ask Claude runs in the app use only Plonix's read-only MCP server, with your other MCP servers and Claude Code's shell, file and web tools switched off.
- ws:// and plain http:// through the capture browser are captured, browser crawls keep to a program's request rate, and Bench and crawl responses keep only the body limit while recording the real size.

### Programs
- A Programs screen (⌘9) and `plonix program`: connect HackerOne, paste a program's policy, or look up a domain's security.txt, then review and follow the program.
- With a platform connected, Plonix pulls every program you can work on, with its assets and rules, and lets you search them all by program or by asset. It tells you when a followed program's scope changes.
- Following a program sets the project's scope from its assets: in-scope ones are accepted, listed exclusions rejected, and IP ranges work as scope rules.
- Its rules apply to every request Plonix sends: at most the program's request rate, the headers it asks for (on browser traffic too), and no scans, crawls or Bench runs when it bans automated testing.
- Bug bounty platforms are a new kind of Market package: declarative and signed, so more platforms can be added without a Plonix release.

## [0.1.0] - 2026-10-05

The first public release of Plonix for Mac.

### Capture
- An intercepting HTTP and HTTPS proxy with its own certificate authority, created on first run.
- One step from nothing to captured traffic: open a target and Plonix starts the proxy, sets the scope and opens a browser with its own profile that trusts Plonix.
- HTTP/2 on both sides, WebSocket messages, and bodies that stream through as the server sends them.
- Intercept: hold requests and responses in flight to edit, forward or drop them. Match and replace rules change traffic as it passes.
- Works on every Mac: with no supported browser installed, Plonix offers to download its own, and trusting the certificate for Firefox is one click.
- Client certificates per host for servers that ask for one, and HAR files in and out of a project.

### Investigate
- Traffic with a search language (`host:api.example.com method:POST status:5xx`), include and exclude filters, filter packs and suggestions drawn from your traffic.
- The Lens decodes what it finds in a request or response: tokens, URL-encoded and Base64 values, personal data, leaked secrets and internal addresses.
- Adaptive scope suggests related domains as you browse, with the evidence for each, to accept or reject one by one or in bulk.
- The Map shows hosts, the technologies behind them, and their endpoints and parameters.

### Experiment
- The Bench: edit a request, send it, branch it and compare responses side by side. Edit decoded values in place.
- Bench runs send a request many times with values from payload lists, including lists from the Market.
- Crawl a host to find endpoints, parameters and forms, and run scope-gated active scans from the Scans screen.
- Crawl JavaScript apps in a headless browser that stays within accepted scope.

### Validate
- Findings with the requests that prove them, which you can edit, confirm, close and export as Markdown, HTML or JSON.

### AI and automation
- A read-only MCP server so Claude Code can see traffic, the map, scope and findings, and Ask Claude from any request, finding or host.
- Skills: playbooks agents follow for a job in Plonix.
- Claude can suggest an edited request on the Bench, shown as a diff you apply or discard. Nothing is sent until you click Send.
- The `plonix` command and a token-protected local API for everything the window does.
- Install Command Line Tool… in the app puts the `plonix` command on your PATH.

### Projects and Market
- Several projects open at once, each in a folder you choose, with a Start screen and Settings.
- The Market: a signed catalog of skills, rule packs, filter packs, payload lists and bundles.
- Sandboxed WebAssembly extensions that analyze traffic and propose findings, without network, file or process access.
- Updates you approve: Plonix asks before it downloads or installs anything.
- A demo project to try every tool without a target.

### Trust and privacy
- You accept the license and terms once, on first launch.
- Anonymous usage statistics (counts of features used, never traffic, hosts or anything you type), which you can turn off on first launch, in Settings, or with `PLONIX_NO_ANALYTICS=1`. See docs/privacy.md.
- Crash reports stay on your Mac; Plonix offers to open a prefilled GitHub issue and never sends anything on its own.
- Documentation at plonix.io/docs.

[0.5.1]: https://github.com/SergeyMalych/plonix/compare/v0.1.5...v0.5.1
[0.1.5]: https://github.com/SergeyMalych/plonix/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/SergeyMalych/plonix/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/SergeyMalych/plonix/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/SergeyMalych/plonix/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/SergeyMalych/plonix/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/SergeyMalych/plonix/releases/tag/v0.1.0
