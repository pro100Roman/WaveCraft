#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd "$(dirname "$0")" && pwd)"
project_root="$(cd "$script_directory/.." && pwd)"
bundle="$project_root/target/release/bundle/osx/WaveCraft.app"
contents="$bundle/Contents"

cd "$project_root"
cargo build --release --locked -p wavecraft-gui

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"
install -m 755 target/release/wavecraft-gui "$contents/MacOS/WaveCraft"
cp packaging/macos/Info.plist "$contents/Info.plist"
cp -R resources/fpga "$contents/Resources/fpga"

if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$bundle"
fi

echo "$bundle"
