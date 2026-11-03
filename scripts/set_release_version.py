#!/usr/bin/env python3
"""Set release-facing WaveCraft versions from a Git tag."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


SEMVER_TAG = re.compile(
    r"^v?(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
APPLICATION_PACKAGES = ("wavecraft-cli", "wavecraft-gui")


def normalize_release_version(tag: str) -> tuple[str, str]:
    match = SEMVER_TAG.fullmatch(tag.strip())
    if match is None:
        raise ValueError(
            f"release tag {tag!r} is not SemVer; expected vMAJOR.MINOR.PATCH"
        )
    major, minor, patch, prerelease, build = match.groups()
    core = f"{major}.{minor}.{patch}"
    version = core
    if prerelease:
        version += f"-{prerelease}"
    if build:
        version += f"+{build}"
    return version, core


def replace_once(path: Path, pattern: str, replacement: str) -> None:
    source = path.read_text(encoding="utf-8")
    updated, count = re.subn(pattern, replacement, source, count=1, flags=re.MULTILINE)
    if count != 1:
        raise RuntimeError(f"could not find exactly one version field in {path}")
    path.write_text(updated, encoding="utf-8")


def set_release_version(root: Path, tag: str) -> str:
    version, bundle_version = normalize_release_version(tag)
    for package in APPLICATION_PACKAGES:
        manifest = root / "apps" / package / "Cargo.toml"
        replace_once(
            manifest,
            r'^(version\s*=\s*")[^"]+(")',
            rf"\g<1>{version}\g<2>",
        )
        replace_once(
            root / "Cargo.lock",
            rf'(^\[\[package\]\]\nname = "{re.escape(package)}"\nversion = ")[^"]+(")',
            rf"\g<1>{version}\g<2>",
        )

    plist = root / "packaging" / "macos" / "Info.plist"
    replace_once(
        plist,
        r"(<key>CFBundleShortVersionString</key>\s*<string>)[^<]+(</string>)",
        rf"\g<1>{bundle_version}\g<2>",
    )
    replace_once(
        plist,
        r"(<key>CFBundleVersion</key>\s*<string>)[^<]+(</string>)",
        rf"\g<1>{bundle_version}\g<2>",
    )
    return version


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("tag", help="release tag, for example v0.5.0")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (defaults to the parent of scripts/)",
    )
    arguments = parser.parse_args()
    version = set_release_version(arguments.root.resolve(), arguments.tag)
    print(f"Set WaveCraft application version to {version}")


if __name__ == "__main__":
    main()
