#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

from release_infrastructure import (
    ReleaseError,
    artifact_specs,
    render_manifest,
    stage_artifacts,
    validate_manifest,
    validate_source,
)


class AnnotationsReleaseInfrastructureTests(unittest.TestCase):
    def test_source_metadata_is_consistent(self) -> None:
        self.assertEqual(validate_source(Path(__file__).parents[1], "0.2.2"), ">=0.4.1")

    def test_manifest_uses_staged_hashes_and_immutable_urls(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            specs = artifact_specs("annotations", "0.2.2")
            artifacts = root / "artifacts"
            for index, spec in enumerate(specs):
                source = artifacts / str(index) / spec.filename
                source.parent.mkdir(parents=True)
                source.write_bytes(spec.filename.encode("ascii"))
                digest = hashlib.sha256(source.read_bytes()).hexdigest()
                (source.parent / f"{spec.filename}.sha256").write_text(
                    f"{digest}  {spec.filename}\n", encoding="ascii"
                )
            staged = stage_artifacts(artifacts, root / "staging", specs)
            content = render_manifest(
                "eosin-platform/eov-annotations-plugin",
                "0.2.2",
                ">=0.4.1",
                specs,
                staged,
            )
            manifest = root / "release.toml"
            manifest.write_text(content, encoding="utf-8")
            import tomllib

            data = tomllib.loads(content)
            validate_manifest(
                data,
                "eosin-platform/eov-annotations-plugin",
                "0.2.2",
                ">=0.4.1",
                specs,
            )
            self.assertNotIn("/latest/", content)
            self.assertEqual(len(manifest.read_text(encoding="utf-8")), len(content))

    def test_missing_artifact_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            specs = artifact_specs("annotations", "0.2.2")
            with self.assertRaisesRegex(ReleaseError, "missing release artifacts"):
                stage_artifacts(root, root / "staging", specs)


if __name__ == "__main__":
    unittest.main()
