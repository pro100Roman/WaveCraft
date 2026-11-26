import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from set_release_version import (
    latest_release_tag,
    normalize_release_version,
    semver_precedence,
    set_release_version,
)


class ReleaseVersionTests(unittest.TestCase):
    def test_normalizes_stable_and_prerelease_tags(self):
        self.assertEqual(normalize_release_version("v1.2.3"), ("1.2.3", "1.2.3"))
        self.assertEqual(
            normalize_release_version("2.0.0-rc.1+build.4"),
            ("2.0.0-rc.1+build.4", "2.0.0"),
        )

    def test_rejects_non_semver_release_tag(self):
        with self.assertRaisesRegex(ValueError, "is not SemVer"):
            normalize_release_version("release-latest")

    def test_orders_semver_tags_by_precedence(self):
        tags = [
            "v1.9.0",
            "v1.10.0-rc.2",
            "v1.10.0-rc.10",
            "v1.10.0",
        ]
        self.assertEqual(max(tags, key=semver_precedence), "v1.10.0")

    def test_finds_latest_semver_tag_in_git_repository(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=WaveCraft Tests",
                    "-c",
                    "user.email=tests@wavecraft.invalid",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "initial",
                ],
                cwd=root,
                check=True,
            )
            for tag in ("v0.9.0", "not-a-release", "v0.12.0", "v0.11.1"):
                subprocess.run(["git", "tag", tag], cwd=root, check=True)

            self.assertEqual(latest_release_tag(root), "v0.12.0")

    def test_updates_app_manifests_lockfile_and_macos_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for package in ("wavecraft-cli", "wavecraft-gui"):
                manifest = root / "apps" / package / "Cargo.toml"
                manifest.parent.mkdir(parents=True)
                manifest.write_text(
                    f'[package]\nname = "{package}"\nversion = "0.1.0"\n',
                    encoding="utf-8",
                )
            (root / "Cargo.lock").write_text(
                "[[package]]\n"
                'name = "wavecraft-cli"\n'
                'version = "0.1.0"\n\n'
                "[[package]]\n"
                'name = "wavecraft-gui"\n'
                'version = "0.1.0"\n',
                encoding="utf-8",
            )
            plist = root / "packaging" / "macos" / "Info.plist"
            plist.parent.mkdir(parents=True)
            plist.write_text(
                "<key>CFBundleShortVersionString</key>\n"
                "<string>0.1.0</string>\n"
                "<key>CFBundleVersion</key>\n"
                "<string>1</string>\n",
                encoding="utf-8",
            )

            version = set_release_version(root, "v0.5.0")

            self.assertEqual(version, "0.5.0")
            for package in ("wavecraft-cli", "wavecraft-gui"):
                self.assertIn(
                    'version = "0.5.0"',
                    (root / "apps" / package / "Cargo.toml").read_text(),
                )
            self.assertEqual(
                (root / "Cargo.lock").read_text().count('version = "0.5.0"'), 2
            )
            self.assertIn("<string>0.5.0</string>", plist.read_text())


class MacosPackagingVersionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.origin = self.directory / "origin"
        self.root = self.directory / "checkout"
        self.origin.mkdir()
        subprocess.run(["git", "init", "-q", str(self.origin)], check=True)
        self.git(self.origin, "-c", "user.name=WaveCraft Tests", "-c",
                 "user.email=tests@wavecraft.invalid", "commit", "--allow-empty",
                 "-qm", "initial")
        self.git(self.origin, "tag", "v0.12.0")
        subprocess.run(["git", "clone", "-q", str(self.origin), str(self.root)], check=True)
        for name in ("scripts/package-macos.sh", "scripts/set_release_version.py",
                     "apps/wavecraft-cli/Cargo.toml", "apps/wavecraft-gui/Cargo.toml",
                     "Cargo.lock", "packaging/macos/Info.plist"):
            destination = self.root / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, destination)
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        # Stop at the first Cargo build, after capturing the version Cargo sees.
        # No native compiler, Apple SDK, hardware, or dependencies are needed.
        cargo = self.bin / "cargo"
        cargo.write_text(
            f"#!{sys.executable}\n"
            "import pathlib, re\n"
            "root = pathlib.Path.cwd()\n"
            "source = (root / 'apps/wavecraft-gui/Cargo.toml').read_text()\n"
            "version = re.search(r'^version = \"([^\"]+)\"', source, re.M)[1]\n"
            "(root / 'cargo-version').write_text(version)\n"
            "raise SystemExit(97)\n",
            encoding="utf-8",
        )
        cargo.chmod(0o755)
        self.environment = os.environ.copy()
        for name in ("GITHUB_ACTIONS", "GITHUB_REF_TYPE", "GITHUB_REF_NAME"):
            self.environment.pop(name, None)
        self.environment["PATH"] = str(self.bin) + os.pathsep + os.environ["PATH"]

    @staticmethod
    def git(root, *arguments):
        return subprocess.run(["git", *arguments], cwd=root, check=True,
                              capture_output=True, text=True)

    def package(self, *arguments):
        return subprocess.run(["bash", "scripts/package-macos.sh", *arguments],
                              cwd=self.root, env=self.environment,
                              capture_output=True, text=True)

    def assert_version(self, expected):
        self.assertEqual((self.root / "cargo-version").read_text(), expected)
        for package in ("wavecraft-cli", "wavecraft-gui"):
            self.assertIn(f'version = "{expected}"',
                          (self.root / "apps" / package / "Cargo.toml").read_text())
        packages = tomllib.loads((self.root / "Cargo.lock").read_text())["package"]
        for name in ("wavecraft-cli", "wavecraft-gui"):
            self.assertEqual(next(package["version"] for package in packages
                                  if package["name"] == name), expected)
        self.assertEqual((self.root / "packaging/macos/Info.plist").read_text().count(
            f"<string>{expected}</string>"), 2)

    def test_every_packaging_run_fetches_new_tags_before_cargo(self):
        for arguments in ((), ("--universal",)):
            for version in ("0.12.1", "0.13.0", "0.14.0"):
                with self.subTest(arguments=arguments, version=version):
                    self.git(self.origin, "tag", "--force", f"v{version}")
                    result = self.package(*arguments)
                    self.assertEqual(result.returncode, 97, result.stderr)
                    self.assert_version(version)
            # Start the second build mode from the old tags again.
            for version in ("0.12.1", "0.13.0", "0.14.0"):
                self.git(self.origin, "tag", "-d", f"v{version}")
                self.git(self.root, "tag", "-d", f"v{version}")

    def test_ci_release_uses_triggering_tag_instead_of_newest_tag(self):
        self.git(self.root, "tag", "v9.0.0")
        self.git(self.root, "remote", "set-url", "origin", str(self.directory / "missing"))
        self.environment.update(GITHUB_ACTIONS="true", GITHUB_REF_TYPE="tag",
                                GITHUB_REF_NAME="v0.12.0")
        result = self.package("--universal")
        self.assertEqual(result.returncode, 97, result.stderr)
        self.assert_version("0.12.0")

    def test_failed_fetch_stops_before_cargo_and_version_changes(self):
        original = (self.root / "apps/wavecraft-gui/Cargo.toml").read_text()
        self.git(self.root, "remote", "set-url", "origin", str(self.directory / "missing"))
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "cargo-version").exists())
        self.assertEqual((self.root / "apps/wavecraft-gui/Cargo.toml").read_text(), original)


if __name__ == "__main__":
    unittest.main()
