# Releasing Plonix

This page is for maintainers. It covers the one-time setup that lets CI sign and notarize Plonix.app, and how to cut a release.

## Contents

- [How signing works](#how-signing-works)
- [One-time setup](#one-time-setup)
- [GitHub secrets](#github-secrets)
- [Cutting a release](#cutting-a-release)
- [Signing by hand](#signing-by-hand)
- [Troubleshooting](#troubleshooting)

## How signing works

macOS only opens downloaded apps without warnings when they are signed with a **Developer ID Application** certificate, use the **hardened runtime**, and are **notarized** by Apple, with the notarization ticket **stapled** to the app.

Tauri does all of that during `tauri build` when the right environment variables are set:

- `crates/plonix-app/tauri.conf.json` turns on the hardened runtime (`bundle.macOS.hardenedRuntime`) and points at `crates/plonix-app/entitlements.plist` (`bundle.macOS.entitlements`).
- The entitlements file asks for one exception only: Apple Events, so that **Ask Claude Code** can open Terminal. macOS still asks the user first, with the text in `crates/plonix-app/Info.plist`. Plonix is not sandboxed and needs no JIT entitlement: the web view's JavaScript runs in WebKit's own process.
- With `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY` set, Tauri imports the certificate into a temporary keychain and signs the app. With the notarization variables also set, it submits the app to Apple's notary service, waits for the result and staples the ticket.

`scripts/macos/sign-and-notarize.sh` is the explicit fallback. Given a built `Plonix.app` and the same variables, it imports the certificate into a temporary keychain, signs with the hardened runtime and the entitlements, notarizes with `xcrun notarytool submit --wait`, staples, verifies with `codesign --verify --deep --strict` and `spctl`, and deletes the keychain. With `--verify` it only checks a bundle someone else signed, which is how CI checks what Tauri produced. Run it with `--help` for the options.

Where it happens:

| Workflow | When | Result |
| --- | --- | --- |
| CI (`.github/workflows/ci.yml`, job `macos-app`) | Pull requests, forks, or no signing secrets | Unsigned `Plonix-macOS` artifact, as before |
| CI, job `macos-app` | Pushes to `main` with the signing secrets set | Signed, notarized and stapled `Plonix-macOS` artifact; the job fails if any check fails |
| Release (`.github/workflows/release.yml`) | Pushing a `v*` tag | A GitHub release with the app and the update package (see [Cutting a release](#cutting-a-release)) |

Secrets are never given to workflows triggered by pull requests from forks, and CI only signs on pushes to `main`, so code from a pull request never runs with the certificate.

## One-time setup

You need a Mac with Xcode or the Command Line Tools (`xcode-select --install`).

### 1. Join the Apple Developer Program

Enroll at [developer.apple.com/programs](https://developer.apple.com/programs/) as an individual or an organization (USD 99 a year). Only the **Account Holder** can create Developer ID certificates. Once enrolled, note your **Team ID**: a 10-character code shown under Membership details at [developer.apple.com/account](https://developer.apple.com/account).

### 2. Create a Developer ID Application certificate

1. On your Mac, open **Keychain Access**, then **Keychain Access › Certificate Assistant › Request a Certificate From a Certificate Authority…**. Enter your email and name, choose **Saved to disk**, and save the `.certSigningRequest` file.
2. At [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates/list), click **+**, choose **Developer ID Application**, pick the **G2 Sub-CA** profile, upload the request and download the `.cer` file.
3. Double-click the `.cer` to add it to your login keychain. Check it is there and copy its exact name, which is your **signing identity**:

   ```sh
   security find-identity -v -p codesigning
   #   1) 3F2A…  "Developer ID Application: Jane Doe (AB12CD34EF)"
   ```

Developer ID certificates are valid for five years. Keep a backup of the exported `.p12` (next step) somewhere safe: you can't download the private key again.

### 3. Export the certificate as a .p12

1. In Keychain Access, under **login › My Certificates**, find **Developer ID Application: …**, expand it to check the private key is there, right-click the certificate and choose **Export…**.
2. Save it as `DeveloperID.p12` in **Personal Information Exchange (.p12)** format and set a strong password.
3. Encode it for GitHub:

   ```sh
   base64 -i DeveloperID.p12 | pbcopy     # now on the clipboard
   ```

### 4. Create notarization credentials

Pick one.

**A. Apple ID with an app-specific password** (simplest)

1. Sign in at [account.apple.com](https://account.apple.com), open **Sign-In and Security › App-Specific Passwords** and create one called `Plonix notarization`.
2. You'll need your Apple ID email, that password and your Team ID.

**B. App Store Connect API key** (not tied to a person's password)

1. In [App Store Connect](https://appstoreconnect.apple.com), open **Users and Access › Integrations › App Store Connect API**, and under **Team Keys** click **+**. Name it `Plonix notarization` with the **Developer** role.
2. Download `AuthKey_<KEYID>.p8`. Apple only lets you download it once.
3. Note the **Key ID** (in the table) and the **Issuer ID** (above the table).

Check the credentials work before adding them to GitHub:

```sh
xcrun notarytool history --apple-id you@example.com --password abcd-efgh-ijkl-mnop --team-id AB12CD34EF
# or
xcrun notarytool history --key AuthKey_ABC123DEF4.p8 --key-id ABC123DEF4 --issuer 00000000-0000-0000-0000-000000000000
```

### 5. Create the update signing key

Plonix.app checks for updates and only installs packages signed with its own update key. This key is separate from the Apple certificate. Follow [docs/updates.md](updates.md) (added with the updater) to create it with `cargo tauri signer generate`, put the public key in `plugins.updater.pubkey` in `crates/plonix-app/tauri.conf.json`, and add the two secrets listed below.

### 6. Add the GitHub secrets

In the repository, open **Settings › Secrets and variables › Actions › New repository secret** and add the secrets below. Names must match exactly.

## GitHub secrets

Signing (all required to sign):

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | The base64 text of `DeveloperID.p12` (step 3) |
| `APPLE_CERTIFICATE_PASSWORD` | The password you set when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The identity name, e.g. `Developer ID Application: Jane Doe (AB12CD34EF)` |

Notarization, option A (Apple ID):

| Secret | Value |
| --- | --- |
| `APPLE_ID` | Your Apple ID email |
| `APPLE_PASSWORD` | The app-specific password (not your Apple ID password) |
| `APPLE_TEAM_ID` | Your Team ID, e.g. `AB12CD34EF` |

Notarization, option B (API key):

| Secret | Value |
| --- | --- |
| `APPLE_API_KEY` | The key ID, e.g. `ABC123DEF4` |
| `APPLE_API_ISSUER` | The issuer ID (a UUID) |
| `APPLE_API_PRIVATE_KEY` | The full contents of `AuthKey_<KEYID>.p8`, including the `BEGIN` and `END` lines |

The workflows write `APPLE_API_PRIVATE_KEY` to a temporary file and pass its path to Tauri and `notarytool` as `APPLE_API_KEY_PATH`. Set only one option; if both are set, the Apple ID is used.

Updates (from the updater, see [docs/updates.md](updates.md)):

| Secret | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | The contents of the update private key file |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Its password, if you set one |

Once the signing and notarization secrets are in place, the next push to `main` produces a signed and notarized `Plonix-macOS` artifact. Check the `macos-app` job's **Verify the signature and notarization** step to confirm.

## Cutting a release

1. Make sure `main` is green, including the signed `macos-app` build.
2. Bump the version in `crates/plonix-app/tauri.conf.json` (`version`), and in `Cargo.toml` files if you want the CLI to report the same version. Commit with a message like `Release 0.2.0`.
3. Tag the commit and push the tag:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

4. The **Release** workflow (added with the updater; it signs once it passes the signing secrets to the build like CI does) checks that the tag matches the version, builds a universal (Apple silicon and Intel) Plonix.app, signs and notarizes it, signs the update package, writes `latest.json` and publishes a GitHub release with all of them. It stops with an error if the tag and version differ or a key is missing.
5. Edit the release notes on GitHub if needed, then download `Plonix-macOS.zip` from the release and check it on a Mac:

   ```sh
   ditto -x -k Plonix-macOS.zip . && scripts/macos/sign-and-notarize.sh --verify Plonix.app
   ```

   or simply double-click it: a notarized app opens without the "unidentified developer" warning.

## Signing by hand

To sign a build on your own Mac, for example to test the entitlements:

```sh
cd crates/plonix-app
cargo tauri build --bundles app
cd ../..

# Uses the identity in your login keychain:
APPLE_SIGNING_IDENTITY="Developer ID Application: Jane Doe (AB12CD34EF)" \
APPLE_ID=you@example.com APPLE_PASSWORD=abcd-efgh-ijkl-mnop APPLE_TEAM_ID=AB12CD34EF \
  scripts/macos/sign-and-notarize.sh --zip Plonix-macOS.zip target/release/bundle/macos/Plonix.app
```

Add `--no-notarize` to only sign, or set the variables before `cargo tauri build` to let Tauri do it all.

## Troubleshooting

- **Notarization rejected.** The script prints Apple's log. For a submission made by Tauri, run `xcrun notarytool history` and `xcrun notarytool log <id>` with the same credentials. The usual causes are a missing secure timestamp, the hardened runtime being off, or a binary signed with a different identity.
- **"The specified item could not be found in the keychain".** `APPLE_SIGNING_IDENTITY` doesn't match the certificate in `APPLE_CERTIFICATE`. Compare it with the output of `security find-identity -v -p codesigning`.
- **Ask Claude Code does nothing in a signed build.** Check the app has the Apple Events entitlement (`codesign -d --entitlements - Plonix.app`) and that Plonix is allowed under **System Settings › Privacy & Security › Automation**.
- **The certificate expires or leaks.** Create a new one (step 2), update the three signing secrets, and if it leaked, revoke the old one in the developer portal. Apps already notarized keep working.
