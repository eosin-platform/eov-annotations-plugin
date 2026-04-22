#!/usr/bin/env python3

from __future__ import annotations

import argparse
import pathlib
import tarfile


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Package the annotations plugin as a .eop tarball")
    parser.add_argument("--plugin-root", required=True, help="Path to the plugin repository root")
    parser.add_argument("--library", required=True, help="Compiled plugin shared library to include")
    parser.add_argument("--output", required=True, help="Output .eop file path")
    return parser.parse_args()


def add_directory(archive: tarfile.TarFile, root: pathlib.Path, relative_dir: pathlib.Path) -> None:
    directory = root / relative_dir
    for path in sorted(directory.rglob("*")):
        if path.is_file():
            archive.add(path, arcname=path.relative_to(root).as_posix())


def main() -> int:
    args = parse_args()
    plugin_root = pathlib.Path(args.plugin_root).resolve()
    library_path = pathlib.Path(args.library).resolve()
    output_path = pathlib.Path(args.output).resolve()

    manifest_path = plugin_root / "plugin.toml"
    ui_dir = plugin_root / "ui"

    if not manifest_path.is_file():
        raise SystemExit(f"missing plugin manifest: {manifest_path}")
    if not ui_dir.is_dir():
        raise SystemExit(f"missing ui directory: {ui_dir}")
    if not library_path.is_file():
        raise SystemExit(f"missing plugin library: {library_path}")

    output_path.parent.mkdir(parents=True, exist_ok=True)

    with tarfile.open(output_path, "w") as archive:
        archive.add(manifest_path, arcname="plugin.toml")
        archive.add(library_path, arcname=library_path.name)
        add_directory(archive, plugin_root, pathlib.Path("ui"))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())