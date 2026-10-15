//! What user actions do. Handlers run on the UI thread, read the state and
//! queue [`Op`]s whose plans run on the database thread against the
//! up-to-date state.

use crate::change::Change;
use crate::core::{Core, DbJob, Op, PALETTE_TOKEN, Planned};
use crate::geometry::{self, Normalized};
use crate::measure::Calibration;
use crate::model::{
    Annotation, Bounds, Class, Color, Geometry, GeometryKind, Layer, LayerRole, SlideId, Status,
    clean_name, new_id, now_ms, pick_color,
};
use crate::overlay::{self, HostOp, parse_host_layer_id, reticle_layer_id};
use crate::state::{SlideData, State};
use crate::tools;
use eov_plugin_api::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

const LOADING: &str = "The annotations of this slide are still loading; try again in a moment.";

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
        .ok_or("The slide is not open any more.")?;
    let slide = state.slide_of(file_id).ok_or(LOADING)?;
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
            return Err(format!("Layer \"{}\" is locked.", layer.name));
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
        created_at: now,
        updated_at: now,
    }
}

fn notes_for(normalized: &Normalized) -> Option<String> {
    let mut notes = Vec::new();
    if normalized.repaired {
        notes.push(if normalized.parts.len() > 1 {
            format!(
                "The outline crossed itself and was split into {} parts.",
                normalized.parts.len()
            )
        } else {
            "The outline crossed itself and was repaired.".to_string()
        });
    }
    if normalized.clipped {
        notes.push("Parts outside the slide were cut off.".to_string());
    }
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// The geometry of an HPF field centred at `(x, y)`.
pub fn field_geometry(state: &State, info: &FileInfo, x: f64, y: f64) -> Result<Geometry, String> {
    let calibration = Calibration::new(info.mpp_x, info.mpp_y);
    let area = state
        .settings
        .field
        .area_mm2()
        .ok_or("The HPF size is not valid; check it in the Count tab.")?;
    let radius_um = (area / std::f64::consts::PI).sqrt() * 1000.0;
    let (rx, ry) = calibration
        .um_to_px(radius_um)
        .ok_or("This slide has no pixel size, so HPF fields cannot be sized.")?;
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
            let class = if modifiers.shift {
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
                "Count cell",
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
            "Add point",
        ),
        tools::POLYLINE => add(
            core,
            file_id,
            vec![geometry],
            LayerRole::Normal,
            active,
            "",
            true,
            "Add line",
        ),
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
                "Add text",
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
                        "Select one region to add to (Shift) or cut from (Alt). The shape was added as a new annotation.",
                    );
                    add(
                        core,
                        file_id,
                        vec![geometry],
                        LayerRole::Normal,
                        active,
                        "",
                        true,
                        "Add region",
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
                    "Add region",
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
    label: &str,
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
    core.submit(Op::new("Place HPF field", Some(file_id), move |state| {
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
                    f.min_x < b.min_x || f.min_y < b.min_y || f.max_x > b.max_x || f.max_y > b.max_y
                })
            })
            .then(|| {
                "This field extends past the slide edge; its area includes empty canvas."
                    .to_string()
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
    }));
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
        "Add to region"
    } else {
        "Cut from region"
    };
    core.submit(Op::new(label, Some(file_id), move |state| {
        let (slide, image) = editable_slide(state, file_id)?;
        let target = slide
            .annotations
            .get(&target_id)
            .cloned()
            .ok_or("The selected region no longer exists.")?;
        if target.locked || slide.layer(&target.layer_id).is_some_and(|l| l.locked) {
            return Err("The selected region is locked.".to_string());
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
            .ok_or("Only regions can be combined.")?;
            match parts.len() {
                0 => {
                    return Ok(Planned {
                        changes: vec![Change::Annotation {
                            before: Some(target.clone()),
                            after: None,
                        }],
                        notice: Some("The cut removed the whole region.".to_string()),
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
            Some(
                "The shapes do not touch; the new shape was added as a separate annotation."
                    .to_string(),
            )
        } else if !extra.is_empty() {
            Some(format!(
                "The cut split the region into {} parts.",
                extra.len() + 1
            ))
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
    core.submit(Op::new("Edit shape", Some(file_id), move |state| {
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
            return refuse(slide_id, "That annotation no longer exists.".to_string());
        };
        let layer = slide.layer(&current.layer_id);
        if current.locked || layer.is_some_and(|l| l.locked) {
            return refuse(slide_id, "The annotation is locked.".to_string());
        }
        let (mut geometry, _) = Geometry::from_shape(&shape);
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
            Err(e) => return refuse(slide_id, format!("The edit was undone: {e}")),
        };
        if normalized.repaired || normalized.parts.len() != 1 {
            return refuse(
                slide_id,
                "That edit would make the outline cross itself, so it was undone.".to_string(),
            );
        }
        let mut updated = (*current).clone();
        updated.geometry = normalized.parts.into_iter().next().expect("one part");
        if updated.geometry == current.geometry {
            return Ok(Planned::default());
        }
        updated.updated_at = now_ms().max(current.updated_at + 1);
        Ok(Planned {
            changes: vec![Change::Annotation {
                before: Some(current),
                after: Some(Arc::new(updated)),
            }],
            revert: Some((slide_id, vec![id.clone()])),
            notice: normalized
                .clipped
                .then(|| "Parts outside the slide were cut off.".to_string()),
            ..Default::default()
        })
    }));
}

/// Deletes annotations (Delete key, context menu, sidebar).
pub fn delete(core: &Arc<Core>, file_id: FileId, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let label = if ids.len() == 1 {
        "Delete annotation".to_string()
    } else {
        format!("Delete {} annotations", ids.len())
    };
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
        let warning = (locked > 0).then(|| format!("{locked} locked annotation(s) were kept."));
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
    label: impl Into<String>,
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
                return Err("The target layer is locked or gone.".to_string());
            }
            updated.updated_at = now.max(current.updated_at + 1);
            changes.push(Change::Annotation {
                before: Some(current.clone()),
                after: Some(Arc::new(updated)),
            });
        }
        if changes.is_empty() && locked > 0 {
            return Err(format!("{locked} locked annotation(s) were not changed."));
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
        core.host.warn(format!("cannot frame the annotation: {e}"));
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

pub fn layer_new(core: &Arc<Core>, file_id: FileId) {
    core.submit(Op::new("New layer", Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
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
    }));
}

/// Changes a layer's metadata. View toggles (visibility, opacity) are not
/// recorded for undo.
pub fn layer_update(
    core: &Arc<Core>,
    file_id: FileId,
    layer_id: String,
    label: &str,
    record: bool,
    change: impl FnOnce(&mut Layer) -> Result<(), String> + Send + 'static,
) {
    let op = Op::new(label, Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
        let current = slide
            .layer(&layer_id)
            .cloned()
            .ok_or("That layer no longer exists.")?;
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
        "Rename layer",
        true,
        move |layer| {
            layer.name = clean_name(&name, "layer name", false)?;
            Ok(())
        },
    );
}

/// Deletes a layer with its contents (after the user confirmed).
pub fn layer_delete(core: &Arc<Core>, file_id: FileId, layer_id: String) {
    core.submit(Op::new("Delete layer", Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
        let layer = slide
            .layer(&layer_id)
            .cloned()
            .ok_or("That layer no longer exists.")?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let contents: Vec<Arc<Annotation>> = slide.in_layer(&layer_id).cloned().collect();
        Ok(Planned {
            label: Some(format!("Delete layer \"{}\"", layer.name)),
            changes: vec![Change::RemoveLayer { layer, contents }],
            ..Default::default()
        })
    }));
}

pub fn set_current_layer(core: &Arc<Core>, file_id: FileId, layer_id: String) {
    core.submit(
        Op::new("Choose layer", Some(file_id), move |state| {
            let slide = state.slide_of(file_id).ok_or(LOADING)?;
            let layer = slide.layer(&layer_id).ok_or("That layer no longer exists.")?;
            if layer.role != LayerRole::Normal {
                return Err("New annotations go to a regular layer; count points and HPF fields have their own.".to_string());
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
    core.submit(Op::new("Clear counts", Some(file_id), move |state| {
        let (slide, _) = editable_slide(state, file_id)?;
        let Some(layer) = slide.layer_by_role(LayerRole::Counts).cloned() else {
            return Ok(Planned::default());
        };
        let contents: Vec<Arc<Annotation>> = slide.in_layer(&layer.id).cloned().collect();
        Ok(Planned::changes(vec![Change::RemoveLayer {
            layer,
            contents,
        }]))
    }));
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
            "There are no HPF fields on this slide yet. Use the HPF field tool (H) to place them.",
        );
    }
    select(core, file_id, ids);
}

/// Repairs a self-intersecting polygon from an older version.
pub fn repair(core: &Arc<Core>, file_id: FileId, id: String) {
    core.submit(Op::new("Repair outline", Some(file_id), move |state| {
        let (slide, image) = editable_slide(state, file_id)?;
        let current = slide
            .annotations
            .get(&id)
            .cloned()
            .ok_or("That annotation no longer exists.")?;
        let normalized = geometry::normalize(current.geometry.clone(), image)?;
        let now = now_ms();
        let mut parts = normalized.parts.into_iter();
        let mut updated = (*current).clone();
        updated.geometry = parts.next().ok_or("Nothing is left of the outline.")?;
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
            notice: (n > 1).then(|| format!("The outline was split into {n} parts.")),
            ..Default::default()
        })
    }));
}

/// Moves the layers of another slide record to this slide (ANN-8).
pub fn relink(core: &Arc<Core>, file_id: FileId, from: SlideId) {
    core.db_run(move |core, ctx| {
        let layer_ids = match ctx.db.as_ref().map(|db| db.layer_ids(from)) {
            Some(Ok(ids)) => ids,
            Some(Err(e)) => {
                core.error("Could not attach the annotations", e.to_string());
                return;
            }
            None => return,
        };
        let op = Op::new(
            "Attach annotations of another slide",
            Some(file_id),
            move |state| {
                state.writable()?;
                let to = state.slide_of(file_id).ok_or(LOADING)?.id();
                if to == from || to <= 0 {
                    return Err("Those annotations already belong to this slide.".to_string());
                }
                if layer_ids.is_empty() {
                    return Err("That slide has no annotations left.".to_string());
                }
                Ok(Planned {
                    changes: vec![Change::MoveLayers {
                        from,
                        to,
                        layer_ids: layer_ids.clone(),
                    }],
                    notice: Some(format!(
                        "Attached {} layer(s) to this slide.",
                        layer_ids.len()
                    )),
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
            Err(e) => core.host.warn(format!("cannot list slides: {e}")),
        }
        core.request_refresh();
    });
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

pub fn class_new(core: &Arc<Core>) {
    core.submit(Op::new("New class", None, |state| {
        state.writable()?;
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
    label: &str,
    record: bool,
    change: impl FnOnce(&State, &mut Class) -> Result<(), String> + Send + 'static,
) {
    let op = Op::new(label, None, move |state| {
        state.writable()?;
        let current = state
            .class(&class_id)
            .cloned()
            .ok_or("That class no longer exists.")?;
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
    class_update(core, class_id, "Rename class", true, move |state, class| {
        let name = clean_name(&name, "class name", false)?;
        if state
            .class_by_name(&name)
            .is_some_and(|other| other.id != class.id)
        {
            return Err(format!("A class named \"{name}\" already exists."));
        }
        class.name = name;
        Ok(())
    });
}

pub fn class_hotkey(core: &Arc<Core>, class_id: String, key: String) {
    class_update(
        core,
        class_id,
        "Set class hotkey",
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
        "Change class colour",
        true,
        move |_, class| {
            class.color =
                Color::parse(&color).ok_or_else(|| format!("\"{color}\" is not a colour."))?;
            Ok(())
        },
    );
}

pub fn class_delete(core: &Arc<Core>, class_id: String) {
    core.submit(Op::new("Delete class", None, move |state| {
        state.writable()?;
        let current = state
            .class(&class_id)
            .cloned()
            .ok_or("That class no longer exists.")?;
        Ok(Planned {
            label: Some(format!("Delete class \"{}\"", current.name)),
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
            .map_or("Unclassified".to_string(), |c| c.name.clone());
        core.host.set_status(Some(&format!("Class: {name}")));
    }
    let _ = core.host.post(PALETTE_TOKEN);
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
// HPF reticle
// ---------------------------------------------------------------------------

/// The HPF reticle shown at the centre of each pane while the field tool is
/// active (the slide moves under it, like an eyepiece field).
pub fn reticle_ops(state: &State, file_id: FileId) -> Vec<HostOp> {
    let layer_id = reticle_layer_id(file_id);
    let Some(file) = state.files.get(&file_id) else {
        return Vec::new();
    };
    let active = state.active_tool.as_deref() == Some(tools::FIELD);
    if !active || file.view_centres.is_empty() {
        return vec![HostOp::RemoveLayer(layer_id)];
    }
    let slide = state.slide_of(file_id);
    let area = state.settings.field.area_mm2().unwrap_or(0.0);
    let objects: Vec<OverlayObject> = file
        .view_centres
        .iter()
        .filter_map(|(pane, c)| {
            let field = field_geometry(state, &file.info, c[0], c[1]).ok()?;
            let counted = slide.map_or(0, |s| crate::counting::count_in(s, &field));
            Some(
                OverlayObject::new(format!("reticle-{pane}"), field.to_shape(""))
                    .with_style(Style {
                        fill: Rgba(0xFF, 0xFF, 0xFF, 0x10),
                        stroke: Rgba(0xFF, 0xFF, 0xFF, 0xE0),
                        stroke_width: 1.5,
                        dash: Dash::Dashed,
                        ..Style::default()
                    })
                    .with_label(format!("1 HPF · {area:.4} mm² · {counted} counted")),
            )
        })
        .collect();
    if objects.is_empty() {
        return vec![HostOp::RemoveLayer(layer_id)];
    }
    let spec = LayerSpec::new(layer_id.clone(), file_id)
        .named("HPF reticle")
        .at_z(90);
    vec![HostOp::CreateLayer(spec), HostOp::Upsert(layer_id, objects)]
}

/// Refreshes the reticle of every file.
pub fn refresh_reticles(core: &Arc<Core>) {
    let ops: Vec<HostOp> = {
        let state = core.state();
        state
            .files
            .keys()
            .flat_map(|id| reticle_ops(&state, *id))
            .collect()
    };
    core.exec(ops);
}

/// Remembers a pane's view centre (from `ViewChanged` or the snapshot).
pub fn view_changed(core: &Arc<Core>, view: &ViewInfo) {
    {
        let mut state = core.state();
        for (id, file) in state.files.iter_mut() {
            if *id == view.file_id {
                file.view_centres
                    .insert(view.pane, [view.center_x, view.center_y]);
            } else {
                file.view_centres.remove(&view.pane);
            }
        }
        if state.active_tool.as_deref() != Some(tools::FIELD) {
            return;
        }
    }
    let ops = reticle_ops(&core.state(), view.file_id);
    core.exec(ops);
}

/// Annotation-kind label for lists.
pub fn kind_glyph(kind: GeometryKind) -> &'static str {
    match kind {
        GeometryKind::Point => "•",
        GeometryKind::Polyline => "╱",
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
