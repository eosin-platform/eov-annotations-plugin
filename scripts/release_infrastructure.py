#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import tomllib
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path


class ReleaseError(Exception):
    pass


SEMVER_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")


@dataclass(frozen=True)
class ArtifactSpec:
    section: str
    filename: str


def validate_version(version: str) -> str:
    if SEMVER_RE.fullmatch(version) is None:
        raise ReleaseError(f"version must be valid SemVer: {version!r}")
    return version


def read_toml(path: Path) -> dict[str, object]:
    try:
        with path.open("rb") as input_file:
            return tomllib.load(input_file)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError(f"could not read TOML file {path}: {error}") from error


def write_atomically(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            dir=path.parent,
            prefix=f".{path.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary_file:
            temporary_path = Path(temporary_file.name)
            temporary_file.write(content)
            temporary_file.flush()
            os.fsync(temporary_file.fileno())
        os.replace(temporary_path, path)
    except OSError as error:
        if temporary_path is not None:
            try:
                temporary_path.unlink()
            except OSError:
                pass
        raise ReleaseError(f"could not write {path}: {error}") from error


def nested_table(value: dict[str, object], dotted_name: str) -> dict[str, object]:
    current: object = value
    for part in dotted_name.split("."):
        if not isinstance(current, dict) or part not in current:
            raise ReleaseError(f"manifest is missing [{dotted_name}]")
        current = current[part]
    if not isinstance(current, dict):
        raise ReleaseError(f"manifest entry [{dotted_name}] is not a table")
    return current


def artifact_specs(plugin_name: str, version: str) -> tuple[ArtifactSpec, ...]:
    if plugin_name not in {"annotations", "gamepad"}:
        raise ReleaseError(f"unsupported plugin name: {plugin_name!r}")
    return tuple(
        ArtifactSpec(section, f"{plugin_name}-v{version}-{platform}.eop")
        for section, platform in (
            ("platform.windows.x86_64", "windows-x86_64"),
            ("platform.windows.arm64", "windows-arm64"),
            ("platform.linux.arm64", "linux-arm64"),
            ("platform.linux.x86_64", "linux-x86_64"),
            ("platform.macos.arm64", "macos-arm64"),
            ("platform.macos.x86_64", "macos-x86_64"),
        )
    )


def stage_artifacts(
    artifacts_dir: Path, staging_dir: Path, specs: tuple[ArtifactSpec, ...]
) -> dict[str, Path]:
    if not artifacts_dir.is_dir():
        raise ReleaseError(f"artifact directory does not exist: {artifacts_dir}")
    expected_names = {spec.filename for spec in specs}
    artifacts: dict[str, Path] = {}
    sidecars: dict[str, Path] = {}
    for path in artifacts_dir.rglob("*"):
        if not path.is_file():
            continue
        if path.name.endswith(".sha256"):
            name = path.name.removesuffix(".sha256")
            if name not in expected_names:
                raise ReleaseError(f"unexpected checksum artifact: {path}")
            if name in sidecars:
                raise ReleaseError(f"duplicate checksum artifact: {name}")
            sidecars[name] = path
            continue
        if path.name not in expected_names:
            raise ReleaseError(f"unexpected release artifact: {path}")
        if path.name in artifacts:
            raise ReleaseError(f"duplicate release artifact: {path.name}")
        artifacts[path.name] = path
    missing = sorted(expected_names - artifacts.keys())
    if missing:
        raise ReleaseError(f"missing release artifacts: {', '.join(missing)}")

    staging_dir.mkdir(parents=True, exist_ok=True)
    staged: dict[str, Path] = {}
    for name in sorted(expected_names):
        source = artifacts[name]
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        sidecar = sidecars.get(name)
        if sidecar is not None:
            lines = [
                line.strip()
                for line in sidecar.read_text(encoding="ascii").splitlines()
                if line.strip()
            ]
            if len(lines) != 1:
                raise ReleaseError(f"checksum sidecar {sidecar} must contain one entry")
            match = re.fullmatch(r"([0-9a-fA-F]{64})\s+(.+)", lines[0])
            if (
                match is None
                or match.group(1).lower() != digest
                or match.group(2).removeprefix("*") != name
            ):
                raise ReleaseError(f"checksum sidecar {sidecar} does not match {name}")
        destination = staging_dir / name
        if destination.exists():
            raise ReleaseError(f"staging directory already contains {destination}")
        shutil.copy2(source, destination)
        if sidecar is not None:
            shutil.copy2(sidecar, staging_dir / sidecar.name)
        staged[name] = destination
    return staged


def validate_source(root: Path, version: str) -> str:
    validate_version(version)
    cargo_data = read_toml(root / "Cargo.toml")
    package = cargo_data.get("package")
    cargo_version = package.get("version") if isinstance(package, dict) else None
    plugin_data = read_toml(root / "plugin.toml")
    manifest_version = plugin_data.get("version")
    if cargo_version != version or manifest_version != version:
        raise ReleaseError(
            f"plugin versions disagree with requested {version}: "
            f"Cargo.toml={cargo_version!r}, plugin.toml={manifest_version!r}"
        )
    environment = plugin_data.get("environment")
    if not isinstance(environment, dict) or not isinstance(
        environment.get("version"), str
    ):
        raise ReleaseError("plugin.toml has no valid [environment].version")
    return environment["version"]


def immutable_url(repository: str, version: str, filename: str) -> str:
    return f"https://github.com/{repository}/releases/download/v{version}/{filename}"


def render_manifest(
    repository: str,
    version: str,
    environment: str,
    specs: tuple[ArtifactSpec, ...],
    staged: dict[str, Path],
) -> str:
    lines: list[str] = []
    for spec in specs:
        digest = hashlib.sha256(staged[spec.filename].read_bytes()).hexdigest()
        lines.extend(
            (
                f"[{spec.section}]",
                f"version = {json.dumps(version)}",
                f"sha256 = {json.dumps(digest)}",
                f"url = {json.dumps(immutable_url(repository, version, spec.filename))}",
                f"environment = {json.dumps(environment)}",
                "",
            )
        )
    return "\n".join(lines)


def validate_manifest(
    data: dict[str, object],
    repository: str,
    version: str,
    environment: str,
    specs: tuple[ArtifactSpec, ...],
) -> None:
    for spec in specs:
        entry = nested_table(data, spec.section)
        if entry.get("version") != version or entry.get("environment") != environment:
            raise ReleaseError(
                f"[{spec.section}] has inconsistent version or environment"
            )
        digest = entry.get("sha256")
        if not isinstance(digest, str) or SHA256_RE.fullmatch(digest) is None:
            raise ReleaseError(f"[{spec.section}] has an invalid SHA-256")
        if entry.get("url") != immutable_url(repository, version, spec.filename):
            raise ReleaseError(
                f"[{spec.section}] does not use an immutable release URL"
            )


def command_manifest(args: argparse.Namespace) -> None:
    version = validate_version(args.version)
    environment = validate_source(args.root, version)
    specs = artifact_specs(args.plugin_name, version)
    staged = stage_artifacts(args.artifacts_dir, args.staging_dir, specs)
    write_atomically(
        args.output,
        render_manifest(args.repository, version, environment, specs, staged),
    )
    data = read_toml(args.output)
    validate_manifest(data, args.repository, version, environment, specs)
    for spec in specs:
        expected = hashlib.sha256(staged[spec.filename].read_bytes()).hexdigest()
        if nested_table(data, spec.section)["sha256"] != expected:
            raise ReleaseError(f"[{spec.section}] hash does not match the staged file")
    print(f"generated {args.output}")


def command_verify_assets(args: argparse.Namespace) -> None:
    version = validate_version(args.version)
    environment = validate_source(args.root, version)
    specs = artifact_specs(args.plugin_name, version)
    data = read_toml(args.manifest)
    validate_manifest(data, args.repository, version, environment, specs)
    for spec in specs:
        entry = nested_table(data, spec.section)
        request = urllib.request.Request(
            entry["url"], headers={"User-Agent": "eov-plugin-release-verifier"}
        )
        digest = hashlib.sha256()
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                if response.status != 200:
                    raise ReleaseError(
                        f"asset URL returned HTTP {response.status}: {entry['url']}"
                    )
                while chunk := response.read(1024 * 1024):
                    digest.update(chunk)
        except (OSError, urllib.error.URLError) as error:
            raise ReleaseError(
                f"could not verify release asset {entry['url']}: {error}"
            ) from error
        if digest.hexdigest() != entry["sha256"]:
            raise ReleaseError(f"release asset hash mismatch for {entry['url']}")
    print(f"verified {len(specs)} published assets")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Plugin release manifest tooling")
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate-source")
    validate.add_argument("--root", type=Path, required=True)
    validate.add_argument("--version", required=True)
    validate.set_defaults(
        function=lambda args: validate_source(args.root, args.version)
    )

    manifest = subparsers.add_parser("manifest")
    manifest.add_argument("--root", type=Path, required=True)
    manifest.add_argument(
        "--plugin-name", choices=("annotations", "gamepad"), required=True
    )
    manifest.add_argument("--version", required=True)
    manifest.add_argument("--repository", required=True)
    manifest.add_argument("--artifacts-dir", type=Path, required=True)
    manifest.add_argument("--staging-dir", type=Path, required=True)
    manifest.add_argument("--output", type=Path, required=True)
    manifest.set_defaults(function=command_manifest)

    verify = subparsers.add_parser("verify-assets")
    verify.add_argument("--root", type=Path, required=True)
    verify.add_argument(
        "--plugin-name", choices=("annotations", "gamepad"), required=True
    )
    verify.add_argument("--version", required=True)
    verify.add_argument("--repository", required=True)
    verify.add_argument("--manifest", type=Path, required=True)
    verify.set_defaults(function=command_verify_assets)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        args.function(args)
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
