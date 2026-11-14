#!/usr/bin/env bash
# Sign and notarize the Mac app of a draft release on your own Mac, then
# publish the release.
#
# Pushing a tag builds the release in CI as a draft (see docs/releasing.md).
# This script finishes it: it downloads that build's Plonix.app, signs it with
# the Developer ID certificate in your keychain, notarizes and staples it,
# rebuilds the disk image, the zip and the update package from the signed app,
# signs the update package with the update key, puts its signature in
# latest.json, uploads all of them over the CI files, drops the "not
# notarized" part of the notes and publishes the release.
#
# Usage:
#   scripts/macos/release.sh [options] vX.Y.Z
#
# Options:
#   --no-publish   Upload the signed files but leave the release a draft.
#   -h, --help     Show this help.
#
# Settings (environment variables, all optional):
#   APPLE_SIGNING_IDENTITY   Identity to sign with. Default: the one
#                            "Developer ID Application" identity in your
#                            keychain.
#   APPLE_KEYCHAIN_PROFILE   notarytool profile saved with
#                            `xcrun notarytool store-credentials`.
#                            Default: plonix-notary.
#   PLONIX_UPDATER_KEY       Update signing key file.
#                            Default: ~/Plonix/signing-keys/plonix-updater.key
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  Its password, if it has one.
#   PLONIX_REPO              Default: SergeyMalych/plonix
#
# Needs: Xcode command line tools, gh (signed in), jq, and npx or cargo tauri.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="${PLONIX_REPO:-SergeyMalych/plonix}"
publish=true
tag=""

usage() {
  sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() {
  echo "error: $*" >&2
  exit 1
}

step() {
  echo
  echo "==> $*"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --no-publish) publish=false ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*) die "unknown option $1 (see --help)" ;;
    *)
      [ -z "$tag" ] || die "only one tag, please"
      tag="$1"
      ;;
  esac
  shift
done

[ -n "$tag" ] || die "give the release tag, for example v0.6.0 (see --help)"
case "$tag" in v*) ;; *) die "the tag starts with v, for example v0.6.0" ;; esac
[ "$(uname -s)" = Darwin ] || die "this script runs on macOS only"
for tool in gh jq xcrun codesign hdiutil ditto; do
  command -v "$tool" >/dev/null || die "$tool is not installed"
done
if command -v npx >/dev/null; then
  tauri=(npx --yes @tauri-apps/cli@2)
elif cargo tauri --version >/dev/null 2>&1; then
  tauri=(cargo tauri)
else
  die "install Node.js (for npx) or the Tauri CLI (cargo install tauri-cli)"
fi

export APPLE_KEYCHAIN_PROFILE="${APPLE_KEYCHAIN_PROFILE:-plonix-notary}"
updater_key="${PLONIX_UPDATER_KEY:-$HOME/Plonix/signing-keys/plonix-updater.key}"
[ -f "$updater_key" ] || die "update key not found at $updater_key (set PLONIX_UPDATER_KEY)"

if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
  identities="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | sort -u)"
  count="$(printf '%s' "$identities" | grep -c . || true)"
  [ "$count" -ge 1 ] || die "no Developer ID Application identity in your keychain (docs/releasing.md, step 2)"
  [ "$count" -eq 1 ] || die "several Developer ID identities; pick one with APPLE_SIGNING_IDENTITY"
  APPLE_SIGNING_IDENTITY="$identities"
fi
export APPLE_SIGNING_IDENTITY
unset APPLE_CERTIFICATE APPLE_ID APPLE_API_KEY

step "Checking the release"
info="$(gh release view "$tag" --repo "$repo" --json isDraft,assets)" ||
  die "no release $tag in $repo yet; wait for the Release workflow to finish"
echo "Signing as: $APPLE_SIGNING_IDENTITY"
echo "Notarizing with the keychain profile: $APPLE_KEYCHAIN_PROFILE"
xcrun notarytool history --keychain-profile "$APPLE_KEYCHAIN_PROFILE" >/dev/null ||
  die "the notarytool profile $APPLE_KEYCHAIN_PROFILE does not work (docs/releasing.md, step 3)"
[ "$(jq -r .isDraft <<<"$info")" = true ] ||
  echo "Note: $tag is already published. Its Mac files will be replaced."

work="$(mktemp -d "${TMPDIR:-/tmp}/plonix-release.XXXXXX")"
trap 'rm -rf "$work"' EXIT
cd "$work"

step "Downloading the build from CI"
gh release download "$tag" --repo "$repo" --pattern Plonix-macOS.zip --pattern latest.json
ditto -x -k Plonix-macOS.zip unpacked
app="$work/unpacked/Plonix.app"
[ -d "$app" ] || die "Plonix-macOS.zip has no Plonix.app"

step "Signing and notarizing Plonix.app"
"$here/sign-and-notarize.sh" "$app"

out="$work/out"
mkdir -p "$out"

step "Building the disk image"
stage="$work/dmg"
mkdir -p "$stage"
ditto "$app" "$stage/Plonix.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Plonix -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$out/Plonix-macOS.dmg"
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "$out/Plonix-macOS.dmg"

step "Notarizing the disk image"
xcrun notarytool submit "$out/Plonix-macOS.dmg" --keychain-profile "$APPLE_KEYCHAIN_PROFILE" --wait --output-format json >dmg.json || true
cat dmg.json
echo
[ "$(plutil -extract status raw -o - dmg.json 2>/dev/null)" = Accepted ] || die "the disk image was not accepted"
xcrun stapler staple "$out/Plonix-macOS.dmg"
hdiutil verify "$out/Plonix-macOS.dmg"
spctl --assess --type open --context context:primary-signature -vvv "$out/Plonix-macOS.dmg"

step "Writing the zip and the update package"
ditto -c -k --keepParent "$app" "$out/Plonix-macOS.zip"
COPYFILE_DISABLE=1 tar -czf "$out/Plonix-macOS.app.tar.gz" -C "$work/unpacked" Plonix.app
"${tauri[@]}" signer sign --private-key-path "$updater_key" \
  --password "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" "$out/Plonix-macOS.app.tar.gz"
[ -s "$out/Plonix-macOS.app.tar.gz.sig" ] || die "the update package was not signed"
jq --rawfile sig "$out/Plonix-macOS.app.tar.gz.sig" \
  '.platforms["darwin-aarch64"].signature = $sig | .platforms["darwin-x86_64"].signature = $sig' \
  latest.json >"$out/latest.json"

step "Uploading to the release"
gh release upload "$tag" --repo "$repo" --clobber \
  "$out/Plonix-macOS.dmg" "$out/Plonix-macOS.zip" "$out/Plonix-macOS.app.tar.gz" "$out/latest.json"

step "Updating the release notes"
gh release view "$tag" --repo "$repo" --json body --jq .body >notes.md
# The notes CI writes for an app that is not notarized have a section on how
# to open it the first time; the app is notarized now.
awk '
  /^### Opening Plonix the first time/ { skip = 1; next }
  /^### / { skip = 0 }
  !skip { print }
' notes.md >notes-signed.md
gh release edit "$tag" --repo "$repo" --notes-file notes-signed.md

if [ "$publish" = true ]; then
  step "Publishing $tag"
  gh release edit "$tag" --repo "$repo" --draft=false --latest
  echo
  echo "Done: https://github.com/$repo/releases/tag/$tag"
else
  echo
  echo "Done. $tag is still a draft: publish it on GitHub, or run"
  echo "  gh release edit $tag --repo $repo --draft=false --latest"
fi
