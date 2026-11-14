# Releasing Plonix

This page is for maintainers. It covers the one-time setup for signing and notarizing Plonix.app, and how to cut a release.

## Contents

- [How signing works](#how-signing-works)
- [One-time setup](#one-time-setup)
- [Cutting a release](#cutting-a-release)
- [Windows code signing](#windows-code-signing)
- [Signing in CI instead](#signing-in-ci-instead)
- [Signing by hand](#signing-by-hand)
- [Troubleshooting](#troubleshooting)

## How signing works

macOS only opens downloaded apps without warnings when they are signed with a **Developer ID Application** certificate, use the **hardened runtime**, and are **notarized** by Apple, with the notarization ticket **stapled** to the app.

The Mac app is signed on the maintainer's Mac, so the certificate's private key never leaves it and no Apple credential is stored in GitHub:

1. Pushing a `v*` tag runs the **Release** workflow (`.github/workflows/release.yml`). It builds a universal Plonix.app (signed ad hoc), the Windows installer and the update packages, and makes a **draft** release. Installed copies of Plonix and plonix.io only see published releases, so nobody gets the draft.
2. On the Mac, `scripts/macos/release.sh vX.Y.Z` downloads that build, signs it with the Developer ID certificate in the keychain, notarizes and staples it, rebuilds the disk image (also signed, notarized and stapled), the zip and the update package from the signed app, signs the update package with the update key, writes its signature into `latest.json`, uploads all of them over the CI files, removes the "not notarized" section from the notes and publishes the release.

What the signature covers:

- `crates/plonix-app/tauri.conf.json` turns on the hardened runtime (`bundle.macOS.hardenedRuntime`) and points at `crates/plonix-app/entitlements.plist` (`bundle.macOS.entitlements`).
- The entitlements file asks for one exception only: Apple Events, so that **Ask Claude Code** can open Terminal. macOS still asks the user first, with the text in `crates/plonix-app/Info.plist`. Plonix is not sandboxed and needs no JIT entitlement: the web view's JavaScript runs in WebKit's own process.
- `scripts/macos/sign-and-notarize.sh` does the app itself: it signs nested code first (the `plonix` command inside the app), then the app with the entitlements, all with the hardened runtime and a secure timestamp, notarizes with `xcrun notarytool submit --wait`, staples, and verifies with `codesign --verify --deep --strict` and `spctl`. With `--verify` it only checks a bundle. Run either script with `--help` for the options.

CI builds on pull requests and on `main` (`.github/workflows/ci.yml`, job `macos-app`) stay unsigned.

## One-time setup

You need a Mac with Xcode (or the Command Line Tools, `xcode-select --install`), [GitHub CLI](https://cli.github.com) signed in with `gh auth login`, `jq`, and Node.js (for `npx`) or the Tauri CLI.

### 1. Join the Apple Developer Program

Enroll at [developer.apple.com/programs](https://developer.apple.com/programs/) as an individual or an organization (USD 99 a year). Only the **Account Holder** can create Developer ID certificates. Your **Team ID** is the 10-character code under Membership details at [developer.apple.com/account](https://developer.apple.com/account).

### 2. Create a Developer ID Application certificate

Use **Developer ID Application**. "Apple Development", "Apple Distribution" and "Mac App Distribution" certificates can't sign an app people download from the web.

The easiest way, which also puts the private key in your keychain: in **Xcode › Settings › Accounts**, select your team, click **Manage Certificates…**, then **+ › Developer ID Application**.

Or on the website:

1. In **Keychain Access › Certificate Assistant › Request a Certificate From a Certificate Authority…**, enter your email and name, choose **Saved to disk**, and save the `.certSigningRequest` file.
2. At [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates/list), click **+**, choose **Developer ID Application**, pick the **G2 Sub-CA** profile, upload the request and download the `.cer` file.
3. Double-click the `.cer` on the same Mac that made the request (the private key is there).

Check it is there:

```sh
security find-identity -v -p codesigning
#   1) 3F2A…  "Developer ID Application: Jane Doe (AB12CD34EF)"
```

Developer ID certificates are valid for five years. Back up the certificate with its private key (Keychain Access › My Certificates › right-click › **Export…** as `.p12`) somewhere safe: you can't download the private key again.

### 3. Save the notarization credentials in the keychain

`release.sh` uses a notarytool profile called `plonix-notary`. With an App Store Connect API key (not tied to a person's password):

1. In [App Store Connect](https://appstoreconnect.apple.com), open **Users and Access › Integrations › App Store Connect API**, and under **Team Keys** click **+**. Name it `Plonix notarization` with the **Developer** role.
2. Download `AuthKey_<KEYID>.p8` (Apple lets you download it only once) and note the **Key ID** and the **Issuer ID** above the table.
3. Save the profile:

   ```sh
   xcrun notarytool store-credentials plonix-notary \
     --key ~/Downloads/AuthKey_ABC123DEF4.p8 --key-id ABC123DEF4 \
     --issuer 00000000-0000-0000-0000-000000000000
   ```

   notarytool keeps a copy in your keychain, so move the `.p8` to your backup afterwards.

With an Apple ID instead: create an app-specific password at [account.apple.com](https://account.apple.com) (**Sign-In and Security › App-Specific Passwords**) and run `xcrun notarytool store-credentials plonix-notary --apple-id you@example.com --team-id AB12CD34EF`, which asks for it.

Check it works: `xcrun notarytool history --keychain-profile plonix-notary`.

### 4. The update signing key

Plonix.app only installs updates signed with its own update key, which is separate from the Apple certificate ([docs/updates.md](updates.md)). `release.sh` reads it from `~/Plonix/signing-keys/plonix-updater.key`; set `PLONIX_UPDATER_KEY` for another path and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if it has a password. CI signs the Windows update package with the same key from the `TAURI_SIGNING_PRIVATE_KEY` secret.

## Cutting a release

1. Make sure `main` is green.
2. Bump the version in `crates/plonix-app/tauri.conf.json` (`version`), and in `Cargo.toml` files if you want the CLI to report the same version. Move the `Unreleased` entries in `CHANGELOG.md` under a heading for the new version (`## [0.2.0] - <date>`): that section becomes the release notes. Commit with a message like `Release 0.2.0`.
3. Tag the commit and push the tag:

   ```sh
   git fetch origin && git tag v0.2.0 origin/main && git push origin v0.2.0
   ```

4. The **Release** workflow checks that the tag matches the version, builds the `plonix` command for both architectures and joins them with `lipo` (Plonix.app carries it for Install Command Line Tool…, see [setup.md](setup.md#the-plonix-command-from-the-app)), builds a universal (Apple silicon and Intel) Plonix.app, packages it as a disk image and a zip, builds the Windows installer, signs the update packages, writes `latest.json` and makes a **draft** release with the notes from `CHANGELOG.md`. It stops with an error if the tag and version differ or the changelog has no section for the version.
5. When it has finished, on your Mac, from a checkout of this repository:

   ```sh
   scripts/macos/release.sh v0.2.0
   ```

   Notarization usually takes a few minutes for each of the app and the disk image. The script stops at the first problem and leaves the release a draft; run it again once fixed. Add `--no-publish` to upload the signed files and publish the draft yourself on GitHub.
6. Double-click the published `Plonix-macOS.dmg` on a Mac: a notarized app opens without asking about an unidentified developer.

To ship a release without the Mac step (not notarized; its notes say how to open it the first time), publish the draft on GitHub as it is.

## Windows code signing

The Windows installer is signed for free by SignPath Foundation, which signs open-source projects that build in public CI. Until the settings below exist, the release workflow ships the installer unsigned and the release notes say how to get past SmartScreen.

1. Apply at [signpath.org/apply](https://signpath.org/apply.html). Plonix meets the conditions: Apache-2.0, public repository, releases built by GitHub Actions, and a [code signing policy](code-signing.md) linked from plonix.io.
2. Once approved, in SignPath: create the project `plonix` linked to this repository (GitHub as a trusted build system), a signing policy `release-signing` that needs manual approval, and this artifact configuration (the workflow uploads a zip holding the installer):

   ```xml
   <?xml version="1.0" encoding="utf-8"?>
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="Plonix-Windows-setup.exe">
         <authenticode-sign/>
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

3. Create a CI user in SignPath with submitter rights on the project and copy its API token.
4. In GitHub, **Settings › Secrets and variables › Actions**: add the secret `SIGNPATH_API_TOKEN`, and the variable `SIGNPATH_ORGANIZATION_ID`. Set `SIGNPATH_PROJECT_SLUG` or `SIGNPATH_SIGNING_POLICY_SLUG` only if you named them differently.

From then on every release sends the installer to SignPath, waits up to an hour for the request to be approved, checks the Authenticode signature, and only then signs the update package, so updates carry the signed installer. A new certificate can still meet a SmartScreen warning for a while, until enough people have downloaded the signed installer.

## Signing in CI instead

The Release workflow can also sign and notarize in CI: when these repository secrets are set, it signs with Developer ID during the build and publishes the release directly, and pushes to `main` produce a signed `Plonix-macOS` artifact. With them, the certificate's private key is stored in GitHub.

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | The base64 text of the exported `.p12` (`base64 -i DeveloperID.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12`'s password |
| `APPLE_SIGNING_IDENTITY` | The identity name, e.g. `Developer ID Application: Jane Doe (AB12CD34EF)` |
| `APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_PRIVATE_KEY` | Key ID, issuer ID and the full `.p8` contents; or `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` |

Secrets are never given to workflows triggered by pull requests from forks, and CI only signs on pushes to `main` and tags.

## Signing by hand

To sign a build on your own Mac, for example to test the entitlements:

```sh
cd crates/plonix-app
cargo tauri build --bundles app
cd ../..

# Uses the identity in your login keychain and the notarytool profile:
APPLE_SIGNING_IDENTITY="Developer ID Application: Jane Doe (AB12CD34EF)" \
APPLE_KEYCHAIN_PROFILE=plonix-notary \
  scripts/macos/sign-and-notarize.sh --zip Plonix-macOS.zip target/release/bundle/macos/Plonix.app
```

Add `--no-notarize` to only sign.

## Troubleshooting

- **Notarization rejected.** The script prints Apple's log. For a submission made by Tauri, run `xcrun notarytool history` and `xcrun notarytool log <id>` with the same credentials. The usual causes are a missing secure timestamp, the hardened runtime being off, or a binary signed with a different identity.
- **"The specified item could not be found in the keychain".** `APPLE_SIGNING_IDENTITY` doesn't match the certificate in `APPLE_CERTIFICATE`. Compare it with the output of `security find-identity -v -p codesigning`.
- **Ask Claude Code does nothing in a signed build.** Check the app has the Apple Events entitlement (`codesign -d --entitlements - Plonix.app`) and that Plonix is allowed under **System Settings › Privacy & Security › Automation**.
- **"no Developer ID Application identity in your keychain".** The certificate is missing, is another kind (see step 2), or its private key is on another Mac. `security find-identity -v -p codesigning` only lists certificates whose private key is present.
- **The certificate expires or leaks.** Create a new one (step 2), and if it leaked, revoke the old one in the developer portal. Apps already notarized keep working.
