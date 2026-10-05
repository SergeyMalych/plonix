# Setting up capture on any Mac

Plonix aims to take you from download to captured traffic in under a minute,
whatever is installed on your Mac. This page covers the three things that can
stand in the way and what Plonix does about each: which browser it opens, the
certificate HTTPS needs, and the `plonix` command for the terminal.

## Which browser Plonix opens

**Open target** (⌘O in the app) and `plonix open <target>` open the site in
the *capture browser*: a separate browser window with a profile of its own,
routed through the Plonix proxy. Your everyday browser, its cookies and its
settings are never touched. Plonix picks, in order:

1. `$PLONIX_BROWSER`, if you set it to a browser's executable.
2. The first Chromium-based browser it finds: Google Chrome, Chromium, Brave,
   Microsoft Edge, Chrome Canary (in `/Applications` or `~/Applications`).
3. The **Plonix browser**, if you downloaded it (below).
4. Firefox.

Chromium-based browsers trust the Plonix certificate on their own (Plonix
passes its key pin when it launches them), so HTTPS works with nothing to set
up. `plonix browser` shows which browser Plonix will use.

## The Plonix browser: for Macs with only Safari

Safari cannot be launched with an isolated profile and its own proxy, so a
Mac with only Safari has nothing for Plonix to open. In that case Open target
shows **Get the Plonix browser**:

- One click downloads Chromium: Google's official *Chrome for Testing* stable
  build for your Mac (Apple silicon or Intel), about 150 MB. Plonix looks up
  the current version in Google's published list
  (`googlechromelabs.github.io/chrome-for-testing`) and downloads it over
  HTTPS from Google's servers.
- A progress bar shows the download. Plonix then unpacks it, checks that it
  is complete and starts on your Mac, and only then puts it in place. If
  anything fails, you see why and can **Try again**; nothing half-finished is
  left behind.
- When it is ready, the target opens in it right away, and every later Open
  target uses it too. It behaves like any other capture browser: its own
  profile per project, the proxy, the Plonix certificate trusted.

It is kept in `~/.plonix/chromium` (or `$PLONIX_HOME/chromium`), separate from
the app, which is why Plonix.app itself stays small. If you install Chrome,
Brave or Edge later, Plonix uses that instead.

From the terminal:

```sh
plonix browser            # which browser Plonix uses, and the Plonix browser if downloaded
plonix browser install    # download (or update to) the current Plonix browser
plonix browser remove     # delete it; your capture profiles are kept
```

`plonix open` offers the download itself when it finds no browser and you are
at a terminal. On Linux (64-bit x86) the same download works with the Linux
build; it needs `unzip`.

## Firefox and the Plonix certificate

When Firefox is the capture browser, Plonix gives it an isolated profile that
uses the proxy and follows the certificates your Mac trusts
(`security.enterprise_roots.enabled`). Firefox therefore needs the Plonix
certificate trusted once in your login keychain.

Right after Open target launches Firefox, the window shows **Trust the Plonix
certificate**, but only while the certificate is not trusted yet. Clicking
**Trust certificate** adds it to your login keychain; macOS asks for your
password or Touch ID. Reload the page in Firefox and HTTPS goes through
Plonix. The same thing from a terminal is `plonix ca trust`.

The certificate was generated on your Mac the first time Plonix ran, and its
private key never leaves `~/.plonix`. Trusting it also lets Safari, curl and
other apps accept HTTPS through the Plonix proxy. To undo it, open Keychain
Access and delete "Plonix CA".

On Linux, Firefox keeps its own certificate list: import `~/.plonix/ca.pem` in
Firefox › Settings › Privacy & Security › Certificates (`plonix ca` shows the
path).

## The plonix command from the app

Plonix.app carries the `plonix` command line tool inside it. To use it in a
terminal, choose **Plonix › Install Command Line Tool…**:

- Plonix links `/usr/local/bin/plonix` to the tool inside the app, so the
  command always matches the app, updates included. `/usr/local/bin` is on
  the PATH of every macOS shell.
- If that folder does not exist yet or cannot be written as you (the usual
  case on a new Apple silicon Mac), macOS shows its standard administrator
  prompt. Otherwise nothing is asked.
- A dialog confirms it worked, or says why it did not. Open a new Terminal
  window and run `plonix --help`.
- Run Plonix from Applications first: a command linked to the app on the
  disk image would stop working once the image is ejected, so Plonix asks
  you to move it.
- If `/usr/local/bin/plonix` already exists and did not come from Plonix,
  Plonix asks before replacing it.

**Plonix › Uninstall Command Line Tool…** removes the link (only if Plonix
made it).

The command talks to the same projects as the app: projects open in the app
are reachable with `plonix -p <project> …`, and projects you start from the
terminal show up on the Start screen.

### Building the app with the command inside

The tool is bundled as a Tauri sidecar named after the target it was built
for: `crates/plonix-app/binaries/plonix-cli-<target>`. The CI and release
workflows build it before the app (for releases, one binary per architecture
joined with `lipo` into `plonix-cli-universal-apple-darwin`). A local
`cargo build` or `cargo run -p plonix-app` without it gets a stand-in script,
and the menu item then explains that this build has no command line tool.
To bundle it in a local build:

```sh
cargo build --release -p plonix
mkdir -p crates/plonix-app/binaries
cp target/release/plonix crates/plonix-app/binaries/plonix-cli-$(rustc -vV | sed -n 's/^host: //p')
cd crates/plonix-app && cargo tauri build --bundles app
```

Replace an earlier stand-in the same way: the build picks up the real binary
as soon as it is there.
