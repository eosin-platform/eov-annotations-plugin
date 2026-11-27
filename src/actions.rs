//! What user actions do. Handlers run on the UI thread, read the state and
//! queue [`Op`]s whose plans run on the database thread against the
//! up-to-date state.

use crate::change::Change;
use crate::core::{Core, DbJob, Label, Op, Planned, label};
use crate::geometry::{self, Normalized};
use crate::measure::Calibration;
use crate::model::{
    ARROWHEAD_VERTICES, Annotation, Bounds, Class, Color, Comment, Field, Geometry, GeometryKind,
    Layer, LayerRole, MAX_COMMENTS, SlideId, Status, clean_name, clean_text, new_id, now_ms,
    pick_color,
};
use crate::overlay::{self, HostOp, parse_host_layer_id, reticle_layer_id};
use crate::state::{BRUSH_SIZES, BrushStroke, FieldPress, SlideData, State};
use crate::tools;
use eov_plugin_api::i18n;
use eov_plugin_api::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

fn loading() -> String {
    tr!("The annotations of this slide are still loading; try again in a moment.")
}

fn layer_gone() -> String {
    tr!("That layer no longer exists.")
}

fn annotation_gone() -> String {
    tr!("That annotation no longer exists.")
}

fn class_gone() -> String {
    tr!("That class no longer exists.")
}

fn layer_locked(name: &str) -> String {
    tr!("Layer \"{}\" is locked.", i18n::isolate(name))
}

fn outside_cut_off() -> String {
    tr!("Parts outside the slide were cut off.")
}

/// The level-0 canvas of a file.
fn image_bounds(info: &FileInfo) -> Option<Bounds> {
    (info.width > 0 && info.height > 0).then_some(Bounds {
        min_x: 0.0,
        min_y: 0.0,
        max_x: info.width as f64,
        max_y: info.height as f64,
    })
}

/// The slide and canvas of a file, ready for editing.
fn editable_slide(state: &State, file_id: FileId) -> Result<(&SlideData, Option<Bounds>), String> {
    state.writable()?;
    let file = state
        .files
        .get(&file_id)
        .ok_or_else(|| tr!("The slide is not open any more."))?;
    let slide = state.slide_of(file_id).ok_or_else(loading)?;
    Ok((slide, image_bounds(&file.info)))
}

/// The layer new annotations of `role` go to, creating it when missing.
fn target_layer(
    slide: &SlideData,
    role: LayerRole,
    changes: &mut Vec<Change>,
) -> Result<Layer, String> {
    let existing = match role {
        LayerRole::Normal => slide.target_layer(),
        other => slide.layer_by_role(other),
    };
    if let Some(layer) = existing {
        if layer.locked {
            return Err(layer_locked(&layer.name));
        }
        return Ok(layer.clone());
    }
    let now = now_ms();
    let layer = Layer {
        id: new_id(),
        slide_id: slide.id(),
        name: role.default_name().to_string(),
        notes: String::new(),
        color: pick_color(slide.layers.iter().map(|l| &l.color)),
        visible: true,
        opacity: 1.0,
        locked: false,
        role,
        sort_order: slide.layers.iter().map(|l| l.sort_order).max().unwrap_or(0) + 1,
        created_at: now,
        updated_at: now,
    };
    changes.push(Change::Layer {
        before: None,
        after: Some(layer.clone()),
    });
    Ok(layer)
}

/// A new annotation.
fn annotation(
    layer: &Layer,
    geometry: Geometry,
    class: Option<String>,
    name: &str,
    author: &str,
) -> Annotation {
    let now = now_ms();
    Annotation {
        id: new_id(),
        layer_id: layer.id.clone(),
        class_id: class,
        geometry,
        name: name.to_string(),
        description: String::new(),
        status: Status::Draft,
        author: author.to_string(),
        locked: false,
        properties: Vec::new(),
        comments: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

fn notes_for(normalized: &Normalized) -> Option<String> {
    let mut notes = Vec::new();
    if normalized.repaired {
        notes.push(if normalized.parts.len() > 1 {
            // Translators: {n} is 2 or more.
            tr!("The outline crossed itself and was split into {n} parts."
                | "The outline crossed itself and was split into {n} parts."
                    % normalized.parts.len())
        } else {
            tr!("The outline crossed itself and was repaired.")
        });
    }
    if normalized.clipped {
        notes.push(outside_cut_off());
    }
    (!notes.is_empty()).then(|| i18n::join_sentences(&notes))
}

/// The geometry of an HPF field centred at `(x, y)`.
pub fn field_geometry(state: &State, info: &FileInfo, x: f64, y: f64) -> Result<Geometry, String> {
    let calibration = Calibration::new(info.mpp_x, info.mpp_y);
    let area = state
        .settings
        .field
        .area_mm2()
        .ok_or_else(|| tr!("The HPF size is not valid; check it in the Count tab."))?;
    let radius_um = (area / std::f64::consts::PI).sqrt() * 1000.0;
    let (rx, ry) = calibration
        .um_to_px(radius_um)
        .ok_or_else(|| tr!("This slide has no pixel size, so HPF fields cannot be sized."))?;
    Ok(Geometry::Ellipse {
        cx: x,
        cy: y,
        rx,
        ry,
    })
}

/// A shape drawn with one of the plugin's tools.
pub fn shape_drawn(
    core: &Arc<Core>,
    tool: &str,
    file_id: FileId,
    shape: Shape,
    modifiers: Modifiers,
) {
    let (geometry, _) = Geometry::from_shape(&shape);
    let (info, selection, settings) = {
        let state = core.state();
        let Some(file) = state.files.get(&file_id) else {
            return;
        };
        (
            file.info.clone(),
            file.selection.clone(),
            state.settings.clone(),
        )
    };
    let active = settings.active_class.clone();
    match tool {
        tools::COUNT => {
            let class = if modifiers.alt && settings.immune_class.is_some() {
                settings.immune_class.clone()
            } else if modifiers.shift {
                settings.alternate_class.clone().or(active)
            } else {
                active
            };
            add(
                core,
                file_id,
                vec![geometry],
                LayerRole::Counts,
                class,
                "",
                false,
                label!(tr!("Count cell")),
            );
        }
        tools::POINT => add(
            core,
            file_id,
            vec![geometry],
            LayerRole::Normal,
            active,
            "",
            true,
            label!(tr!("Add point")),
        ),
        tools::POLYLINE => add(
            core,
            file_id,
            vec![geometry],
            LayerRole::Normal,
            active,
            "",
            true,
            label!(tr!("Add line")),
        ),
        tools::ARROW => {
            let Geometry::Polyline { points } = geometry else {
                return;
            };
            add(
                core,
                file_id,
                vec![Geometry::Arrow { points }],
                LayerRole::Normal,
                active,
                "",
                true,
                label!(tr!("Add arrow")),
            );
        }
        tools::TEXT => {
            let (x, y) = match geometry {
                Geometry::Point { x, y } => (x, y),
                _ => return,
            };
            add(
                core,
                file_id,
                vec![Geometry::Text { x, y }],
                LayerRole::Normal,
                active,
                "Text",
                true,
                label!(tr!("Add text")),
            );
        }
        tools::FIELD => {
            let Geometry::Point { x, y } = geometry else {
                return;
            };
            let field = match field_geometry(&core.state(), &info, x, y) {
                Ok(field) => field,
                Err(message) => {
                    core.notify(NotifyLevel::Warning, message);
                    return;
                }
            };
            add_field(core, file_id, field);
        }
        tools::POLYGON | tools::FREEHAND | tools::RECTANGLE | tools::ELLIPSE => {
            let combine = if modifiers.shift {
                Some(true)
            } else if modifiers.alt || modifiers.primary {
                Some(false)
            } else {
                None
            };
            let target = {
                let state = core.state();
                state.slide_of(file_id).and_then(|slide| {
                    let areas: Vec<&String> = selection
                        .iter()
                        .filter(|id| {
                            slide
                                .annotations
                                .get(*id)
                                .is_some_and(|a| a.geometry.kind().is_area())
                        })
                        .collect();
                    (areas.len() == 1).then(|| areas[0].clone())
                })
            };
            match (combine, target) {
                (Some(union), Some(target)) => combine_with(core, file_id, target, geometry, union),
                (Some(_), None) => {
                    core.notify(
                        NotifyLevel::Info,
                        tr!(
                            "To add the shape to a region (Shift) or cut it from a region (Alt), select exactly one region first. The shape was added as a new annotation."
                        ),
                    );
                    add(
                        core,
                        file_id,
                        vec![geometry],
                        LayerRole::Normal,
                        active,
                        "",
                        true,
                        label!(tr!("Add region")),
                    );
                }
                (None, _) => add(
                    core,
                    file_id,
                    vec![geometry],
                    LayerRole::Normal,
                    active,
                    "",
                    true,
                    label!(tr!("Add region")),
                ),
            }
        }
        _ => {}
    }
}

/// Adds annotations (normalised on the database thread).
#[allow(clippy::too_many_arguments)]
pub fn add(
    core: &Arc<Core>,
    file_id: FileId,
    geometries: Vec<Geometry>,
    role: LayerRole,
    class: Option<String>,
    name: &str,
    select: bool,
    label: Label,
) {
    let name = name.to_string();
    core.submit(Op::new(label, Some(file_id), move |state| {
        let (slide, image) = editable_slide(state, file_id)?;
        let class = class.filter(|id| state.class(id).is_some());
        let author = state.settings.author.clone();
        let mut changes = Vec::new();
        let layer = target_layer(slide, role, &mut changes)?;
        let mut notice = None;
        let mut ids = Vec::new();
        for geometry in geometries {
            let normalized = geometry::normalize(geometry, image)?;
            notice = notes_for(&normalized).or(notice);
            for part in normalized.parts {
                let a = annotation(&layer, part, class.clone(), &name, &author);
                ids.push(a.id.clone());
                changes.push(Change::Annotation {
                    before: None,
                    after: Some(Arc::new(a)),
                });
            }
        }
        Ok(Planned {
            changes,
            select: select.then_some((file_id, ids)),
            current_layer: (role == LayerRole::Normal).then(|| (slide.id(), layer.id.clone())),
            notice,
            ..Default::default()
        })
    }));
}

fn add_field(core: &Arc<Core>, file_id: FileId, field: Geometry) {
    core.submit(Op::new(
        label!(tr!("Place HPF field")),
        Some(file_id),
        move |state| {
            let (slide, image) = editable_slide(state, file_id)?;
            let mut changes = Vec::new();
            let layer = target_layer(slide, LayerRole::Fields, &mut changes)?;
            let number = slide.count_in(&layer.id) + 1;
            let normalized = geometry::normalize(field, image)?;
            let notice = normalized
                .parts
                .first()
                .is_some_and(|g| {
                    image.is_some_and(|b| {
                        let f = g.bounds();
                        f.min_x < b.min_x
                            || f.min_y < b.min_y
                            || f.max_x > b.max_x
                            || f.max_y > b.max_y
                    })
                })
                .then(|| {
                    tr!("This field extends past the slide edge; its area includes empty canvas.")
                });
            let author = state.settings.author.clone();
            let mut ids = Vec::new();
            for part in normalized.parts {
                let a = annotation(&layer, part, None, &format!("HPF {number}"), &author);
                ids.push(a.id.clone());
                changes.push(Change::Annotation {
                    before: None,
                    after: Some(Arc::new(a)),
                });
            }
            Ok(Planned {
                changes,
                notice,
                ..Default::default()
            })
        },
    ));
}

/// Shift-draw (union) or Alt-draw (difference) with a selected region.
fn combine_with(
    core: &Arc<Core>,
    file_id: FileId,
    target_id: String,
    shape: Geometry,
    union: bool,
) {
    let label = if union {
        label!(tr!("Add to region"))
    } else {
        label!(tr!("Cut from region"))
    };
    core.submit(Op::new(label, Some(file_id), move |state| {
        let (slide, image) = editable_slide(state, file_id)?;
        let target = slide
            .annotations
            .get(&target_id)
            .cloned()
            .ok_or_else(|| tr!("The selected region no longer exists."))?;
        if target.locked || slide.layer(&target.layer_id).is_some_and(|l| l.locked) {
            return Err(tr!("The selected region is locked."));
        }
        let shape = geometry::normalize(shape, image)?;
        let mut result = target.geometry.clone();
        let mut extra: Vec<Geometry> = Vec::new();
        for part in &shape.parts {
            let parts = if union {
                geometry::union(&result, part)
            } else {
                geometry::difference(&result, part)
            }
            .ok_or_else(|| tr!("Only regions can be combined."))?;
            match parts.len() {
                0 => {
                    return Ok(Planned {
                        changes: vec![Change::Annotation {
                            before: Some(target.clone()),
                            after: None,
                        }],
                        notice: Some(tr!("The cut removed the whole region.")),
                        ..Default::default()
                    });
                }
                1 => result = parts.into_iter().next().expect("one part"),
                _ if union => extra.push(part.clone()),
                _ => {
                    let mut parts = parts.into_iter();
                    result = parts.next().expect("parts");
                    extra.extend(parts);
                }
            }
        }
        let now = now_ms();
        let mut updated = (*target).clone();
        updated.geometry = result;
        updated.updated_at = now.max(target.updated_at + 1);
        let mut changes = vec![Change::Annotation {
            before: Some(target.clone()),
            after: Some(Arc::new(updated)),
        }];
        let notice = if !extra.is_empty() && union {
            Some(tr!(
                "The shapes do not touch; the new shape was added as a separate annotation."
            ))
        } else if !extra.is_empty() {
            let parts = extra.len() + 1;
            // Translators: {n} is 2 or more.
            Some(tr!("The cut split the region into {n} parts."
                | "The cut split the region into {n} parts." % parts))
        } else {
            None
        };
        let mut ids = vec![target.id.clone()];
        for geometry in extra {
            let mut part = (*target).clone();
            part.id = new_id();
            part.geometry = geometry;
            part.created_at = now;
            part.updated_at = now;
            ids.push(part.id.clone());
            changes.push(Change::Annotation {
                before: None,
                after: Some(Arc::new(part)),
            });
        }
        Ok(Planned {
            changes,
            select: Some((file_id, ids)),
            notice,
            ..Default::default()
        })
    }));
}

/// The host reshaped an object (vertex drag, move, vertex insert/delete).
pub fn object_edited(core: &Arc<Core>, object: ObjectRef, shape: Shape) {
    let Some((file_id, _)) = parse_host_layer_id(&object.layer_id) else {
        return;
    };
    let id = object.object_id;
    core.submit(Op::new(
        label!(tr!("Edit shape")),
        Some(file_id),
        move |state| {
            let refuse = |slide: SlideId, message: String| {
                Ok(Planned {
                    revert: Some((slide, vec![id.clone()])),
                    warning: Some(message),
                    ..Default::default()
                })
            };
            let Some(slide) = state.slide_of(file_id) else {
                return Err(String::new());
            };
            let slide_id = slide.id();
            if let Err(e) = state.writable() {
                return refuse(slide_id, e);
            }
            let Some(current) = slide.annotations.get(&id).cloned() else {
                return refuse(slide_id, annotation_gone());
            };
            let layer = slide.layer(&current.layer_id);
            if current.locked || layer.is_some_and(|l| l.locked) {
                return refuse(slide_id, tr!("The annotation is locked."));
            }
            let (mut geometry, _) = Geometry::from_shape(&shape);
            // Arrows are drawn with their head (`arrow_outline`): keep the shaft
            // the host moved or reshaped, and draw a new head for it.
            let arrow_shaft = |points: &[[f64; 2]], shaft: usize| -> Option<Vec<[f64; 2]>> {
                if points.len() == shaft + ARROWHEAD_VERTICES {
                    Some(points[..shaft].to_vec())
                } else {
                    None
                }
            };
            if let Geometry::Arrow { points: shaft } = &current.geometry {
                let Geometry::Polyline { points } = &geometry else {
                    return refuse(
                        slide_id,
                        tr!("That edit would turn the arrow into another shape, so it was undone."),
                    );
                };
                let Some(points) = arrow_shaft(points, shaft.len()) else {
                    return refuse(slide_id, tr!("That edit was undone."));
                };
                geometry = Geometry::Arrow { points };
            }
            // HPF fields keep their size: moving is fine, resizing is not.
            if layer.is_some_and(|l| l.role == LayerRole::Fields)
                && let Geometry::Ellipse { rx, ry, .. } = current.geometry
            {
                let c = geometry.anchor();
                geometry = Geometry::Ellipse {
                    cx: c[0],
                    cy: c[1],
                    rx,
                    ry,
                };
            }
            if let Geometry::Text { x, y } = geometry
                && !matches!(current.geometry, Geometry::Text { .. })
            {
                geometry = Geometry::Point { x, y };
            }
            let image = state
                .files
                .get(&file_id)
                .and_then(|f| image_bounds(&f.info));
            let normalized = match geometry::normalize(geometry, image) {
                Ok(n) => n,
                Err(e) => return refuse(slide_id, tr!("The edit was undone: {}", e)),
            };
            if normalized.repaired || normalized.parts.len() != 1 {
                return refuse(
                    slide_id,
                    tr!("That edit would make the outline cross itself, so it was undone."),
                );
            }
            let mut updated = (*current).clone();
            updated.geometry = normalized.parts.into_iter().next().expect("one part");
            if updated.geometry == current.geometry {
                // Nothing to store; an arrow whose head was dragged is redrawn.
                return Ok(Planned {
                    revert: matches!(current.geometry, Geometry::Arrow { .. })
                        .then(|| (slide_id, vec![id.clone()])),
                    ..Default::default()
                });
            }
            updated.updated_at = now_ms().max(current.updated_at + 1);
            Ok(Planned {
                changes: vec![Change::Annotation {
                    before: Some(current),
                    after: Some(Arc::new(updated)),
                }],
                revert: Some((slide_id, vec![id.clone()])),
                notice: normalized.clipped.then(outside_cut_off),
                ..Default::default()
            })
        },
    ));
}

/// Deletes annotations (Delete key, context menu, sidebar).
pub fn delete(core: &Arc<Core>, file_id: FileId, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let label = label!(tr!(
        "Delete annotation" | "Delete {n} annotations" % ids.len()
    ));
    core.submit(Op::new(label, Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
        let mut changes = Vec::new();
        let mut locked = 0;
        for id in &ids {
            let Some(a) = slide.annotations.get(id) else {
                continue;
            };
            if a.locked || slide.layer(&a.layer_id).is_some_and(|l| l.locked) {
                locked += 1;
                continue;
            }
            changes.push(Change::Annotation {
                before: Some(a.clone()),
                after: None,
            });
        }
        let warning = (locked > 0).then(|| {
            tr!("{n} locked annotation was kept." | "{n} locked annotations were kept." % locked)
        });
        Ok(Planned {
            changes,
            warning,
            ..Default::default()
        })
    }));
}

/// Deletes the objects the host asked to delete.
pub fn delete_objects(core: &Arc<Core>, objects: Vec<ObjectRef>) {
    let mut by_file: BTreeMap<FileId, Vec<String>> = BTreeMap::new();
    for object in objects {
        if let Some((file_id, _)) = parse_host_layer_id(&object.layer_id) {
            by_file.entry(file_id).or_default().push(object.object_id);
        }
    }
    for (file_id, ids) in by_file {
        delete(core, file_id, ids);
    }
}

/// Edits annotations' attributes (name, class, status, …).
pub fn edit(
    core: &Arc<Core>,
    file_id: FileId,
    ids: Vec<String>,
    label: Label,
    change: impl Fn(&mut Annotation) -> Result<(), String> + Send + 'static,
) {
    core.submit(Op::new(label, Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
        let now = now_ms();
        let mut changes = Vec::new();
        let mut locked = 0;
        for id in &ids {
            let Some(current) = slide.annotations.get(id) else {
                continue;
            };
            let mut updated = (**current).clone();
            change(&mut updated)?;
            if updated == **current {
                continue;
            }
            let layer_locked = slide.layer(&current.layer_id).is_some_and(|l| l.locked);
            // Locked annotations can only be unlocked.
            if (current.locked && updated.locked) || layer_locked {
                locked += 1;
                continue;
            }
            if updated.layer_id != current.layer_id
                && slide.layer(&updated.layer_id).is_none_or(|l| l.locked)
            {
                return Err(tr!("The target layer is locked or gone."));
            }
            updated.updated_at = now.max(current.updated_at + 1);
            changes.push(Change::Annotation {
                before: Some(current.clone()),
                after: Some(Arc::new(updated)),
            });
        }
        if changes.is_empty() && locked > 0 {
            return Err(tr!("{n} locked annotation was not changed."
                | "{n} locked annotations were not changed." % locked));
        }
        Ok(Planned::changes(changes))
    }));
}

/// Replaces the selection of a file (from the sidebar) and mirrors it to
/// the host.
pub fn select(core: &Arc<Core>, file_id: FileId, ids: Vec<String>) {
    let ops = {
        let mut state = core.state();
        let Some(file) = state.files.get_mut(&file_id) else {
            return;
        };
        file.selection = ids;
        overlay::select(&state, file_id)
    };
    core.exec(ops);
    core.request_refresh();
}

/// The host selection changed.
pub fn selection_changed(core: &Arc<Core>, objects: Vec<ObjectRef>) {
    let mut by_file: BTreeMap<FileId, Vec<String>> = BTreeMap::new();
    for object in objects {
        if let Some((file_id, _)) = parse_host_layer_id(&object.layer_id) {
            by_file.entry(file_id).or_default().push(object.object_id);
        }
    }
    let mut state = core.state();
    for (file_id, file) in state.files.iter_mut() {
        file.selection = by_file.remove(file_id).unwrap_or_default();
    }
    drop(state);
    core.request_refresh();
}

/// Frames an annotation in the focused pane.
pub fn frame(core: &Arc<Core>, file_id: FileId, id: &str) {
    let rect = {
        let state = core.state();
        let Some(a) = state.slide_of(file_id).and_then(|s| s.annotations.get(id)) else {
            return;
        };
        let b = a.geometry.bounds();
        // Points and small objects get a field of view of a few hundred µm
        // (or 512 px) rather than an extreme zoom (ANN-18).
        let file = state.files.get(&file_id);
        let min_size = file
            .and_then(|f| Calibration::new(f.info.mpp_x, f.info.mpp_y).um_to_px(400.0))
            .map_or(512.0, |(x, y)| x.max(y));
        let width = b.width().max(min_size);
        let height = b.height().max(min_size);
        let c = b.center();
        Rect {
            x: c[0] - width / 2.0,
            y: c[1] - height / 2.0,
            width,
            height,
        }
    };
    if let Err(e) = core.host.frame(None, rect) {
        core.report(
            NotifyLevel::Warning,
            tr!("Could not zoom to the annotation"),
            Some(e.to_string()),
            format!("cannot frame the annotation: {e}"),
        );
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

pub fn layer_new(core: &Arc<Core>, file_id: FileId) {
    core.submit(Op::new(
        label!(tr!("New layer")),
        Some(file_id),
        move |state| {
            let (slide, _) = editable_slide(state, file_id)?;
            // The name is data: English, whatever the language (see
            // `LayerRole::default_name`).
            let mut n = slide.layers.len() + 1;
            let mut name = format!("Layer {n}");
            while slide
                .layers
                .iter()
                .any(|l| l.name.eq_ignore_ascii_case(&name))
            {
                n += 1;
                name = format!("Layer {n}");
            }
            let now = now_ms();
            let layer = Layer {
                id: new_id(),
                slide_id: slide.id(),
                name,
                notes: String::new(),
                color: pick_color(slide.layers.iter().map(|l| &l.color)),
                visible: true,
                opacity: 1.0,
                locked: false,
                role: LayerRole::Normal,
                sort_order: slide.layers.iter().map(|l| l.sort_order).max().unwrap_or(0) + 1,
                created_at: now,
                updated_at: now,
            };
            Ok(Planned {
                current_layer: Some((slide.id(), layer.id.clone())),
                changes: vec![Change::Layer {
                    before: None,
                    after: Some(layer),
                }],
                ..Default::default()
            })
        },
    ));
}

/// Changes a layer's metadata. View toggles (visibility, opacity) are not
/// recorded for undo.
pub fn layer_update(
    core: &Arc<Core>,
    file_id: FileId,
    layer_id: String,
    label: Label,
    record: bool,
    change: impl FnOnce(&mut Layer) -> Result<(), String> + Send + 'static,
) {
    let op = Op::new(label, Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
        let current = slide.layer(&layer_id).cloned().ok_or_else(layer_gone)?;
        let mut updated = current.clone();
        change(&mut updated)?;
        if updated == current {
            return Ok(Planned::default());
        }
        updated.updated_at = now_ms().max(current.updated_at + 1);
        Ok(Planned::changes(vec![Change::Layer {
            before: Some(current),
            after: Some(updated),
        }]))
    });
    core.submit(if record { op } else { op.without_history() });
}

pub fn layer_rename(core: &Arc<Core>, file_id: FileId, layer_id: String, name: String) {
    layer_update(
        core,
        file_id,
        layer_id,
        label!(tr!("Rename layer")),
        true,
        move |layer| {
            layer.name = clean_name(&name, Field::LayerName, false)?;
            Ok(())
        },
    );
}

/// Deletes a layer with its contents (after the user confirmed).
pub fn layer_delete(core: &Arc<Core>, file_id: FileId, layer_id: String) {
    core.submit(Op::new(
        label!(tr!("Delete layer")),
        Some(file_id),
        move |state| {
            let (slide, _) = editable_slide(state, file_id)?;
            let layer = slide.layer(&layer_id).cloned().ok_or_else(layer_gone)?;
            if layer.locked {
                return Err(layer_locked(&layer.name));
            }
            let contents: Vec<Arc<Annotation>> = slide.in_layer(&layer_id).cloned().collect();
            Ok(Planned {
                label: Some(label!(tr!("Delete layer \"{}\"", layer.name))),
                changes: vec![Change::RemoveLayer { layer, contents }],
                ..Default::default()
            })
        },
    ));
}

pub fn set_current_layer(core: &Arc<Core>, file_id: FileId, layer_id: String) {
    core.submit(
        Op::new(label!(tr!("Choose layer")), Some(file_id), move |state| {
            let slide = state.slide_of(file_id).ok_or_else(loading)?;
            let layer = slide.layer(&layer_id).ok_or_else(layer_gone)?;
            if layer.role != LayerRole::Normal {
                return Err(tr!(
                    "New annotations go to a regular layer; count points and HPF fields have their own."
                ));
            }
            Ok(Planned {
                current_layer: Some((slide.id(), layer_id.clone())),
                ..Default::default()
            })
        })
        .without_history(),
    );
}

/// Removes every count point of a slide (the counts layer; undoable).
pub fn clear_counts(core: &Arc<Core>, file_id: FileId) {
    core.submit(Op::new(
        // Translators: an undo step, shown by eov as "Undo {}".
        label!(tr!("Clear counts")),
        Some(file_id),
        move |state| {
            let (slide, _) = editable_slide(state, file_id)?;
            let Some(layer) = slide.layer_by_role(LayerRole::Counts).cloned() else {
                return Ok(Planned::default());
            };
            let contents: Vec<Arc<Annotation>> = slide.in_layer(&layer.id).cloned().collect();
            Ok(Planned::changes(vec![Change::RemoveLayer {
                layer,
                contents,
            }]))
        },
    ));
}

/// Selects every HPF field of a slide (to count inside them).
pub fn select_fields(core: &Arc<Core>, file_id: FileId) {
    let ids: Vec<String> = {
        let state = core.state();
        let Some(slide) = state.slide_of(file_id) else {
            return;
        };
        match slide.layer_by_role(LayerRole::Fields) {
            Some(layer) => slide.in_layer(&layer.id).map(|a| a.id.clone()).collect(),
            None => Vec::new(),
        }
    };
    if ids.is_empty() {
        core.notify(
            NotifyLevel::Info,
            tr!(
                "There are no HPF fields on this slide yet. Use the HPF field tool (H) to place them."
            ),
        );
    }
    select(core, file_id, ids);
}

/// Repairs a self-intersecting polygon from an older version.
pub fn repair(core: &Arc<Core>, file_id: FileId, id: String) {
    core.submit(Op::new(
        label!(tr!("Repair outline")),
        Some(file_id),
        move |state| {
            let (slide, image) = editable_slide(state, file_id)?;
            let current = slide
                .annotations
                .get(&id)
                .cloned()
                .ok_or_else(annotation_gone)?;
            let normalized = geometry::normalize(current.geometry.clone(), image)?;
            let now = now_ms();
            let mut parts = normalized.parts.into_iter();
            let mut updated = (*current).clone();
            updated.geometry = parts
                .next()
                .ok_or_else(|| tr!("Nothing is left of the outline."))?;
            updated.updated_at = now.max(current.updated_at + 1);
            let mut changes = vec![Change::Annotation {
                before: Some(current.clone()),
                after: Some(Arc::new(updated)),
            }];
            for part in parts {
                let mut extra = (*current).clone();
                extra.id = new_id();
                extra.geometry = part;
                extra.created_at = now;
                extra.updated_at = now;
                changes.push(Change::Annotation {
                    before: None,
                    after: Some(Arc::new(extra)),
                });
            }
            let n = changes.len();
            Ok(Planned {
                changes,
                notice: (n > 1).then(|| {
                    // Translators: {n} is 2 or more.
                    tr!("The outline was split into {n} parts."
                        | "The outline was split into {n} parts." % n)
                }),
                ..Default::default()
            })
        },
    ));
}

/// Moves the layers of another slide record to this slide (ANN-8).
pub fn relink(core: &Arc<Core>, file_id: FileId, from: SlideId) {
    core.db_run(move |core, ctx| {
        let layer_ids = match ctx.db.as_ref().map(|db| db.layer_ids(from)) {
            Some(Ok(ids)) => ids,
            Some(Err(e)) => {
                core.error(tr!("Could not attach the annotations"), e.to_string());
                return;
            }
            None => return,
        };
        let op = Op::new(
            label!(tr!("Attach annotations of another slide")),
            Some(file_id),
            move |state| {
                state.writable()?;
                let to = state.slide_of(file_id).ok_or_else(loading)?.id();
                if to == from || to <= 0 {
                    return Err(tr!("Those annotations already belong to this slide."));
                }
                if layer_ids.is_empty() {
                    return Err(tr!("That slide has no annotations left."));
                }
                Ok(Planned {
                    changes: vec![Change::MoveLayers {
                        from,
                        to,
                        layer_ids: layer_ids.clone(),
                    }],
                    notice: Some(tr!("Attached {n} layer to this slide."
                        | "Attached {n} layers to this slide." % layer_ids.len())),
                    ..Default::default()
                })
            },
        );
        crate::core::run_op(core, ctx, op);
        core.state().orphans = None;
        list_orphans(core);
    });
}

/// Lists the slides of the database (for relinking).
pub fn list_orphans(core: &Arc<Core>) {
    core.db_run(|core, ctx| {
        let Some(db) = ctx.db.as_ref() else {
            return;
        };
        match db.slide_summaries() {
            Ok(list) => {
                let mut state = core.state();
                let open: Vec<SlideId> =
                    state.files.values().filter_map(|f| f.slide_id()).collect();
                state.orphans = Some(
                    list.into_iter()
                        .filter(|s| !open.contains(&s.record.id))
                        .collect(),
                );
            }
            // The Data tab lists none; for the log.
            Err(e) => core.host.info(format!("cannot list slides: {e}")),
        }
        core.request_refresh();
    });
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

pub fn class_new(core: &Arc<Core>) {
    core.submit(Op::new(label!(tr!("New class")), None, |state| {
        state.writable()?;
        // The name is data: English, whatever the language (see
        // `LayerRole::default_name`).
        let mut n = state.classes.len() + 1;
        let mut name = format!("Class {n}");
        while state.class_by_name(&name).is_some() {
            n += 1;
            name = format!("Class {n}");
        }
        let now = now_ms();
        Ok(Planned::changes(vec![Change::Class {
            before: None,
            after: Some(Class {
                id: new_id(),
                name,
                color: pick_color(state.classes.iter().map(|c| &c.color)),
                hotkey: None,
                visible: true,
                opacity: 1.0,
                sort_order: state
                    .classes
                    .iter()
                    .map(|c| c.sort_order)
                    .max()
                    .unwrap_or(0)
                    + 1,
                created_at: now,
                updated_at: now,
            }),
        }]))
    }));
}

pub fn class_update(
    core: &Arc<Core>,
    class_id: String,
    label: Label,
    record: bool,
    change: impl FnOnce(&State, &mut Class) -> Result<(), String> + Send + 'static,
) {
    let op = Op::new(label, None, move |state| {
        state.writable()?;
        let current = state.class(&class_id).cloned().ok_or_else(class_gone)?;
        let mut updated = current.clone();
        change(state, &mut updated)?;
        if updated == current {
            return Ok(Planned::default());
        }
        updated.updated_at = now_ms().max(current.updated_at + 1);
        Ok(Planned::changes(vec![Change::Class {
            before: Some(current),
            after: Some(updated),
        }]))
    });
    core.submit(if record { op } else { op.without_history() });
}

pub fn class_rename(core: &Arc<Core>, class_id: String, name: String) {
    class_update(
        core,
        class_id,
        label!(tr!("Rename class")),
        true,
        move |state, class| {
            let name = clean_name(&name, Field::ClassName, false)?;
            if state
                .class_by_name(&name)
                .is_some_and(|other| other.id != class.id)
            {
                return Err(tr!(
                    "A class named \"{}\" already exists.",
                    i18n::isolate(&name)
                ));
            }
            class.name = name;
            Ok(())
        },
    );
}

pub fn class_hotkey(core: &Arc<Core>, class_id: String, key: String) {
    class_update(
        core,
        class_id,
        label!(tr!("Set class hotkey")),
        true,
        move |state, class| {
            let mut taken: Vec<String> = tools::tool_hotkeys()
                .iter()
                .map(|k| k.to_string())
                .collect();
            taken.extend(
                state
                    .classes
                    .iter()
                    .filter(|c| c.id != class.id)
                    .filter_map(|c| c.hotkey.clone()),
            );
            let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
            class.hotkey = crate::model::clean_hotkey(&key, &taken)?;
            Ok(())
        },
    );
}

pub fn class_color(core: &Arc<Core>, class_id: String, color: String) {
    class_update(
        core,
        class_id,
        label!(tr!("Change class colour")),
        true,
        move |_, class| {
            class.color =
                Color::parse(&color).ok_or_else(|| tr!("\"{}\" is not a colour.", color))?;
            Ok(())
        },
    );
}

pub fn class_delete(core: &Arc<Core>, class_id: String) {
    core.submit(Op::new(label!(tr!("Delete class")), None, move |state| {
        state.writable()?;
        let current = state.class(&class_id).cloned().ok_or_else(class_gone)?;
        Ok(Planned {
            label: Some(label!(tr!("Delete class \"{}\"", current.name))),
            changes: vec![Change::Class {
                before: Some(current),
                after: None,
            }],
            ..Default::default()
        })
    }));
}

/// Annotations of a class in loaded slides (for the delete confirmation).
pub fn class_usage(state: &State, class_id: &str) -> usize {
    state
        .slides
        .values()
        .map(|s| {
            s.annotations
                .values()
                .filter(|a| a.class_id.as_deref() == Some(class_id))
                .count()
        })
        .sum()
}

/// Sets the class new annotations get (and plain count clicks).
pub fn set_active_class(core: &Arc<Core>, class_id: Option<String>) {
    {
        let mut state = core.state();
        if class_id
            .as_ref()
            .is_some_and(|id| state.class(id).is_none())
        {
            return;
        }
        state.settings.active_class = class_id.clone();
        if state.settings.alternate_class == class_id {
            state.settings.alternate_class = None;
        }
        let _ = core.host.set_setting("settings", &state.settings);
        let name = class_id
            .as_deref()
            .and_then(|id| state.class(id))
            .map_or_else(|| tr!("Unclassified"), |c| i18n::isolate(&c.name));
        core.host.set_status(Some(&tr!("Class: {}", name)));
    }
    core.request_refresh();
}

pub fn set_alternate_class(core: &Arc<Core>, class_id: Option<String>) {
    {
        let mut state = core.state();
        state.settings.alternate_class = class_id;
        let _ = core.host.set_setting("settings", &state.settings);
    }
    core.request_refresh();
}

// ---------------------------------------------------------------------------
// HPF field tool: a reticle follows the pointer, a click places a field
// ---------------------------------------------------------------------------

/// Pointer travel (logical pixels) after which a press pans instead of
/// placing a field.
const FIELD_DRAG_PX: f64 = 4.0;

/// The reticle's label, from its area and what it would count: as shown,
/// in the language of the interface, and in English for images (eov burns
/// labels into exports and copies of the view, with `.` decimals and a font
/// that has Latin and Greek letters only).
fn reticle_label(area_mm2: f64, counted: u64) -> (String, String) {
    let shown = tr!(
        // Translators: on the slide, beside the HPF field that follows the
        // pointer: its area and how many points (such as counted cells) lie in it.
        "1 HPF · {area} mm² · {n} point" | "1 HPF · {area} mm² · {n} points" % counted,
        area = i18n::format_decimal(area_mm2, 4)
    );
    let area = i18n::format_decimal_for(i18n::source(), area_mm2, 4);
    let english = i18n::format(
        if counted == 1 {
            "1 HPF · {area} mm² · {n} point"
        } else {
            "1 HPF · {area} mm² · {n} points"
        },
        &[
            ("area", &area as &dyn std::fmt::Display),
            ("n", &counted as &dyn std::fmt::Display),
        ],
    );
    (shown, english)
}

/// Host calls for a file's HPF reticle: one dashed field, the size of an
/// HPF, at the pointer, labelled with what it would count.
pub fn reticle_ops(state: &mut State, file_id: FileId) -> Vec<HostOp> {
    let layer_id = reticle_layer_id(file_id);
    let active = state.active_tool.as_deref() == Some(tools::FIELD);
    let Some(file) = state.files.get(&file_id) else {
        return Vec::new();
    };
    let object = file
        .reticle
        .filter(|_| active)
        .and_then(|[x, y]| field_geometry(state, &file.info, x, y).ok())
        .map(|field| {
            let counted = state
                .slide_of(file_id)
                .map_or(0, |s| crate::counting::count_in(s, &field));
            let area = state.settings.field.area_mm2().unwrap_or(0.0);
            let (label, english) = reticle_label(area, counted);
            OverlayObject::new("reticle", field.to_shape(""))
                .with_style(Style {
                    fill: Rgba(0xFF, 0xFF, 0xFF, 0x10),
                    stroke: Rgba(0xFF, 0xFF, 0xFF, 0xE0),
                    stroke_width: 1.5,
                    dash: Dash::Dashed,
                    ..Style::default()
                })
                .with_label(label)
                .with_export_label(english)
        });
    let had_layer = file.reticle_layer;
    let Some(file) = state.files.get_mut(&file_id) else {
        return Vec::new();
    };
    match object {
        Some(object) => {
            let mut ops = Vec::new();
            if !had_layer {
                let mut spec = LayerSpec::new(layer_id.clone(), file_id)
                    .named("HPF reticle")
                    .at_z(90);
                spec.interactive = false;
                ops.push(HostOp::CreateLayer(spec));
                file.reticle_layer = true;
            }
            ops.push(HostOp::Upsert(layer_id, vec![object]));
            ops
        }
        None if had_layer => {
            file.reticle_layer = false;
            vec![HostOp::RemoveLayer(layer_id)]
        }
        None => Vec::new(),
    }
}

/// Refreshes the reticle of every file (the field size or the tool changed).
pub fn refresh_reticles(core: &Arc<Core>) {
    let ops: Vec<HostOp> = {
        let mut state = core.state();
        if state.active_tool.as_deref() != Some(tools::FIELD) {
            for file in state.files.values_mut() {
                file.reticle = None;
            }
            state.field_press = None;
        }
        let files: Vec<FileId> = state.files.keys().copied().collect();
        files
            .into_iter()
            .flat_map(|id| reticle_ops(&mut state, id))
            .collect()
    };
    core.exec(ops);
}

/// Moves (or with `None` hides) a file's reticle.
fn set_reticle(core: &Arc<Core>, file_id: FileId, at: Option<[f64; 2]>) {
    let ops = {
        let mut state = core.state();
        for (id, file) in state.files.iter_mut() {
            // One reticle: the pointer is over one pane at a time.
            file.reticle = if *id == file_id { at } else { None };
        }
        let files: Vec<FileId> = state.files.keys().copied().collect();
        files
            .into_iter()
            .flat_map(|id| reticle_ops(&mut state, id))
            .collect::<Vec<_>>()
    };
    core.exec(ops);
}

/// Pointer input of the HPF field tool (a `Custom` tool, so that the
/// reticle can follow the pointer): a click places a field, a drag pans.
pub fn field_pointer(core: &Arc<Core>, pane: PaneId, file_id: FileId, event: PointerEvent) {
    let screen = [event.screen_x, event.screen_y];
    let image = [event.image_x, event.image_y];
    match event.kind {
        PointerKind::Down if event.button == PointerButton::Left => {
            core.state().field_press = Some(FieldPress {
                pane,
                file_id,
                screen,
                image,
                last: screen,
                dragging: false,
                double: false,
            });
            set_reticle(core, file_id, Some(image));
        }
        PointerKind::DoubleClick => {
            if let Some(press) = core.state().field_press.as_mut() {
                press.double = true;
            }
        }
        PointerKind::Move | PointerKind::Wheel => {
            let pan = {
                let mut state = core.state();
                match state.field_press.as_mut() {
                    Some(press) if press.pane == pane => {
                        let travel =
                            (screen[0] - press.screen[0]).hypot(screen[1] - press.screen[1]);
                        if travel > FIELD_DRAG_PX {
                            press.dragging = true;
                        }
                        let delta = [screen[0] - press.last[0], screen[1] - press.last[1]];
                        press.last = screen;
                        press.dragging.then_some(delta)
                    }
                    _ => None,
                }
            };
            if let Some([dx, dy]) = pan
                && (dx != 0.0 || dy != 0.0)
            {
                // The slide follows the pointer, as with eov's own tools.
                let _ = core.host.pan(Some(pane), -dx, -dy);
            }
            set_reticle(core, file_id, Some(image));
        }
        PointerKind::Up => {
            let press = core.state().field_press.take();
            if let Some(press) = press
                && press.pane == pane
                && !press.dragging
                && !press.double
            {
                shape_drawn(
                    core,
                    tools::FIELD,
                    press.file_id,
                    Shape::Point {
                        x: press.image[0],
                        y: press.image[1],
                    },
                    event.modifiers,
                );
            }
        }
        PointerKind::Leave => {
            core.state().field_press = None;
            set_reticle(core, file_id, None);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// Adds a comment by the current author (also on locked annotations: a
/// comment does not change the annotation itself).
pub fn comment_add(core: &Arc<Core>, file_id: FileId, annotation_id: String, text: String) {
    core.submit(Op::new(
        label!(tr!("Add comment")),
        Some(file_id),
        move |state| {
            state.writable()?;
            let text = clean_text(&text, Field::Comment)?;
            if text.trim().is_empty() {
                return Err(tr!("Write the comment first."));
            }
            let slide = state.slide_of(file_id).ok_or_else(loading)?;
            let current = slide
                .annotations
                .get(&annotation_id)
                .cloned()
                .ok_or_else(annotation_gone)?;
            if current.comments.len() >= MAX_COMMENTS {
                return Err(tr!("An annotation can have at most {n} comments."
                    | "An annotation can have at most {n} comments." % MAX_COMMENTS));
            }
            let now = now_ms();
            let mut updated = (*current).clone();
            updated.comments.push(Comment {
                id: new_id(),
                author: state.settings.author.clone(),
                text: text.trim().to_string(),
                created_at: now,
            });
            updated.updated_at = now.max(current.updated_at + 1);
            Ok(Planned {
                changes: vec![Change::Annotation {
                    before: Some(current),
                    after: Some(Arc::new(updated)),
                }],
                ..Default::default()
            })
        },
    ));
}

pub fn comment_remove(
    core: &Arc<Core>,
    file_id: FileId,
    annotation_id: String,
    comment_id: String,
) {
    core.submit(Op::new(
        label!(tr!("Delete comment")),
        Some(file_id),
        move |state| {
            state.writable()?;
            let slide = state.slide_of(file_id).ok_or_else(loading)?;
            let current = slide
                .annotations
                .get(&annotation_id)
                .cloned()
                .ok_or_else(annotation_gone)?;
            let mut updated = (*current).clone();
            updated.comments.retain(|c| c.id != comment_id);
            if updated.comments.len() == current.comments.len() {
                return Err(tr!("That comment no longer exists."));
            }
            updated.updated_at = now_ms().max(current.updated_at + 1);
            Ok(Planned {
                changes: vec![Change::Annotation {
                    before: Some(current),
                    after: Some(Arc::new(updated)),
                }],
                ..Default::default()
            })
        },
    ));
}

// ---------------------------------------------------------------------------
// Brush: paint a region, or erase from the selected one
// ---------------------------------------------------------------------------

pub fn brush_layer_id(file_id: FileId) -> String {
    format!("brush.{file_id}")
}

/// Host calls for a file's brush preview: the brush outline at the pointer
/// and the stroke painted so far.
pub fn brush_ops(state: &mut State, file_id: FileId) -> Vec<HostOp> {
    let layer_id = brush_layer_id(file_id);
    let active = state.active_tool.as_deref() == Some(tools::BRUSH);
    let size = state.settings.brush_size;
    let eraser = state.settings.brush_eraser;
    let mut objects = Vec::new();
    if active {
        let erasing = state.brush.as_ref().map_or(eraser, |s| s.erase);
        let tint = if erasing {
            Rgba(0xFF, 0x50, 0x50, 0xFF)
        } else {
            Rgba(0xFF, 0xFF, 0xFF, 0xFF)
        };
        if let Some(stroke) = state.brush.as_ref().filter(|s| s.file_id == file_id) {
            let style = Style {
                fill: tint.with_alpha(0x50),
                stroke: tint.with_alpha(0x50),
                stroke_width: size,
                point_radius: size / 2.0,
                ..Style::default()
            };
            let shape = match stroke.points.as_slice() {
                [p] => Shape::Point { x: p[0], y: p[1] },
                points => Shape::Polyline {
                    points: points.to_vec(),
                },
            };
            objects.push(OverlayObject::new("stroke", shape).with_style(style));
        }
        if let Some((_, at)) = state.brush_cursor.filter(|(f, _)| *f == file_id) {
            objects.push(
                OverlayObject::new("outline", Shape::Point { x: at[0], y: at[1] }).with_style(
                    Style {
                        fill: Rgba::TRANSPARENT,
                        stroke: tint.with_alpha(0xE0),
                        stroke_width: 1.5,
                        point_radius: size / 2.0,
                        ..Style::default()
                    },
                ),
            );
        }
    }
    let Some(file) = state.files.get_mut(&file_id) else {
        return Vec::new();
    };
    if objects.is_empty() {
        if !file.brush_layer {
            return Vec::new();
        }
        file.brush_layer = false;
        return vec![HostOp::RemoveLayer(layer_id)];
    }
    let mut ops = Vec::new();
    if !file.brush_layer {
        let mut spec = LayerSpec::new(layer_id.clone(), file_id)
            .named("Brush")
            .at_z(95);
        spec.interactive = false;
        ops.push(HostOp::CreateLayer(spec));
        file.brush_layer = true;
    }
    ops.push(HostOp::Replace(layer_id, objects));
    ops
}

/// Refreshes the brush preview of every file.
pub fn refresh_brush(core: &Arc<Core>) {
    let ops: Vec<HostOp> = {
        let mut state = core.state();
        if state.active_tool.as_deref() != Some(tools::BRUSH) {
            state.brush = None;
            state.brush_cursor = None;
        }
        let files: Vec<FileId> = state.files.keys().copied().collect();
        files
            .into_iter()
            .flat_map(|id| brush_ops(&mut state, id))
            .collect()
    };
    core.exec(ops);
}

/// Level-0 pixels per screen pixel in a pane (1 when unknown).
fn pane_pixel(core: &Arc<Core>, pane: PaneId) -> f64 {
    core.host
        .snapshot()
        .ok()
        .and_then(|s| {
            s.panes
                .iter()
                .find(|p| p.pane == pane)
                .and_then(|p| p.view.as_ref())
                .map(|v| v.zoom)
        })
        .filter(|z| z.is_finite() && *z > 0.0)
        .map_or(1.0, |z| 1.0 / z)
}

/// Pointer input of the brush (a `Custom` tool).
pub fn brush_pointer(core: &Arc<Core>, pane: PaneId, file_id: FileId, event: PointerEvent) {
    let image = [event.image_x, event.image_y];
    match event.kind {
        PointerKind::Down if event.button == PointerButton::Left => {
            let pixel = pane_pixel(core, pane);
            let mut state = core.state();
            let radius = f64::from(state.settings.brush_size) / 2.0 * pixel;
            let erase = state.settings.brush_eraser || event.modifiers.alt;
            state.brush = Some(BrushStroke {
                pane,
                file_id,
                points: vec![image],
                radius,
                pixel,
                erase,
            });
            state.brush_cursor = Some((file_id, image));
        }
        PointerKind::Move | PointerKind::Wheel => {
            let mut state = core.state();
            state.brush_cursor = Some((file_id, image));
            if let Some(stroke) = state
                .brush
                .as_mut()
                .filter(|s| s.pane == pane && s.file_id == file_id)
                && stroke
                    .points
                    .last()
                    .is_none_or(|last| geometry::distance(*last, image) >= stroke.radius * 0.25)
            {
                stroke.points.push(image);
            }
        }
        PointerKind::Up | PointerKind::Leave => {
            let stroke = core.state().brush.take();
            if event.kind == PointerKind::Leave {
                core.state().brush_cursor = None;
            }
            if let Some(mut stroke) = stroke.filter(|s| s.file_id == file_id) {
                if event.kind == PointerKind::Up && stroke.points.last() != Some(&image) {
                    stroke.points.push(image);
                }
                // Simplified to a quarter of a screen pixel.
                let regions =
                    geometry::brush_stroke(&stroke.points, stroke.radius, stroke.pixel * 0.25);
                brush_apply(core, file_id, regions, stroke.erase);
            }
        }
        _ => {}
    }
    refresh_brush(core);
}

/// `[` and `]` while the brush is active: a smaller or bigger brush.
pub fn brush_key(core: &Arc<Core>, key: &str) {
    let factor = match key_name(key).as_str() {
        "[" => 1.0 / 1.25,
        "]" => 1.25,
        _ => return,
    };
    {
        let mut state = core.state();
        let size = (state.settings.brush_size * factor).round();
        state.settings.brush_size = size.clamp(*BRUSH_SIZES.start(), *BRUSH_SIZES.end());
        let _ = core.host.set_setting("settings", &state.settings);
        core.host.set_status(Some(&brush_status(&state)));
    }
    refresh_brush(core);
    core.request_refresh();
}

pub fn brush_status(state: &State) -> String {
    let size = state.settings.brush_size;
    if state.settings.brush_eraser {
        tr!(
            "Brush {} px, eraser: paint over a selected region to erase from it; [ and ] change the size",
            size
        )
    } else {
        tr!(
            "Brush {} px: paint a region (over a selected region to extend it); Alt erases; [ and ] change the size",
            size
        )
    }
}

/// Applies a brush stroke as one change (one undo step): erasing cuts it
/// from the selected regions it touches; painting extends the first
/// selected region it overlaps, or makes a new region.
fn brush_apply(core: &Arc<Core>, file_id: FileId, stroke: Vec<Geometry>, erase: bool) {
    if stroke.is_empty() {
        return;
    }
    let label = if erase {
        label!(tr!("Erase"))
    } else {
        label!(tr!("Brush stroke"))
    };
    core.submit(Op::new(label, Some(file_id), move |state| {
        let (slide, image) = editable_slide(state, file_id)?;
        let selection = state
            .files
            .get(&file_id)
            .map(|f| f.selection.clone())
            .unwrap_or_default();
        let editable =
            |a: &Annotation| !a.locked && slide.layer(&a.layer_id).is_none_or(|l| !l.locked);
        let targets: Vec<Arc<Annotation>> = selection
            .iter()
            .filter_map(|id| slide.annotations.get(id))
            .filter(|a| a.geometry.kind().is_area())
            .cloned()
            .collect();
        let mut parts = Vec::new();
        for g in stroke {
            if let Ok(n) = geometry::normalize(g, image) {
                parts.extend(n.parts);
            }
        }
        if parts.is_empty() {
            return Err(tr!("The stroke is outside the slide."));
        }
        let bounds = parts
            .iter()
            .map(Geometry::bounds)
            .reduce(|a, b| Bounds {
                min_x: a.min_x.min(b.min_x),
                min_y: a.min_y.min(b.min_y),
                max_x: a.max_x.max(b.max_x),
                max_y: a.max_y.max(b.max_y),
            })
            .unwrap_or_default();
        let now = now_ms();
        let split = |target: &Arc<Annotation>,
                     mut pieces: Vec<Geometry>,
                     changes: &mut Vec<Change>,
                     ids: &mut Vec<String>| {
            if pieces.is_empty() {
                changes.push(Change::Annotation {
                    before: Some(target.clone()),
                    after: None,
                });
                return;
            }
            let first = pieces.remove(0);
            let mut updated = (**target).clone();
            updated.geometry = first;
            updated.updated_at = now.max(target.updated_at + 1);
            ids.push(updated.id.clone());
            changes.push(Change::Annotation {
                before: Some(target.clone()),
                after: Some(Arc::new(updated)),
            });
            for piece in pieces {
                let mut part = (**target).clone();
                part.id = new_id();
                part.geometry = piece;
                part.created_at = now;
                part.updated_at = now;
                ids.push(part.id.clone());
                changes.push(Change::Annotation {
                    before: None,
                    after: Some(Arc::new(part)),
                });
            }
        };
        let mut changes = Vec::new();
        let mut ids = Vec::new();
        if erase {
            if targets.is_empty() {
                return Err(tr!("Select the region to erase from."));
            }
            for target in &targets {
                if !editable(target) || !target.geometry.bounds().intersects(&bounds) {
                    ids.push(target.id.clone());
                    continue;
                }
                let mut pieces = vec![target.geometry.clone()];
                for part in &parts {
                    pieces = pieces
                        .iter()
                        .flat_map(|g| {
                            geometry::difference(g, part).unwrap_or_else(|| vec![g.clone()])
                        })
                        .collect();
                }
                if pieces.len() == 1
                    && geometry::area(&pieces[0]) >= geometry::area(&target.geometry) - 1e-9
                {
                    ids.push(target.id.clone());
                    continue;
                }
                split(target, pieces, &mut changes, &mut ids);
            }
            if changes.is_empty() {
                return Err(tr!("The eraser did not touch the selected region."));
            }
            return Ok(Planned {
                changes,
                select: Some((file_id, ids)),
                ..Default::default()
            });
        }
        for target in targets.iter().filter(|t| editable(t)) {
            if !target.geometry.bounds().intersects(&bounds) {
                continue;
            }
            let mut merged = target.geometry.clone();
            let mut separate = Vec::new();
            let mut touched = false;
            for part in &parts {
                match geometry::union(&merged, part) {
                    Some(mut u) if u.len() == 1 => {
                        merged = u.remove(0);
                        touched = true;
                    }
                    _ => separate.push(part.clone()),
                }
            }
            if touched {
                let mut pieces = vec![merged];
                pieces.extend(separate);
                split(target, pieces, &mut changes, &mut ids);
                return Ok(Planned {
                    changes,
                    select: Some((file_id, ids)),
                    ..Default::default()
                });
            }
        }
        let layer = target_layer(slide, LayerRole::Normal, &mut changes)?;
        let class = state
            .settings
            .active_class
            .clone()
            .filter(|id| state.class(id).is_some());
        let author = state.settings.author.clone();
        for part in parts {
            let a = annotation(&layer, part, class.clone(), "", &author);
            ids.push(a.id.clone());
            changes.push(Change::Annotation {
                before: None,
                after: Some(Arc::new(a)),
            });
        }
        Ok(Planned {
            changes,
            select: Some((file_id, ids)),
            current_layer: Some((slide.id(), layer.id.clone())),
            ..Default::default()
        })
    }));
}

/// Annotation-kind label for lists.
pub fn kind_glyph(kind: GeometryKind) -> &'static str {
    match kind {
        GeometryKind::Point => "•",
        GeometryKind::Polyline => "╱",
        GeometryKind::Arrow => "→",
        GeometryKind::Polygon => "△",
        GeometryKind::Rectangle => "▭",
        GeometryKind::Ellipse => "○",
        GeometryKind::Text => "T",
    }
}

/// Queues a check for changes made by another eov window.
pub fn check_external(core: &Arc<Core>) {
    core.db(DbJob::Run(Box::new(crate::core::check_external)));
}
