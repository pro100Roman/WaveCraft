#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd "$(dirname "$0")" && pwd)"
project_root="$(cd "$script_directory/.." && pwd)"
bundle="$project_root/target/release/bundle/osx/WaveCraft.app"
contents="$bundle/Contents"
shim_source="$script_directory/macos-libhackrf-no-detach.c"
shim_build_directory="$project_root/target/release/macos-shim"
shim_library="$shim_build_directory/libwavecraft-libusb-no-detach.dylib"
probe_helper_manifest="$project_root/crates/wavecraft-probe-rs/Cargo.toml"
probe_helper_target_directory="$project_root/target/probe-helper"
probe_helper_binary="$probe_helper_target_directory/release/wavecraft-probe-helper"
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

    cargo build --release --locked \
        --manifest-path "$probe_helper_manifest" \
        --target-dir "$probe_helper_target_directory" \
        --bin wavecraft-probe-helper \
        --target aarch64-apple-darwin
    cargo build --release --locked \
        --manifest-path "$probe_helper_manifest" \
        --target-dir "$probe_helper_target_directory" \
        --bin wavecraft-probe-helper \
        --target x86_64-apple-darwin
    mkdir -p "$probe_helper_target_directory/release"
    lipo -create \
        "$probe_helper_target_directory/aarch64-apple-darwin/release/wavecraft-probe-helper" \
        "$probe_helper_target_directory/x86_64-apple-darwin/release/wavecraft-probe-helper" \
        -output "$probe_helper_binary"

    mkdir -p "$shim_build_directory"
    xcrun clang -arch arm64 \
        -dynamiclib \
        -undefined dynamic_lookup \
        -o "$shim_build_directory/libwavecraft-libusb-no-detach-arm64.dylib" \
        "$shim_source"
    xcrun clang -arch x86_64 \
        -dynamiclib \
        -undefined dynamic_lookup \
        -o "$shim_build_directory/libwavecraft-libusb-no-detach-x86_64.dylib" \
        "$shim_source"
    lipo -create \
        "$shim_build_directory/libwavecraft-libusb-no-detach-arm64.dylib" \
        "$shim_build_directory/libwavecraft-libusb-no-detach-x86_64.dylib" \
        -output "$shim_library"
else
    cargo build --release --locked -p wavecraft-gui
    cargo build --release --locked \
        --manifest-path "$probe_helper_manifest" \
        --target-dir "$probe_helper_target_directory" \
        --bin wavecraft-probe-helper
    mkdir -p "$shim_build_directory"
    xcrun clang \
        -dynamiclib \
        -undefined dynamic_lookup \
        -o "$shim_library" \
        "$shim_source"
fi

rm -rf "$bundle"
mkdir -p "$contents/Frameworks" "$contents/MacOS" "$contents/Resources"
install -m 755 packaging/macos/WaveCraft "$contents/MacOS/WaveCraft"
install -m 755 target/release/wavecraft-gui "$contents/MacOS/WaveCraft-bin"
install -m 755 "$probe_helper_binary" "$contents/MacOS/wavecraft-probe-helper"
install -m 755 "$shim_library" \
    "$contents/Frameworks/libwavecraft-libusb-no-detach.dylib"
cp packaging/macos/Info.plist "$contents/Info.plist"
cp -R resources/fpga "$contents/Resources/fpga"
/usr/libexec/PlistBuddy -c \
    "Set :LSMinimumSystemVersion $MACOSX_DEPLOYMENT_TARGET" \
    "$contents/Info.plist"

if command -v codesign >/dev/null 2>&1; then
    codesign --force --sign - \
        "$contents/Frameworks/libwavecraft-libusb-no-detach.dylib"
    codesign --force --sign - "$contents/MacOS/wavecraft-probe-helper"
    codesign --force --sign - "$contents/MacOS/WaveCraft-bin"
    codesign --force --deep --sign - "$bundle"
fi

echo "$bundle"
