# Updates

Plonix.app never installs anything on its own. Every step that changes the app
on disk is the user's choice, and closing a dialog always means "no".

## What the user sees

1. **Whether to look at all.** On first launch Plonix asks once: *Check Daily*
   or *Only When I Ask*. Until that is answered, nothing is checked. The choice
   can be changed any time in **Plonix › Check for Updates Automatically**:
   When Plonix Starts, Daily, Weekly, or Never (Only When I Ask).
   **Plonix › Check for Updates…** works whatever is picked.
2. **Whether to download.** When a newer version is found, Plonix shows its
   version and release notes with **Download…**, **Skip This Version** and
   **Not Now**. An automatic check stays quiet about a skipped version; a
   manual check still shows it. Downloading only fetches the package and
   checks its signature. Nothing on disk changes.
3. **Whether to install.** Once the package is downloaded and verified, Plonix
   asks again: **Install and Restart** or **Not Now**. Only then is the app
   replaced and restarted. Projects and captured traffic live in
   `~/.plonix` and are kept.

Automatic checks only speak up when there is something new. A manual check
always reports its outcome, including errors.

The choice is stored in `$PLONIX_HOME/updates.json` (default
`~/.plonix/updates.json`):

```json
{ "check": "daily", "last_check": 1791100000, "skipped_version": null }
```

`check` is one of `on-launch`, `daily`, `weekly`, `never`, or absent while the
first-launch question is unanswered.

The checks, downloads and installs run in the app itself, behind native
dialogs. The pages shown in the Plonix window have no access to them.

## How a release reaches users

A check reads `latest.json` from the newest GitHub release:

```
https://github.com/SergeyMalych/plonix/releases/latest/download/latest.json
```

Update packages are signed. The app only installs a package whose signature
matches the public key built into it (`plugins.updater.pubkey` in
`crates/plonix-app/tauri.conf.json`). A build without that key can still tell
the user about a new version, but sends them to the releases page instead of
installing it.

### One-time setup

1. Create the signing key pair on your own machine and keep the private key
   out of the repository:

   ```sh
   cargo tauri signer generate -w ~/.plonix-release/updater.key
   ```

2. Paste the printed **public** key into `plugins.updater.pubkey` in
   `crates/plonix-app/tauri.conf.json` and commit it.
3. In the repository settings, under Secrets and variables › Actions, add
   `TAURI_SIGNING_PRIVATE_KEY` (the contents of `updater.key`) and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (its password, if you set one).

Keep a backup of the private key. Without it, installed copies can't verify
any later release and their users have to download the next version by hand.

### Publishing a release

1. Bump `version` in `crates/plonix-app/tauri.conf.json` (for example to
   `0.2.0`).
2. Push a matching tag: `git tag v0.2.0 && git push origin v0.2.0`.

The Release workflow builds a universal Plonix.app, signs the update package,
writes `latest.json` and publishes the GitHub release with all three. It stops
with a clear error if the tag and version differ or the key is missing.
