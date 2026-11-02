#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd "$(dirname "$0")" && pwd)"
project_root="$(cd "$script_directory/.." && pwd)"
bundle="$project_root/target/release/bundle/osx/WaveCraft.app"
contents="$bundle/Contents"
universal=false

if [[ $# -gt 1 ]]; then
    echo "usage: $0 [--universal]" >&2
    exit 2
fi

case "${1:-}" in
    "")
        ;;
    --universal)
        universal=true
        ;;
    *)
        echo "usage: $0 [--universal]" >&2
        exit 2
        ;;
esac

if "$universal"; then
    default_deployment_target="11.0"
else
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
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-$default_deployment_target}"

cd "$project_root"
if "$universal"; then
    cargo build --release --locked -p wavecraft-gui \
        --target aarch64-apple-darwin
    cargo build --release --locked -p wavecraft-gui \
        --target x86_64-apple-darwin
    mkdir -p target/release
    lipo -create \
        target/aarch64-apple-darwin/release/wavecraft-gui \
        target/x86_64-apple-darwin/release/wavecraft-gui \
        -output target/release/wavecraft-gui
else
    cargo build --release --locked -p wavecraft-gui
fi

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
