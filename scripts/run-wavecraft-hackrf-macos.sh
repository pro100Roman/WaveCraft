#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This launcher is only needed on macOS." >&2
    exit 1
fi

script_directory="$(cd "$(dirname "$0")" && pwd)"
project_root="$(cd "$script_directory/.." && pwd)"
shim_source="$script_directory/macos-libhackrf-no-detach.c"
shim_directory="$project_root/target/wavecraft-macos"
shim_library="$shim_directory/libwavecraft-libusb-no-detach.dylib"

mkdir -p "$shim_directory"
if [[ ! -f "$shim_library" || "$shim_source" -nt "$shim_library" ]]; then
    xcrun clang \
        -dynamiclib \
        -undefined dynamic_lookup \
        -o "$shim_library" \
        "$shim_source"
    codesign --force --sign - "$shim_library"
fi

libusb_library=""
for candidate in \
    /opt/homebrew/lib/libusb-1.0.dylib \
    /usr/local/lib/libusb-1.0.dylib
do
    if [[ -r "$candidate" ]]; then
        libusb_library="$candidate"
        break
    fi
done

if [[ -z "$libusb_library" ]]; then
    echo "libusb is unavailable; install HackRF host tools with Homebrew." >&2
    exit 1
fi

if [[ -n "${DYLD_INSERT_LIBRARIES:-}" ]]; then
    export DYLD_INSERT_LIBRARIES="$libusb_library:$shim_library:$DYLD_INSERT_LIBRARIES"
else
    export DYLD_INSERT_LIBRARIES="$libusb_library:$shim_library"
fi

cd "$project_root"
exec cargo run -p wavecraft-gui "$@"
