#!/usr/bin/env python3
"""Tests of scripts/package_eop.py with a stand-in library."""

from __future__ import annotations

import re
import shutil
import tarfile
import tempfile
import unittest
from pathlib import Path

from package_eop import PackageError, build_package, translation_files, validate_manifest

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
            # The catalogs, never the template or .gitkeep.
            catalogs = [name for name in members if name.startswith("translations")]
            for name in catalogs:
                self.assertTrue(
                    name == "translations"
                    or name.endswith("/LC_MESSAGES")
                    or re.fullmatch(r"translations/[^/]+(/LC_MESSAGES(/annotations\.po)?)?", name),
                    name,
                )
            self.assertFalse(any(name.endswith((".pot", ".gitkeep")) for name in catalogs))
            for member in members.values():
                self.assertTrue(member.isfile() or member.isdir(), member.name)
                self.assertFalse(member.name.startswith(("/", "..")), member.name)
                self.assertEqual((member.uid, member.gid), (0, 0))

    def test_translation_catalogs_are_packaged_without_the_template(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            root = directory / "plugin"
            shutil.copytree(ROOT / "ui", root / "ui")
            shutil.copy(ROOT / "plugin.toml", root / "plugin.toml")
            translations = root / "translations"
            for tag in ("de", "pt-BR"):
                (translations / tag / "LC_MESSAGES").mkdir(parents=True)
                (translations / tag / "LC_MESSAGES" / "annotations.po").write_text(
                    f'msgid ""\nmsgstr ""\n"Language: {tag}\\n"\n', encoding="utf-8"
                )
            (translations / ".gitkeep").write_text("", encoding="utf-8")
            (translations / "annotations.pot").write_text('msgid ""\nmsgstr ""\n', encoding="utf-8")
            (translations / "de" / "LC_MESSAGES" / "notes.txt").write_text("x", encoding="utf-8")
            library = directory / "libannotations.so"
            library.write_bytes(b"\x7fELF stand-in")
            build_package(root, library, directory / "a.eop")
            with tarfile.open(directory / "a.eop") as archive:
                names = {m.name for m in archive.getmembers() if m.isfile()}
            self.assertEqual(
                sorted(name for name in names if name.startswith("translations/")),
                [
                    "translations/de/LC_MESSAGES/annotations.po",
                    "translations/pt-BR/LC_MESSAGES/annotations.po",
                ],
            )

    def test_catalogs_outside_the_plugin_are_refused(self) -> None:
        for catalogs in ("../elsewhere", "/etc", ""):
            manifest = {"id": "annotations", "i18n": {"catalogs": catalogs}}
            with self.assertRaisesRegex(PackageError, "i18n.catalogs"):
                translation_files(ROOT, manifest)
        # Untranslated plugins package no catalogs.
        self.assertEqual(translation_files(ROOT, {"id": "annotations"}), [])

    def test_a_wrongly_named_library_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            library = directory / "libsomething.so"
            library.write_bytes(b"x")
            with self.assertRaisesRegex(PackageError, "must be named"):
                build_package(ROOT, library, directory / "x.eop")


if __name__ == "__main__":
    unittest.main()
