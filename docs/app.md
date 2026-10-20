# The Plonix window

Plonix is a native app on macOS and Windows. Double-click it and the Start screen opens; pick or create a project and it opens in a window of its own with capture running. Open more projects and each gets its own window, with its own proxy. Closing a project's window closes its session.

## The sidebar

The sidebar lists the tools, with **Open target** at the top. Under them it shows the scope suggestions waiting on you (accept or reject in one click) and the hosts in scope with their request counts: click one to filter Traffic. **Settings** and **Collapse** stay pinned at the bottom. Collapse the sidebar to icons with ⌃⌘S, or `\` when the window runs in a browser.

Saved users and the Access check ([Users](users.md)) and [Callbacks](callbacks.md) are Market tools that come switched on; remove one in the [Market](market.md) to hide its screen. [Programs](programs.md) ships off and appears once you install it.

## Keyboard shortcuts

On Windows, use Ctrl where the Mac uses ⌘.

| Shortcut | What it does |
|---|---|
| ⌘1 to ⌘9 | Traffic, Bench, Scope, Map, Users, Access, Findings, Agents, Callbacks, top to bottom as the sidebar lists them |
| ⌘O | Open target in the capture browser |
| ⌘N | New project |
| ⇧⌘P | The Start screen |
| ⌘, | Settings |
| ⌃⌘S | Show or hide the sidebar |
| ⌘+ / ⌘− / ⌘0 | Bigger text, smaller text, back to normal |
| ⇧⌘B | Open this window in your web browser |
| `b` | Open the selected request on the Bench (or double-click it) |
| `i` | Intercept on or off |
| `f` / `d` | Forward or drop the held item (⌘↵ forwards too) |

## Appearance

**Settings › Appearance** applies to every window on this computer:

- **Style.** **Studio**, the standard Plonix look: cream paper, a blue sidebar, round controls and a shape for every tool. **Classic** is the plainer indigo look. Both work in light and dark and with both spacings.
- **Theme.** Match the system, light or dark.
- **Spacing.** **Dense**, the default, fits more on screen. **Roomy** gives rows and panels more air.
- **Text size.** From 80% to 150%. ⌘+ and ⌘− change it from anywhere and ⌘0 puts it back; it is also in the View menu. The window grows its smallest size with the text, so nothing gets cut off.

A change reaches every open window at once.

## In your web browser

The same window runs in any browser. **Settings › Interface › Open projects in: My web browser** opens projects there instead, View › Open in Browser (⇧⌘B) does it for one window, and `plonix ui` opens the current project from a terminal. The page signs in through a one-time link, so the API token never appears in a URL.

## The command line

**Plonix › Install Command Line Tool…** puts the `plonix` command on your PATH, linked to the tool inside the app so it updates with it. `plonix` commands talk to the same sessions while the app is open (`-p` picks the project), and projects started from a terminal show up on the Start screen. See [Setting up capture on any Mac](setup.md#the-plonix-command-from-the-app).
