#!/bin/bash
# Builds "Usage Lens.app" for the architecture of this Mac.
# Usage: ./build-app.sh [output path]   (the output path must not exist yet)
set -euo pipefail
project_dir="$(cd "$(dirname "$0")" && pwd)"
app_path="${1:-$project_dir/dist/Usage Lens.app}"
if [[ "$(uname -s)" != Darwin ]]; then echo "Building the AppKit application requires macOS." >&2; exit 1; fi
if [[ -e "$app_path" ]]; then echo "The destination already exists: $app_path" >&2; exit 1; fi
cd "$project_dir"
# Keep local folder names (home directory, toolchain locations) out of the binary.
# When several prefixes match, the last one wins, so the most specific goes last.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${HOME}=~ --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup --remap-path-prefix=${project_dir}=."
MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --locked --release
mkdir -p "$app_path/Contents/MacOS" "$app_path/Contents/Resources"
cp Info.plist "$app_path/Contents/Info.plist"
cp target/release/usage-lens-mac "$app_path/Contents/MacOS/usage-lens-mac"
cp assets/AppIcon.icns "$app_path/Contents/Resources/AppIcon.icns"
codesign --force --sign - "$app_path"
echo "Built $app_path"
