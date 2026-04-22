# Annotations Plugin for eov

[![Linux Build](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/build-linux-eop.yml/badge.svg)](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/build-linux-eop.yml)
[![macOS Build](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/build-macos-eop.yml/badge.svg)](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/build-macos-eop.yml)
[![Windows Build](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/build-windows-eop.yml/badge.svg)](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/build-windows-eop.yml)
[![Release Status](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/release.yml/badge.svg)](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0 OR GPL-3.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0%20OR%20GPL--3.0-0366d6)](https://github.com/eosin-platform/eov-annotations-plugin#license)

This repository contains the Annotations plugin for [eov](https://github.com/eosin-platform/eov), the lightweight whole-slide image viewer. The plugin adds local point and polygon annotation tools, a slide-scoped annotation sidebar, and JSON export for annotation data.

The host application is documented in the main eov repository. This README covers the plugin itself: what it does, how it behaves today, and how to build and package it for eov.

Prebuilt `.eop` release artifacts are published for Linux, macOS, and Windows across `x86_64` and `arm64` where GitHub-hosted runners are available.

## What It Does

The plugin extends eov with:

- Point annotations
- Polygon annotations
- Per-slide annotation sets
- A docked annotations sidebar
- Per-set color selection
- Per-set visibility toggles
- Set rename and delete actions
- Annotation delete actions
- JSON export for the active slide's annotations
- Persistent local storage backed by SQLite

Annotations are associated with a slide fingerprint rather than only a file path, so saved annotations follow the same slide content when the file is reopened.

## Current User-Facing Behavior

When the plugin is loaded into eov, it contributes three toolbar buttons:

- `Annotations`: opens the sidebar
- `Create Point Annotation`: activates the point tool, hotkey `1`
- `Create Polygon Annotation`: activates the polygon tool, hotkey `2`

It also adds two viewport context menu actions for an open slide:

- `Create Point`
- `Create Polygon`

### Point Annotations

Point annotations are rendered as colored rings in the viewport. When a point is placed, the plugin stores it immediately in the selected annotation set and refreshes both the sidebar and the viewport.

Placed points can be dragged to a new location. Moving a point updates both the in-memory plugin state and the persistent SQLite store.

### Polygon Annotations

Polygon annotations are rendered as colored filled polygons. The plugin persists a polygon once the host completes placement with at least three vertices.

Placed polygons can be moved, and polygon vertices can be edited through the host application's polygon editing support. The plugin persists updated vertices back to SQLite whenever the polygon changes.

### Annotation Sets

Annotations are organized into sets per slide.

- If you place an annotation on a slide with no selected set, the plugin creates or selects an `Untitled` set automatically.
- Creating a new set from the sidebar starts inline rename mode immediately.
- Sets can be renamed, recolored, collapsed, hidden, and deleted.
- Visibility is controlled per set. Hidden sets disappear from both the viewport overlays and the sidebar's rendered state.

Set colors are chosen from a built-in palette and are reused as evenly as possible across sets.

### Sidebar

The sidebar is a runtime-loaded Slint UI hosted by eov. It currently exposes a single annotation source:

- `Local`

The tree shows annotation sets and their child annotations for the active slide. Clicking a set selects it. Clicking an annotation focuses it in the viewport by framing the corresponding image-space region.

If no slide is open, the sidebar shows an empty-state message. If a slide is open but has no annotations yet, it shows a slide-specific empty state instead.

### Export

The export action writes the active slide's annotations to a JSON file chosen through the host save dialog. The default file name is based on the active slide name and ends with `_annotations.json`.

The exported payload includes:

- Slide file path
- Slide fingerprint as hex
- Annotation sets
- Set metadata including name, notes, color, and timestamps
- Point annotations with coordinates and timestamps
- Polygon annotations with vertices and timestamps

The export is scoped to the active slide only.

## Storage Model

The plugin stores annotations in a local SQLite database.

Default location:

- Linux: `$XDG_CONFIG_HOME/eov/annotations.db` when `XDG_CONFIG_HOME` is set
- Otherwise: `~/.config/eov/annotations.db`

You can override the database path with:

```bash
export EOV_ANNOTATIONS_DB=/path/to/annotations.db
```

The plugin computes a fingerprint for each slide and uses that fingerprint to load the matching annotation sets. This is the key reason annotations are slide-scoped rather than just path-scoped.

## Scope and Limitations

The current implementation is intentionally narrow.

Implemented today:

- Point annotations
- Polygon annotations
- Local storage
- Local export to JSON

Not implemented by this plugin today:

- Remote or shared annotation sources
- Annotation import
- Labels or ontology support
- Ellipse annotations
- Bitmask or brush annotations
- Viewport image filters
- HUD toolbar controls

The database schema already reserves tables for additional annotation types, but this plugin does not expose them yet.

## Building

This is a Rust plugin crate built as a dynamic library.

Build it from the plugin repository:

```bash
cargo build
```

Or from the host repository root:

```bash
cargo build --manifest-path plugins/annotations/Cargo.toml
```

The produced shared library must be packaged together with `plugin.toml` and the `ui/` directory before eov can discover it.

## Packaging for eov

Recent eov builds discover plugins from packaged `.eop` tarballs placed in the configured plugin directory. By default that directory is:

```text
~/.eov/plugins/
```

For this plugin, the package should contain at least:

- `plugin.toml`
- the compiled shared library for the target platform
- `ui/annotations-sidebar.slint`
- the rest of the `ui/` assets used by the sidebar

On Linux, a minimal manual packaging flow looks like this:

```bash
mkdir -p ~/.cache/eov/annotations-package
cp plugin.toml ~/.cache/eov/annotations-package/
cp -r ui ~/.cache/eov/annotations-package/
cp target/debug/libannotations.so ~/.cache/eov/annotations-package/

mkdir -p ~/.eov/plugins
tar -cf ~/.eov/plugins/annotations.eop -C ~/.cache/eov/annotations-package .
```

If you are working inside the main eov repository, there is already a helper script that builds, packages, and launches the app with this plugin:

```bash
./scripts/run-with-annotations.sh path/to/slide.svs
```

That script packages the plugin into `~/.eov/plugins/annotations.eop` and then starts the debug build of eov.

## Using the Plugin in eov

1. Start eov with the plugin available in its plugin directory.
2. Open a slide.
3. Click `Annotations` to open the sidebar.
4. Click the point tool or press `1` to place point annotations.
5. Click the polygon tool or press `2` to place polygon annotations.
6. Use the sidebar to create sets, rename them, change colors, hide or show sets, and export the active slide's annotations.

You can also start annotation placement from the viewport context menu.

## Repository Layout

```text
.
├── Cargo.toml
├── plugin.toml
├── src/
│   ├── db.rs
│   ├── lib.rs
│   ├── model.rs
│   ├── operations.rs
│   ├── sidebar.rs
│   └── state.rs
└── ui/
	├── annotations-sidebar.slint
	├── annotations-sidebar-types.slint
	├── annotations-sidebar-widgets.slint
	└── icons/
```

Key pieces:

- `src/lib.rs`: FFI entrypoints, toolbar actions, overlay providers, context menu integration
- `src/operations.rs`: persistence, selection logic, export, and tool activation flows
- `src/db.rs`: SQLite schema and load/store logic
- `src/sidebar.rs`: sidebar property generation and callback handling
- `ui/`: runtime-loaded Slint sidebar UI

## Development Notes

- The plugin is a Rust FFI plugin using the eov `plugin_api` crate.
- It does not open its own top-level window; its primary UI surface is the embedded sidebar.
- It currently returns no viewport filters and no HUD toolbar buttons.
- The sidebar width requested from the host is `300` pixels.

## License

This repository is tri-licensed under:

- MIT
- Apache-2.0
- GPL-3.0

See the license files in this repository for the full text.
