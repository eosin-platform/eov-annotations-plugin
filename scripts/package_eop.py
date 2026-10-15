#!/usr/bin/env python3
"""Packages the annotations plugin as an eov plugin package (.eop).

An .eop is a plain tar archive with, at its root:

- plugin.toml        the manifest (validated here);
- the library        lib<id>.so, lib<id>.dylib or <id>.dll;
- ui/                the .slint surfaces and their icons;
- assets/            other files the plugin reads at run time (if any).

Entries are sorted, owned by root and dated SOURCE_DATE_EPOCH (or 0), so the
same inputs give the same archive. Only regular files and directories are
written (the host refuses links).

    python3 scripts/package_eop.py --library target/release/libannotations.so \\
        --output dist/annotations-v1.0.0-linux-x86_64.eop
"""

from __future__ import annotations

import argparse
import hashlib
import io
import os
import pathlib
import re
import sys
import tarfile
import tomllib

ID_RE = re.compile(r"^[a-z0-9][a-z0-9_]{0,63}$")
SEMVER_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
# Keys of manifests before plugin API 1.0; the 1.0 host ignores them, so a
# manifest that still has them was not ported.
LEGACY_KEYS = ("entry_ui", "entry_component", "toolbar_buttons")
CONTENT_DIRS = ("ui", "assets")


class PackageError(Exception):
    pass


def library_names(plugin_id: str) -> set[str]:
    return {f"lib{plugin_id}.so", f"lib{plugin_id}.dylib", f"{plugin_id}.dll"}


def validate_manifest(root: pathlib.Path) -> dict[str, object]:
    path = root / "plugin.toml"
    try:
        manifest = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise PackageError(f"cannot read {path}: {error}") from error
    plugin_id = manifest.get("id")
    if not isinstance(plugin_id, str) or ID_RE.fullmatch(plugin_id) is None:
        raise PackageError(f"plugin.toml: invalid id {plugin_id!r}")
    for key in ("name", "version"):
        if not isinstance(manifest.get(key), str) or not manifest[key]:
            raise PackageError(f"plugin.toml: missing {key}")
    if SEMVER_RE.fullmatch(manifest["version"]) is None:
        raise PackageError(f"plugin.toml: version {manifest['version']!r} is not SemVer")
    if not isinstance(manifest.get("api"), str):
        raise PackageError('plugin.toml: missing api (e.g. api = ">=1.0, <2")')
    environment = manifest.get("environment")
    if not isinstance(environment, dict) or not isinstance(environment.get("version"), str):
        raise PackageError("plugin.toml: missing [environment] version")
    legacy = [key for key in LEGACY_KEYS if key in manifest]
    if legacy:
        raise PackageError(f"plugin.toml: keys from before plugin API 1.0: {', '.join(legacy)}")
    cargo = root / "Cargo.toml"
    if cargo.is_file():
        package = tomllib.loads(cargo.read_text(encoding="utf-8")).get("package", {})
        if isinstance(package, dict) and package.get("version") not in (None, manifest["version"]):
            raise PackageError(
                f"Cargo.toml version {package.get('version')!r} differs from "
                f"plugin.toml version {manifest['version']!r}"
            )
    return manifest


def default_library(root: pathlib.Path, plugin_id: str) -> pathlib.Path:
    release = root / "target" / "release"
    found = [release / name for name in sorted(library_names(plugin_id)) if (release / name).is_file()]
    if len(found) != 1:
        raise PackageError(
            f"pass --library: found {len(found)} plugin libraries in {release}"
        )
    return found[0]


def content_files(root: pathlib.Path) -> list[tuple[pathlib.Path, str]]:
    files: list[tuple[pathlib.Path, str]] = []
    for directory in CONTENT_DIRS:
        base = root / directory
        if not base.exists():
            continue
        for path in sorted(base.rglob("*")):
            if path.is_symlink():
                raise PackageError(f"links are not allowed in packages: {path}")
            if path.is_file():
                files.append((path, path.relative_to(root).as_posix()))
    if not any(name.startswith("ui/") and name.endswith(".slint") for _, name in files):
        raise PackageError(f"no .slint files under {root / 'ui'}")
    return files


def tar_info(name: str, size: int, mtime: int, mode: int, directory: bool = False) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.size = 0 if directory else size
    info.mtime = mtime
    info.mode = mode
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    info.type = tarfile.DIRTYPE if directory else tarfile.REGTYPE
    return info


def build_package(
    root: pathlib.Path, library: pathlib.Path, output: pathlib.Path
) -> tuple[dict[str, object], list[str]]:
    manifest = validate_manifest(root)
    plugin_id = manifest["id"]
    if library.name not in library_names(plugin_id):
        raise PackageError(
            f"the library must be named {' or '.join(sorted(library_names(plugin_id)))}, "
            f"not {library.name}"
        )
    if not library.is_file():
        raise PackageError(f"missing plugin library: {library}")
    entries: list[tuple[str, bytes, int]] = [
        ("plugin.toml", (root / "plugin.toml").read_bytes(), 0o644),
        (library.name, library.read_bytes(), 0o755),
    ]
    entries += [(name, path.read_bytes(), 0o644) for path, name in content_files(root)]
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
    directories = sorted(
        {"/".join(name.split("/")[:i]) for name, _, _ in entries for i in range(1, name.count("/") + 1)}
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    names: list[str] = []
    with tarfile.open(temporary, "w", format=tarfile.PAX_FORMAT) as archive:
        for directory in directories:
            archive.addfile(tar_info(directory, 0, mtime, 0o755, directory=True))
            names.append(directory + "/")
        for name, data, mode in sorted(entries):
            archive.addfile(tar_info(name, len(data), mtime, mode), io.BytesIO(data))
            names.append(name)
    os.replace(temporary, output)
    return manifest, names


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    script_root = pathlib.Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description="Package the annotations plugin as an .eop")
    parser.add_argument("--plugin-root", type=pathlib.Path, default=script_root,
                        help="plugin repository root (default: this script's repository)")
    parser.add_argument("--library", type=pathlib.Path,
                        help="the compiled plugin library (default: target/release/lib<id>.*)")
    parser.add_argument("--output", type=pathlib.Path,
                        help="output .eop path (default: dist/<id>-v<version>.eop)")
    parser.add_argument("--check-only", action="store_true",
                        help="validate the manifest and ui/ without writing a package")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    root = args.plugin_root.resolve()
    try:
        manifest = validate_manifest(root)
        content_files(root)
        if args.check_only:
            print(f"{manifest['id']} {manifest['version']}: manifest and ui/ are valid")
            return 0
        library = (args.library or default_library(root, manifest["id"])).resolve()
        output = args.output or root / "dist" / f"{manifest['id']}-v{manifest['version']}.eop"
        _, names = build_package(root, library, output.resolve())
    except PackageError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    print(f"{output} ({len(names)} entries)")
    print(f"sha256 {digest}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
