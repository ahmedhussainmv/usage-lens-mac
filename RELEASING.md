# Releasing

1. Update `version` in `Cargo.toml` and `CFBundleShortVersionString` in `Info.plist`, add notes to `CHANGELOG.md`, and run `cargo test --locked`.
2. Commit, then tag and push: `git tag v0.x.y && git push origin v0.x.y`.
3. The *Release* workflow builds Apple Silicon and Intel apps, zips them as `Usage-Lens-macOS-arm64.zip` and `Usage-Lens-macOS-x86_64.zip`, writes `SHA256SUMS.txt` and publishes the GitHub release.
4. In the download-site repository, update `latest.json` (and the changelog on the page) so the in-app update check sees the new version.
