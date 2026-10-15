#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import tempfile
import tomllib
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

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "eosin-platform/eov-annotations-plugin"


def source_version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as cargo:
        return tomllib.load(cargo)["package"]["version"]


def source_environment() -> str:
    with (ROOT / "plugin.toml").open("rb") as manifest:
        return tomllib.load(manifest)["environment"]["version"]


class AnnotationsReleaseInfrastructureTests(unittest.TestCase):
    def test_source_metadata_is_consistent(self) -> None:
        version = source_version()
        self.assertEqual(validate_source(ROOT, version), source_environment())
        self.assertEqual(source_environment(), ">=1.0.0, <2.0.0")

    def test_a_different_version_is_refused(self) -> None:
        with self.assertRaisesRegex(ReleaseError, "disagree"):
            validate_source(ROOT, "0.0.1")

    def test_manifest_uses_staged_hashes_and_immutable_urls(self) -> None:
        version = source_version()
        environment = source_environment()
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            specs = artifact_specs("annotations", version)
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
                REPOSITORY,
                version,
                environment,
                specs,
                staged,
                "annotations",
                "Annotations",
                "Official annotations plugin",
            )
            manifest = root / "release.toml"
            manifest.write_text(content, encoding="utf-8")
            data = tomllib.loads(content)
            validate_manifest(data, REPOSITORY, version, environment, specs)
            self.assertNotIn("/latest/", content)
            self.assertIn("[manifest]", content)
            self.assertIn("[plugin]", content)
            self.assertIn('id = "annotations"', content)
            self.assertIn(f"annotations-v{version}-linux-x86_64.eop", content)
            self.assertEqual(len(manifest.read_text(encoding="utf-8")), len(content))

    def test_missing_artifact_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            specs = artifact_specs("annotations", source_version())
            with self.assertRaisesRegex(ReleaseError, "missing release artifacts"):
                stage_artifacts(root, root / "staging", specs)


if __name__ == "__main__":
    unittest.main()
