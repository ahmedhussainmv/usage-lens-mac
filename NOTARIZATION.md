# Signing and notarizing releases

Releases are signed ad hoc by default, which makes macOS show a one-time "can't be verified" warning. If the five secrets below exist on the repository, the *Release* workflow instead signs with a Developer ID Application certificate, notarizes the app with Apple, staples the ticket and publishes that build. Nothing else changes.

Note that a Developer ID signature publicly shows the name and Team ID of the Apple developer account it belongs to.

## One-time setup

1. **Create the certificate.** In Xcode: Settings → Accounts → select the team → Manage Certificates → **+** → *Developer ID Application*. (Or create it at developer.apple.com → Certificates.)
2. **Export it.** In Keychain Access, find *Developer ID Application: …* under My Certificates, right-click → Export, save as a `.p12` and choose a password.
3. **Make an app-specific password** at appleid.apple.com → Sign-In and Security → App-Specific Passwords.
4. **Find your Team ID** at developer.apple.com → Membership details.
5. **Add the secrets** from your own terminal (each command prompts for or reads the value; do not paste secrets into issues or chats):

```sh
base64 -i DeveloperID.p12 | gh secret set MACOS_CERT_P12 --repo OWNER/usage-lens-mac
gh secret set MACOS_CERT_PASSWORD --repo OWNER/usage-lens-mac
gh secret set NOTARY_APPLE_ID     --repo OWNER/usage-lens-mac
gh secret set NOTARY_PASSWORD     --repo OWNER/usage-lens-mac
gh secret set NOTARY_TEAM_ID      --repo OWNER/usage-lens-mac
```

Then delete the `.p12` file from disk.

## Releasing

Follow [RELEASING.md](RELEASING.md) and tag a **new** version; re-running an old tag uses that tag's workflow. Afterwards check the build logs for "Signed, notarized and stapled", and on a Mac confirm with:

```sh
spctl --assess --type execute --verbose=2 "/Applications/Usage Lens.app"
xcrun stapler validate "/Applications/Usage Lens.app"
```
