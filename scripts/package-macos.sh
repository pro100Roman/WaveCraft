#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd "$(dirname "$0")" && pwd)"
project_root="$(cd "$script_directory/.." && pwd)"
bundle="$project_root/target/release/bundle/osx/WaveCraft.app"
contents="$bundle/Contents"

case "$(uname -m)" in
    arm64)
        default_deployment_target="11.0"
        ;;
    x86_64)
        default_deployment_target="10.15"
        ;;
    *)
        echo "unsupported macOS architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-$default_deployment_target}"

cd "$project_root"
cargo build --release --locked -p wavecraft-gui

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"
install -m 755 target/release/wavecraft-gui "$contents/MacOS/WaveCraft"
cp packaging/macos/Info.plist "$contents/Info.plist"
cp -R resources/fpga "$contents/Resources/fpga"
/usr/libexec/PlistBuddy -c \
    "Set :LSMinimumSystemVersion $MACOSX_DEPLOYMENT_TARGET" \
    "$contents/Info.plist"

if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$bundle"
fi

echo "$bundle"
