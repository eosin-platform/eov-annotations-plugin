# Annotations plugin for eov

[![CI](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/ci.yml/badge.svg)](https://github.com/eosin-platform/eov-annotations-plugin/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0 OR GPL-3.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0%20OR%20GPL--3.0-0366d6)](#license)

Annotations for [eov](https://github.com/eosin-platform/eov), the whole-slide
image viewer: points, regions, lines and text, reusable classes, cell
counting with densities and indices, high-power fields, measurements in
physical units, and import and export in the formats other tools use
(GeoJSON as QuPath writes it, ASAP and Aperio ImageScope XML).

Version 1.0 of the plugin is written for eov 1.x (plugin API 1.0).

<p align="center">
  <img src="images/panel.webp" width="900" alt="The annotations panel: layers and the details of the selected region, the Count tab with densities and a positive index, and the Classes tab">
</p>

- [Installing](#installing)
- [Using it](#using-it)
- [Formats](#formats)
- [Coordinates and units](#coordinates-and-units)
- [Where annotations are stored](#where-annotations-are-stored)
- [Building, testing and packaging](#building-testing-and-packaging)
- [Limitations](#limitations)

## Installing

```bash
eov plugin install annotations     # the official release for this platform
eov plugin info annotations        # version, compatibility, provenance
```

Packages are also attached to the
[releases](https://github.com/eosin-platform/eov-annotations-plugin/releases).
A package you downloaded or built yourself installs with `--force`, because
it replaces the official source of the `annotations` id:

```bash
eov plugin install --force annotations-v1.0.0-linux-x86_64.eop
```

The package declares `api = ">=1.0, <2"` and `[environment] version =
">=1.0.0, <2.0.0"`; eov refuses it outside that range.

## Using it

Open a slide and click **Annotations** in the toolbar to show the panel. It
has four tabs:

- **Layers**: the annotations of the active slide, grouped in layers, and
  the details of the selection;
- **Classes**: the classes shared by all slides;
- **Count**: cell counts, densities and the high-power field size;
- **Data**: import and export, backups, the database, other slides and
  settings.

### Tools

The tools are in eov's toolbar; each has a key.

| Tool | Key | What it does |
|---|---|---|
| Select | `V` | Click to select, drag to select several; drag an annotation or its handles to move or reshape it. `Delete` deletes, `Esc` clears the selection. |
| Point | `.` | A point annotation. |
| Count | `C` | A count point in the *Cell counts* layer with the class counted by a click; **Shift+click** counts the second class. |
| Polygon | `P` | Click the vertices; click the first vertex, double-click or press Enter to close it. |
| Freehand | `G` | Drag an outline (stored as a polygon). |
| Rectangle | `B` | Drag a rectangle. |
| Ellipse | `O` | Drag an ellipse. |
| Line / path | `L` | Click the vertices; double-click or Enter finishes. Its length is measured. |
| Arrow | `A` | Like a line, with a head at the last point: click the tail, double-click the tip. |
| Text | `T` | A text label; its text is the annotation's name. |
| HPF field | `H` | A dashed field of the configured size follows the pointer; a click places one in the *HPF fields* layer, a drag pans. |

With one region selected, drawing a polygon, freehand outline, rectangle
or ellipse with **Shift** adds the shape to that region and with **Alt**
(or Ctrl/⌘) cuts it out of the region, which makes holes (an excluded lumen
no longer counts as area) or splits the region. Outlines that cross
themselves are split into simple parts when they are drawn, and every shape
is clipped to the slide.

Right-clicking the slide offers: show the annotation in the panel, copy its
measurements, lock or unlock it, delete it, add a point or place an HPF
field at the cursor, and open the panel. The eye button in a view's HUD
hides or shows the slide's annotations in that view only.

Every change (drawing, editing, deleting, an import, deleting a layer or a
class) is one step in eov's undo history, **Ctrl+Z** / **Ctrl+Shift+Z** (⌘ on
macOS). Undo applies to the slide the change was made on.

### Layers and annotations

New annotations go into the layer marked with a bar on the left (click a
layer to choose it); if a slide has none, an *Annotations* layer is created.
Count points and HPF fields have their own layers. A layer can be hidden,
locked (no editing, deleting or moving of its annotations), recoloured,
renamed, given notes and an opacity, or deleted (with undo) from its `⋯`
settings.

Select an annotation on the slide or in the list to see its details: name,
class, review status (*draft*, *verified*, *rejected*), layer, lock,
description, measurements, free-form properties (name–value pairs), its
author and when it was created and last edited. Changes apply to all
selected annotations where that makes sense. Text fields save as you type
(after a pause, on Enter or when you leave the field), to the annotation
they were typed for. Double-click a row to zoom to it; Ctrl+click selects
several. The filter matches names, classes, kinds and statuses; large
layers start collapsed and long lists are shown a few hundred rows at a
time.

### Classes

Classes (Tumour, Stroma, Necrosis, Immune cells, Mitosis, Positive,
Negative and Other to begin with) are shared by all slides. Each has a
colour, an optional one-key hotkey, visibility and opacity, and shows how
many annotations of the active slide use it. Click a class to make it the
*active class*: new annotations get it and the Count tool counts it.
Pressing a class's hotkey makes it active; a hotkey is one character that
eov and the annotation tools do not use. If another plugin already uses
the key, eov says so and the class shows why its key does not work.
The second count class (Shift+click with the Count tool) is chosen in the
class's `⋯` settings or in the Count tab. Deleting a class leaves its
annotations unclassified (with undo).

### Counting

Pick the classes counted by a click and by Shift+click in the Count tab,
choose the Count tool and click cells. The tally counts the point
annotations of the visible layers (optionally of hidden layers too):

- with no region selected, on the whole slide;
- with regions or HPF fields selected, inside their union, with the area,
  the density per mm² with its exact 95 % Poisson (Garwood) interval, and
  the count per 10 HPF (or per field, when only fields are selected);
- with two count classes, the index *first / (first + second)* with its
  exact 95 % (Clopper–Pearson) interval, e.g. the Ki-67 proliferation index
  with *Positive* and *Negative*.

Counts follow every edit at once, and areas use the same even-odd rule as
the point-in-region test and the display. **Clear counts…** removes the
count points of the slide (with undo).

### High-power fields

The field size is given as a field number and objective (the default is
FN 22 at 40×, 0.2376 mm²) or directly as an area in mm². With the HPF
field tool active, a field outline follows the pointer, labelled with what
it would count; click to place a field, drag to pan, use the wheel to zoom.
Placed fields are ordinary regions in the *HPF fields* layer: select them
(**Select all fields**) to count inside them. Without a pixel size, fields
cannot be sized and are refused. The tool uses no keys of its own: eov's
shortcuts, the other tools' keys and class hotkeys work as usual while it
is active, and **Esc** (or `H` again) leaves it.

### Measurements

Every annotation lists, as the slide's pixel size allows:

- area (minus holes) and perimeter, or length for lines;
- maximum diameter (Feret) and minimum caliper (minimum Feret width);
- the centre in level-0 pixels.

Values are computed in micrometre space, so slides with different
horizontal and vertical pixel sizes are measured exactly. With only one
pixel size known it is used for both axes and the values are marked
approximate (≈). With none, values are given in pixels and the panel says
so. **Copy measurements** copies them as tab-separated text; the CSV export
has them for every annotation.

## Formats

| Format | Import | Export |
|---|---|---|
| GeoJSON (QuPath and others) | ✓ | ✓ |
| eov JSON (format 2; format 1 of the plugin before 1.0) | ✓ | ✓ |
| eov archive (every slide in the database) | ✓ (one slide) | ✓ |
| ASAP XML | ✓ | ✓ |
| Aperio ImageScope XML | ✓ | ✓ |
| CSV measurements | | ✓ |

**Importing** (Data → Import…, or `⋯` → Import annotations…) reads the file
in the background and shows a preview before anything is written: what was
found, whether the file was made for this slide (by eov slide identity,
OpenSlide quickhash, size or SHA-256, as far as the file says), what had to
be skipped or repaired and why, and what to do with layers whose names
already exist (add to them, replace them, or keep both). The import is
then written in one transaction and is one undo step. It stays bound to the
slide it was started on and is cancelled if that slide is closed.
Unusable elements (bad coordinates, too few vertices, shapes outside the
slide) are skipped with a reason; crossing outlines are repaired; overlong
names and texts are cut to the limits.

<p align="center">
  <img src="images/import-dialog.webp" width="520" alt="The import preview: the file, what it contains, how it matches the slide, what was skipped, and what to do with layers whose names exist">
</p>

- **GeoJSON**: Points, LineStrings and Polygons (with holes), their Multi
  forms and GeometryCollections, as Features or a FeatureCollection. QuPath's
  `classification` (`name` and `color`, or `names` and `colorRGB` of older
  versions) becomes the class, `name`, `isLocked` and `metadata` are kept;
  detections go to a separate layer. Exports are FeatureCollections that
  QuPath reads; rectangles and ellipses are written as polygons, text as
  points, and `properties.eov` keeps the exact shape, layer, status and
  provenance so a round trip through eov loses nothing.
- **eov JSON**: self-describing (`format`, `format_version`, the coordinate
  convention, the slide's identity, quickhash, size, pixel size and
  bounds), with classes, layers and every annotation field.
- **ASAP XML**: dots, point sets, rectangles, polygons, splines (as
  their control polygon) and measurements (as lines); groups become
  classes. Exports write classes as groups and ASAP's nearest shape for what
  it lacks (ellipses as polygons, lines and arrows as measurements, text as
  dots, holes as polygons of their own), with `Eov…` attributes ASAP
  ignores and eov reads back, so layers, holes, ellipses, arrows and text
  survive a round trip.
- **ImageScope XML**: each annotation becomes a layer, `LineColor` its
  colour; polygon and freehand, rectangle, ellipse, point, ruler (a line)
  and arrow (from its first vertex to its last) regions; negative regions
  become holes of the region that contains them. Exports write one
  annotation per layer and one region per annotation (holes as negative
  regions), with the class and description as region attributes.
- **CSV**: one row per annotation with its layer, class, kind, status,
  author, timestamps, centre and measurements in µm and pixels, and whether
  the slide was calibrated. Cells that a spreadsheet would run as formulas
  are escaped.

Exports never contain the slide's path. The file name is included only
when *Include the slide's file name in exports* is set; the author,
organization, project and license fields are written into eov JSON
exports. Files are written to a temporary file first and renamed into
place, so an interrupted export never leaves a half-written file.

## Coordinates and units

All coordinates, in the database and in every format, are **level-0 pixels**
of the slide: the origin is the top-left corner of the level-0 image, x
grows to the right and **y grows downwards** (the convention of OpenSlide,
QuPath, ASAP and ImageScope, and of eov's plugin API). The OpenSlide bounds offset (`openslide.bounds-x/y`) is **not**
subtracted: coordinates refer to the full level-0 image, and eov JSON
exports record the bounds so that tools working in the bounds' frame can
convert. Physical values use the slide's pixel size (µm per level-0 pixel,
`mpp_x` and `mpp_y`), also recorded in eov exports.

## Where annotations are stored

Annotations live in one SQLite database, `annotations.db` in the plugin's
data directory, `<eov data>/plugins/annotations/`, where `<eov data>` is
`$EOV_DATA_DIR` or the platform's local data directory plus `eov`:

| Platform | Default location |
|---|---|
| Linux | `~/.local/share/eov/plugins/annotations/annotations.db` |
| macOS | `~/Library/Application Support/eov/plugins/annotations/annotations.db` |
| Windows | `%LOCALAPPDATA%\eov\plugins\annotations\annotations.db` |

To use another file (for example a shared one), set `"database_path"` in the
plugin's settings file next to it (`settings.json`, while eov is closed), or
the `EOV_ANNOTATIONS_DB` environment variable. The Data tab shows the file
in use.

- **Several eov windows** can use the same database: it runs in WAL mode,
  every change is one transaction checked against what it changes, and a
  window picks up the other's changes when it is focused.
- **Read-only databases** (a teaching set on a read-only share) open
  read-only: annotations can be viewed and exported, not edited. An older
  read-only database is upgraded in a private copy.
- **Slides** are recognised by eov's slide identity, not by their path, so
  annotations follow a slide that is moved or renamed. A slide that was
  re-saved or de-identified gets a new identity; **Other slides** in the
  Data tab lists the slides with annotations that are not open, and
  **Attach** moves their annotations to the open slide (with undo).
- **Deleted** annotations, layers and classes are kept for undo and removed
  for good after seven days. Every change is also recorded, with its author
  and time, in an `audit_log` table.
- **Backups**: **Back up…** writes a consistent copy of the database (also
  while eov is using it), **Export all slides…** writes every slide to one
  eov archive (JSON), and **Merge…** brings the annotations of another
  database (a backup, or a colleague's) into this one without overwriting
  anything.
- **The schema is versioned** (`PRAGMA user_version`, now 2) and upgraded in
  one transaction when an older database is opened.

### Coming from the plugin before 1.0

Before eov 1.0 the plugin kept its database in eov's settings folder
(`~/.config/eov/annotations.db` on Linux, `~/Library/Application
Support/eov/annotations.db` on macOS, `%APPDATA%\eov\annotations.db` on
Windows). eov tells the plugin where that folder is; when it holds an
`annotations.db`, the plugin offers once, at start, to bring its
annotations into the current database (the old file is only read), and the
Data tab keeps offering it until it is done. **Merge…** in the Data tab
does the same for any other database file. You can also point
`database_path` at the old file to keep using it (it is upgraded in place,
after which older versions of the plugin cannot read it, so back it up
first).
Slides of the old database are recognised by their old sampled fingerprint
the first time they are opened (computed in the background) and linked to
their eov identity from then on. Exports of the old plugin (format 1) can
be imported as well.

## Building, testing and packaging

The plugin builds on its own (it is not a member of eov's workspace) and
needs only `eov-plugin-api`, found at `../../plugin_api` (where it is inside
an eov checkout, under `plugins/annotations`).

```bash
cargo build --release                 # target/release/libannotations.so (.dylib, annotations.dll)
cargo test                            # unit and end-to-end tests (SDK mock host, temporary directories)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests drive the plugin through the same FFI adapter eov uses, with the
SDK's mock host; they only touch temporary directories.

Two development tools check what the unit tests cannot:

```bash
cargo run --release --manifest-path tools/ui-check/Cargo.toml                        # check the .slint surfaces
cargo run --release --manifest-path tools/ui-check/Cargo.toml --bin ui-render -- out  # draw them into PNGs
cargo run --release --manifest-path tools/load-check/Cargo.toml                      # load the built library like eov
```

`tools/ui-check` uses the Slint version eov 1.0 compiles plugin surfaces
with (1.17.1). It compiles `ui/*.slint` as the host does and verifies that
the components declare exactly the callbacks the plugin handles (with the
argument types it reads), that every property the plugin pushes in a range
of realistic states is a public input of the right type (struct fields
included) and that every input is pushed, and it presses a few controls
through the accessibility tree; `ui-render` draws those states with Slint's
software renderer (the screenshots above). `tools/load-check` opens the
release library with abi_stable's layout checks and the API version
handshake, as eov does, and draws an annotation through it.

To package (an `.eop` is a tar archive with `plugin.toml`, the library and
`ui/` at its root; entries are sorted and dated `SOURCE_DATE_EPOCH`, so
builds are reproducible):

```bash
python3 scripts/package_eop.py --output dist/annotations-v1.0.0-linux-x86_64.eop
eov plugin install --force dist/annotations-v1.0.0-linux-x86_64.eop
```

`scripts/package_eop.py --check-only` validates the manifest and `ui/`
without a library. In an eov checkout, `scripts/run-with-plugins.sh` of eov
starts a development build with the plugins in a separate eov home.

CI (`.github/workflows/ci.yml`) runs formatting, clippy, the tests, the
surface check, the load check and the packaging script against eov's
`plugin_api` on Linux, macOS and Windows. Releases
(`release.yml`, dispatched with `scripts/dispatch-release-workflow.sh
<version>`) build the six platform packages, write `release.toml` with
their SHA-256 hashes and immutable URLs, and publish them.

## Limitations

This depends on what eov's plugin API offers today:

- A region made of several separate parts is stored as several
  annotations (eov's shapes have no multipolygon).

Not done yet: a brush with eraser, NDPA and DICOM annotation
formats, comment threads and blinded multi-reader copies.

## Repository layout

```text
plugin.toml         the manifest
src/
  lib.rs            the plugin: events, tools, menus, surfaces
  core.rs           state, the database and I/O threads, operations
  actions.rs        drawing, editing, layers, classes, HPF fields
  overlay.rs        what is drawn on the slide (eov overlay layers)
  sidebar.rs        the panel's properties and callbacks
  transfer.rs       import preview and apply, exports, backups
  counting.rs stats.rs measure.rs geometry.rs
  formats/          GeoJSON, eov JSON, ASAP/ImageScope XML, CSV
  db/               schema, migrations (also from before 1.0), queries
  tests.rs          end-to-end tests
ui/                 the panel and the import dialog (Slint)
tools/ui-check/     surface check and renderer (Slint 1.17.1)
tools/load-check/   loads the built library like eov
scripts/            packaging and release tooling
```

## License

This repository is tri-licensed under MIT, Apache-2.0 or GPL-3.0, at your
option; see `LICENSE-MIT`, `LICENSE-APACHE-2.0` and `LICENSE-GPLv3`.
