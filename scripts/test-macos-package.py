#!/usr/bin/env python3
"""Offline package contracts; stub Mach-O bytes are fixtures, never executed."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import struct
import tempfile
import unittest
import zipfile

SOURCE = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("mac_package", SOURCE / "scripts/package-macos.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vibdock-mac-package-")
        self.root = Path(self.temp.name)
        for name in ("README.md", "LICENSE", "Cargo.toml"):
            shutil.copy2(SOURCE / name, self.root / name)
        shutil.copytree(SOURCE / "assets", self.root / "assets")
        (self.root / "docs").mkdir()
        (self.root / "docs/note.md").write_text("Explicit offline packaging fixture")
        self.binary = self.root / "bin"
        self.binary.mkdir()
        for name in ("vibdock", "vibdock-launcher"):
            path = self.binary / name
            path.write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 2, 0, 0, 0, 0))
            path.chmod(0o755)

    def tearDown(self):
        self.temp.cleanup()

    def package(self, tag="candidate-fixture", target="aarch64-apple-darwin"):
        return MODULE.package(self.root, self.binary, self.root / "out", tag, target)

    def test_dual_entry_bundle_plist_original_art_and_no_credentials(self):
        result = self.package()
        with zipfile.ZipFile(result["archive"]) as archive:
            names = archive.namelist()
            prefix = "VibDock.app/Contents/"
            for name in ("MacOS/vibdock", "MacOS/vibdock-launcher", "Resources/vibdock.icns",
                         "Resources/LICENSE", "Resources/docs/note.md"):
                self.assertIn(prefix + name, names)
            metadata = plistlib.loads(archive.read(prefix + "Info.plist"))
            self.assertEqual(metadata["CFBundleExecutable"], "vibdock-launcher")
            self.assertEqual(metadata["CFBundleIdentifier"], "com.loonghao.vibdock")
            self.assertFalse(metadata["LSUIElement"])
            self.assertNotIn("LSMultipleInstancesProhibited", metadata)
            for key in ("NSMicrophoneUsageDescription", "NSAppleEventsUsageDescription"):
                self.assertNotIn(key, metadata)
            self.assertFalse(any("service.json" in name for name in names))
            if os.name != "nt":
                self.assertTrue(archive.getinfo(prefix + "MacOS/vibdock-launcher").external_attr >> 16 & 0o111)
        self.assertFalse(result["notarized"])

    def test_unsafe_tags_rejected_before_output_write(self):
        for tag in ("../escape", "/tmp/escape", "a/b", "a\\b", "..", "", "--bad"):
            with self.assertRaises(ValueError):
                self.package(tag=tag)
            self.assertFalse((self.root / "out").exists())

    def test_architecture_and_missing_launcher_rejected(self):
        with self.assertRaises(ValueError):
            self.package(target="x86_64-apple-darwin")
        (self.binary / "vibdock-launcher").unlink()
        with self.assertRaises(ValueError):
            self.package()

    def test_changed_original_art_rejected(self):
        (self.root / "assets/brand/vibdock.icns").write_bytes(b"changed")
        with self.assertRaises(ValueError):
            self.package()

    def test_changed_png_chunk_hash_rejected(self):
        path = self.root / "assets/brand/MACOS-PNG-SHA256.json"
        hashes = json.loads(path.read_text())
        hashes["256"] = "0" * 64
        path.write_text(json.dumps(hashes))
        with self.assertRaises(ValueError):
            self.package()

    def test_existing_output_is_preserved(self):
        output = self.root / "out"
        output.mkdir()
        marker = output / "keep"
        marker.write_text("user content")
        with self.assertRaises(FileExistsError):
            self.package()
        self.assertEqual(marker.read_text(), "user content")

    @unittest.skipIf(os.name == "nt", "Windows symlink privilege is not requested")
    def test_resource_symlink_is_rejected(self):
        (self.root / "docs/link").symlink_to(self.root / "LICENSE")
        with self.assertRaises(ValueError):
            self.package()

    @unittest.skipIf(os.name == "nt", "Windows symlink privilege is not requested")
    def test_top_level_symlinks_are_rejected(self):
        for name in ("docs", "README.md", "assets"):
            original = self.root / name
            outside = self.root / (name.replace(".", "_") + "-outside")
            original.rename(outside)
            original.symlink_to(outside, target_is_directory=outside.is_dir())
            with self.assertRaises(ValueError):
                self.package()
            original.unlink()
            outside.rename(original)


if __name__ == "__main__":
    unittest.main()
