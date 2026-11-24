#!/usr/bin/env python3
"""Set release-facing WaveCraft versions from a Git tag."""

from __future__ import annotations

import argparse
import re
import subprocess
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


def semver_precedence(tag: str) -> tuple[object, ...]:
    match = SEMVER_TAG.fullmatch(tag.strip())
    if match is None:
        raise ValueError(
            f"release tag {tag!r} is not SemVer; expected vMAJOR.MINOR.PATCH"
        )
    major, minor, patch, prerelease, _build = match.groups()
    if prerelease is None:
        prerelease_key: tuple[object, ...] = (1,)
    else:
        identifiers = tuple(
            (0, int(identifier)) if identifier.isdigit() else (1, identifier)
            for identifier in prerelease.split(".")
        )
        prerelease_key = (0, identifiers)
    return int(major), int(minor), int(patch), prerelease_key


def latest_release_tag(root: Path) -> str:
    completed = subprocess.run(
        ["git", "tag", "--list"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    tags = [
        tag.strip()
        for tag in completed.stdout.splitlines()
        if SEMVER_TAG.fullmatch(tag.strip())
    ]
    if not tags:
        raise RuntimeError(
            "no SemVer release tags found; fetch tags from origin before building"
        )
    return max(tags, key=semver_precedence)


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
    parser.add_argument(
        "tag",
        nargs="?",
        help="release tag, for example v0.5.0 (defaults to the latest fetched tag)",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (defaults to the parent of scripts/)",
    )
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    tag = arguments.tag or latest_release_tag(root)
    version = set_release_version(root, tag)
    print(f"Set WaveCraft application version to {version} from {tag}")


if __name__ == "__main__":
    main()
