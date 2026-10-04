#!/usr/bin/env bash
# Sign, notarize, staple and verify a built Plonix.app.
#
# Tauri already signs and notarizes Plonix.app during `tauri build` when the
# signing variables below are set; see docs/releasing.md. This script is the
# explicit fallback for doing the same by hand or in CI, and its --verify mode
# checks a bundle Tauri produced.
#
# Usage:
#   scripts/macos/sign-and-notarize.sh [options] path/to/Plonix.app
#
# Options:
#   --verify        Only verify: signature, hardened runtime, notarization
#                   ticket and Gatekeeper. Staples the ticket first if it is
#                   missing but Apple has one.
#   --no-notarize   Sign and verify the signature, but don't notarize.
#   --zip FILE      After verifying, write the app to FILE as a zip (ditto).
#   -h, --help      Show this help.
#
# Signing identity (one of):
#   APPLE_CERTIFICATE           Developer ID Application certificate and key,
#                               as a base64-encoded .p12.
#   APPLE_CERTIFICATE_PASSWORD  The .p12's password.
#   APPLE_SIGNING_IDENTITY      The identity to sign with, for example
#                               "Developer ID Application: Jane Doe (AB12CD34EF)".
#                               Optional with APPLE_CERTIFICATE (found in the
#                               certificate); required without it, in which case
#                               the identity must already be in your keychain.
#
# Notarization (one of):
#   APPLE_ID, APPLE_PASSWORD (an app-specific password), APPLE_TEAM_ID
#   APPLE_API_KEY (key ID), APPLE_API_ISSUER (issuer ID), APPLE_API_KEY_PATH
#   (path to the AuthKey_<id>.p8 file)

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
entitlements="${PLONIX_ENTITLEMENTS:-$here/../../crates/plonix-app/entitlements.plist}"

mode=full
zip_out=""
app=""

usage() {
  sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() {
  echo "error: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --verify) mode=verify ;;
    --no-notarize) mode=sign ;;
    --zip)
      [ $# -ge 2 ] || die "--zip needs a file name"
      zip_out="$2"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*) die "unknown option $1 (see --help)" ;;
    *)
      [ -z "$app" ] || die "only one app path, please"
      app="$1"
      ;;
  esac
  shift
done

[ -n "$app" ] || die "give the path to Plonix.app (see --help)"
[ -d "$app" ] || die "$app is not an app bundle"
[ "$(uname -s)" = Darwin ] || die "this script runs on macOS only"
app="$(cd "$(dirname "$app")" && pwd)/$(basename "$app")"

workdir="$(mktemp -d "${TMPDIR:-/tmp}/plonix-sign.XXXXXX")"
keychain=""
old_keychains=()

cleanup() {
  if [ -n "$keychain" ]; then
    if [ ${#old_keychains[@]} -gt 0 ]; then
      security list-keychains -d user -s "${old_keychains[@]}" || true
    fi
    security delete-keychain "$keychain" 2>/dev/null || true
  fi
  rm -rf "$workdir"
}
trap cleanup EXIT

step() {
  echo
  echo "==> $*"
}

# Import the certificate into a temporary keychain that only lives as long as
# this script, so the private key never lands in the login keychain.
import_certificate() {
  [ -n "${APPLE_CERTIFICATE_PASSWORD:-}" ] || die "APPLE_CERTIFICATE is set but APPLE_CERTIFICATE_PASSWORD is not"
  step "Importing the signing certificate into a temporary keychain"
  local p12="$workdir/certificate.p12"
  local password
  password="$(uuidgen)"
  keychain="$workdir/plonix-signing.keychain-db"

  printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$p12"
  security create-keychain -p "$password" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$password" "$keychain"

  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%\"}"
    line="${line#\"}"
    [ -n "$line" ] && old_keychains+=("$line")
  done < <(security list-keychains -d user)
  security list-keychains -d user -s "$keychain" "${old_keychains[@]}"

  security import "$p12" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" \
    -T /usr/bin/codesign -T /usr/bin/security >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
  rm -f "$p12"

  if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
    APPLE_SIGNING_IDENTITY="$(security find-identity -v -p codesigning "$keychain" |
      sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -n 1)"
    [ -n "$APPLE_SIGNING_IDENTITY" ] || die "no Developer ID Application identity in APPLE_CERTIFICATE"
  fi
  echo "Signing as: $APPLE_SIGNING_IDENTITY"
}

codesign_one() {
  codesign --force --timestamp --options runtime --sign "$APPLE_SIGNING_IDENTITY" "$@"
}

sign_app() {
  [ -f "$entitlements" ] || die "entitlements file not found: $entitlements"
  if [ -n "${APPLE_CERTIFICATE:-}" ]; then
    import_certificate
  else
    [ -n "${APPLE_SIGNING_IDENTITY:-}" ] || die "set APPLE_CERTIFICATE (and APPLE_CERTIFICATE_PASSWORD) or APPLE_SIGNING_IDENTITY"
  fi

  step "Signing $app with the hardened runtime"
  # Inside out: nested code first (frameworks, helpers, extra executables),
  # then the app itself with its entitlements. `codesign --deep` is not used
  # for signing; Apple advises against it.
  local nested=()
  while IFS= read -r -d '' item; do
    nested+=("$item")
  done < <(find "$app/Contents" -depth \( -name '*.framework' -o -name '*.dylib' -o -name '*.bundle' -o -name '*.xpc' -o -name '*.app' \) -print0 2>/dev/null)
  local main_exe
  main_exe="$app/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")"
  while IFS= read -r -d '' item; do
    [ "$item" = "$main_exe" ] || nested+=("$item")
  done < <(find "$app/Contents/MacOS" -type f -perm -u+x -print0)

  if [ ${#nested[@]} -gt 0 ]; then
    for item in "${nested[@]}"; do
      echo "  $item"
      codesign_one "$item"
    done
  fi
  codesign_one --entitlements "$entitlements" "$app"
}

notarize_app() {
  local auth=()
  # Same order as Tauri: the Apple ID first, then the API key.
  if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    auth=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
  elif [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ] && [ -n "${APPLE_API_KEY_PATH:-}" ]; then
    [ -f "$APPLE_API_KEY_PATH" ] || die "APPLE_API_KEY_PATH does not point to a file"
    auth=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
  else
    die "set APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID, or APPLE_API_KEY, APPLE_API_ISSUER and APPLE_API_KEY_PATH, to notarize"
  fi

  step "Submitting to Apple's notary service (this can take a few minutes)"
  local upload result
  upload="$workdir/$(basename "$app" .app)-notarize.zip"
  result="$workdir/notarize.json"
  ditto -c -k --keepParent "$app" "$upload"
  xcrun notarytool submit "$upload" "${auth[@]}" --wait --output-format json >"$result" || true
  cat "$result"
  echo

  local status id
  status="$(plutil -extract status raw -o - "$result" 2>/dev/null || echo unknown)"
  id="$(plutil -extract id raw -o - "$result" 2>/dev/null || echo "")"
  if [ "$status" != Accepted ]; then
    if [ -n "$id" ]; then
      echo "Notarization log:" >&2
      xcrun notarytool log "$id" "${auth[@]}" >&2 || true
    fi
    die "notarization finished with status: $status"
  fi

  step "Stapling the notarization ticket"
  xcrun stapler staple "$app"
}

verify_app() {
  step "Verifying the signature"
  codesign --verify --deep --strict --verbose=2 "$app"

  local details
  details="$(codesign --display --verbose=4 "$app" 2>&1)"
  echo "$details" | grep -E '^(Identifier|Authority|TeamIdentifier|Timestamp|flags)=' || true
  echo "$details" | grep -q '^Authority=Developer ID Application:' ||
    die "not signed with a Developer ID Application certificate"
  echo "$details" | grep -Eq 'flags=.*runtime' || die "the hardened runtime is not enabled"
  echo "$details" | grep -q '^Timestamp=' || die "the signature has no secure timestamp"

  step "Checking the notarization ticket"
  if ! xcrun stapler validate "$app"; then
    echo "No ticket stapled yet; asking Apple for it."
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
  fi

  step "Asking Gatekeeper"
  spctl --assess --type execute -vvv "$app"
}

case "$mode" in
  full)
    sign_app
    notarize_app
    verify_app
    ;;
  sign)
    sign_app
    step "Verifying the signature"
    codesign --verify --deep --strict --verbose=2 "$app"
    ;;
  verify)
    verify_app
    ;;
esac

if [ -n "$zip_out" ]; then
  step "Writing $zip_out"
  rm -f "$zip_out"
  ditto -c -k --keepParent "$app" "$zip_out"
fi

echo
echo "Done: $app"
