#!/usr/bin/env python3

from __future__ import annotations

import base64
import binascii
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Platform:
    section: str
    asset_template: str


class ReleaseError(Exception):
    pass


PLUGIN_NAME = "annotations"
PLUGIN_REPOSITORY = "eosin-platform/eov-annotations-plugin"
PLUGIN_ROOT = Path(__file__).resolve().parents[1]
OUTPUT_PATH = PLUGIN_ROOT / "release.toml"

PLATFORMS = (
    Platform(
        "platform.windows.x86_64",
        "annotations-{tag}-windows-x86_64.eop",
    ),
    Platform(
        "platform.windows.arm64",
        "annotations-{tag}-windows-arm64.eop",
    ),
    Platform(
        "platform.linux.arm64",
        "annotations-{tag}-linux-arm64.eop",
    ),
    Platform(
        "platform.linux.x86_64",
        "annotations-{tag}-linux-x86_64.eop",
    ),
    Platform(
        "platform.macos.arm64",
        "annotations-{tag}-macos-arm64.eop",
    ),
    Platform(
        "platform.macos.x86_64",
        "annotations-{tag}-macos-x86_64.eop",
    ),
)


def fetch_latest_release(gh_path: str) -> dict[str, object]:
    try:
        result = subprocess.run(
            [gh_path, "api", f"repos/{PLUGIN_REPOSITORY}/releases/latest"],
            check=True,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError as error:
        details = "\n".join(
            output.strip()
            for output in (error.stderr, error.stdout)
            if output and output.strip()
        )
        if "404" in details or "not found" in details.lower():
            raise ReleaseError(
                f"Repository {PLUGIN_REPOSITORY} has no accessible latest release."
            ) from error
        if details:
            raise ReleaseError(
                f"GitHub API request for {PLUGIN_REPOSITORY} failed: {details}"
            ) from error
        raise ReleaseError(
            f"GitHub API request for {PLUGIN_REPOSITORY} failed with exit code "
            f"{error.returncode}."
        ) from error

    try:
        release = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError(
            f"GitHub API returned malformed JSON for {PLUGIN_REPOSITORY}: {error}"
        ) from error

    if not isinstance(release, dict):
        raise ReleaseError(
            f"GitHub API returned unexpected JSON for {PLUGIN_REPOSITORY}; "
            "expected an object."
        )
    return release


def fetch_release_environment(gh_path: str, tag: str) -> str:
    try:
        result = subprocess.run(
            [
                gh_path,
                "api",
                f"repos/{PLUGIN_REPOSITORY}/contents/plugin.toml?ref={tag}",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError as error:
        details = "\n".join(
            output.strip()
            for output in (error.stderr, error.stdout)
            if output and output.strip()
        )
        if details:
            raise ReleaseError(
                f"GitHub API request for {PLUGIN_REPOSITORY}/plugin.toml at "
                f"ref {tag} failed: {details}"
            ) from error
        raise ReleaseError(
            f"GitHub API request for {PLUGIN_REPOSITORY}/plugin.toml at ref "
            f"{tag} failed with exit code {error.returncode}."
        ) from error

    try:
        response = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError(
            f"GitHub API returned malformed plugin.toml metadata for "
            f"{PLUGIN_REPOSITORY} at ref {tag}: {error}"
        ) from error

    if not isinstance(response, dict):
        raise ReleaseError(
            f"GitHub API returned unexpected plugin.toml metadata for "
            f"{PLUGIN_REPOSITORY} at ref {tag}; expected an object."
        )

    encoded_content = response.get("content")
    if response.get("encoding") != "base64" or not isinstance(
        encoded_content, str
    ):
        raise ReleaseError(
            f"GitHub API returned plugin.toml for {PLUGIN_REPOSITORY} at ref "
            f"{tag} without base64 content."
        )

    try:
        manifest = tomllib.loads(
            base64.b64decode(
                re.sub(r"\s+", "", encoded_content), validate=True
            ).decode("utf-8")
        )
    except (binascii.Error, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError(
            f"GitHub API returned invalid plugin.toml for {PLUGIN_REPOSITORY} "
            f"at ref {tag}: {error}"
        ) from error

    environment = manifest.get("environment")
    if not isinstance(environment, dict):
        raise ReleaseError(
            f"plugin.toml for {PLUGIN_REPOSITORY} at ref {tag} has no "
            "[environment] table."
        )

    version = environment.get("version")
    if not isinstance(version, str) or not version.strip():
        raise ReleaseError(
            f"plugin.toml for {PLUGIN_REPOSITORY} at ref {tag} has no valid "
            "[environment].version string."
        )
    return version


def release_tag_and_version(release: dict[str, object]) -> tuple[str, str]:
    tag = release.get("tag_name")
    if (
        not isinstance(tag, str)
        or re.fullmatch(r"v?[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", tag) is None
    ):
        raise ReleaseError(
            f"Latest release for {PLUGIN_REPOSITORY} has an invalid or missing "
            "tag_name."
        )
    return tag, tag[1:] if tag.startswith("v") else tag


def platform_releases(
    release: dict[str, object], tag: str
) -> list[tuple[Platform, str, str]]:
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise ReleaseError(
            f"Latest release for {PLUGIN_REPOSITORY} has malformed or missing assets."
        )

    assets_by_name: dict[str, dict[str, object]] = {}
    for index, asset in enumerate(assets):
        if not isinstance(asset, dict):
            raise ReleaseError(
                f"Latest release for {PLUGIN_REPOSITORY} has malformed asset "
                f"at index {index}."
            )
        name = asset.get("name")
        if not isinstance(name, str) or not name:
            raise ReleaseError(
                f"Latest release for {PLUGIN_REPOSITORY} has an asset with an "
                "invalid name."
            )
        if name in assets_by_name:
            raise ReleaseError(
                f"Latest release for {PLUGIN_REPOSITORY} contains duplicate "
                f"asset {name!r}."
            )
        assets_by_name[name] = asset

    discovered: list[tuple[Platform, str, str]] = []
    for platform in PLATFORMS:
        expected_name = platform.asset_template.format(tag=tag)
        asset = assets_by_name.get(expected_name)
        if asset is None:
            raise ReleaseError(
                f"Latest {PLUGIN_REPOSITORY} release is missing required asset "
                f"{expected_name!r}."
            )

        download_url = asset.get("browser_download_url")
        if not isinstance(download_url, str) or not download_url.strip():
            raise ReleaseError(
                f"Asset {expected_name!r} is missing browser_download_url."
            )

        digest = asset.get("digest")
        if not isinstance(digest, str) or not digest:
            raise ReleaseError(f"Asset {expected_name!r} is missing a digest.")
        if re.fullmatch(r"sha256:[0-9a-fA-F]{64}", digest) is None:
            raise ReleaseError(
                f"Asset {expected_name!r} does not provide a SHA-256 digest."
            )

        discovered.append((platform, digest.removeprefix("sha256:"), download_url))

    return discovered


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=True)


def render_release_toml(
    version: str, environment: str, platforms: list[tuple[Platform, str, str]]
) -> str:
    lines: list[str] = []
    for platform, sha256, download_url in platforms:
        lines.extend(
            (
                f"[{platform.section}]",
                f"version = {toml_string(version)}",
                f"sha256 = {toml_string(sha256)}",
                f"url = {toml_string(download_url)}",
                f"environment = {toml_string(environment)}",
                "",
            )
        )
    return "\n".join(lines) + "\n"


def write_atomically(path: Path, content: str) -> None:
    temporary_path: Path | None = None
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
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
        raise ReleaseError(f"Could not write {path}: {error}") from error


def main() -> int:
    gh_path = shutil.which("gh")
    if gh_path is None:
        print(
            "error: GitHub CLI (`gh`) is required to generate the annotations "
            "release manifest.",
            file=sys.stderr,
        )
        return 1

    try:
        release = fetch_latest_release(gh_path)
        tag, version = release_tag_and_version(release)
        platforms = platform_releases(release, tag)
        environment = fetch_release_environment(gh_path, tag)
        content = render_release_toml(version, environment, platforms)
        write_atomically(OUTPUT_PATH, content)
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"Generated {OUTPUT_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
