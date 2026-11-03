import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from set_release_version import normalize_release_version, set_release_version


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


if __name__ == "__main__":
    unittest.main()
