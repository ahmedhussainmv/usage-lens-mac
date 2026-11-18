#!/bin/bash
# Signs an app bundle with a Developer ID Application certificate, notarizes it with Apple
# and staples the ticket. Used by the Release workflow when the signing secrets exist.
#
# Required environment variables (GitHub secrets in CI; never commit them):
#   MACOS_CERT_P12       base64 of the exported Developer ID Application certificate (.p12)
#   MACOS_CERT_PASSWORD  the password chosen when exporting the .p12
#   NOTARY_APPLE_ID      Apple ID used for notarization
#   NOTARY_PASSWORD      an app-specific password for that Apple ID
#   NOTARY_TEAM_ID       the 10-character Apple developer Team ID
set -euo pipefail
app="${1:?usage: sign-and-notarize.sh <path to .app>}"
for v in MACOS_CERT_P12 MACOS_CERT_PASSWORD NOTARY_APPLE_ID NOTARY_PASSWORD NOTARY_TEAM_ID; do
  [[ -n "${!v:-}" ]] || { echo "Missing $v" >&2; exit 1; }
done

work="$(mktemp -d)"
keychain="$work/signing.keychain-db"
keychain_password="$(uuidgen)"
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

echo "$MACOS_CERT_P12" | base64 --decode > "$work/cert.p12"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$work/cert.p12" -P "$MACOS_CERT_PASSWORD" -A -t cert -f pkcs12 -k "$keychain"
security set-key-partition-list -S apple-tool:,apple: -k "$keychain_password" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')

identity="$(security find-identity -v -p codesigning "$keychain" | grep "Developer ID Application" | head -1 | sed -E 's/^ *[0-9]+\) ([0-9A-F]+) .*/\1/')"
[[ -n "$identity" ]] || { echo "No Developer ID Application identity in the certificate." >&2; exit 1; }

# Hardened runtime and a secure timestamp are required for notarization.
codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"

zip="$work/notarize.zip"
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" \
  --apple-id "$NOTARY_APPLE_ID" --password "$NOTARY_PASSWORD" --team-id "$NOTARY_TEAM_ID" \
  --wait --output-format json | tee "$work/result.json"
grep -q '"status" *: *"Accepted"' "$work/result.json" || { echo "Notarization was not accepted." >&2; exit 1; }

xcrun stapler staple "$app"
xcrun stapler validate "$app"
spctl --assess --type execute --verbose=2 "$app"
echo "Signed, notarized and stapled: $app"
