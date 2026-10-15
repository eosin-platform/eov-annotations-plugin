#!/usr/bin/env python3
"""Tests of scripts/package_eop.py with a stand-in library."""

from __future__ import annotations

import tarfile
import tempfile
import unittest
from pathlib import Path

from package_eop import PackageError, build_package, validate_manifest

ROOT = Path(__file__).resolve().parents[1]


class PackageTests(unittest.TestCase):
    def test_the_manifest_is_valid(self) -> None:
        manifest = validate_manifest(ROOT)
        self.assertEqual(manifest["id"], "annotations")
        self.assertEqual(manifest["api"], ">=1.0, <2")

    def test_packages_are_complete_and_reproducible(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            library = directory / "libannotations.so"
            library.write_bytes(b"\x7fELF stand-in")
            first, second = directory / "a.eop", directory / "b.eop"
            build_package(ROOT, library, first)
            build_package(ROOT, library, second)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with tarfile.open(first) as archive:
                members = {m.name: m for m in archive.getmembers()}
            self.assertIn("plugin.toml", members)
            self.assertIn("libannotations.so", members)
            self.assertIn("ui/annotations-sidebar.slint", members)
            self.assertIn("ui/import-dialog.slint", members)
            self.assertTrue(any(name.startswith("ui/icons/") for name in members))
            for member in members.values():
                self.assertTrue(member.isfile() or member.isdir(), member.name)
                self.assertFalse(member.name.startswith(("/", "..")), member.name)
                self.assertEqual((member.uid, member.gid), (0, 0))

    def test_a_wrongly_named_library_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            library = directory / "libsomething.so"
            library.write_bytes(b"x")
            with self.assertRaisesRegex(PackageError, "must be named"):
                build_package(ROOT, library, directory / "x.eop")


if __name__ == "__main__":
    unittest.main()
