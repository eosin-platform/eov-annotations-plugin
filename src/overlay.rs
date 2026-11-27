//! Host overlay layers for annotations.
//!
//! Each annotation layer of a slide is one host layer per open file showing
//! it (`ann.<file id>.<layer id>`), so visibility and opacity toggles are a
//! single layer update. The host does culling, hit-testing, hover and
//! selection; the plugin only pushes what changed (ANN-12): one object per
//! edited annotation, a layer's objects when its colour changes, a class's
//! objects when the class changes.

use crate::change::Change;
use crate::model::{Annotation, Class, GeometryKind, Layer, LayerRole, SlideId, Status};
use crate::state::State;
use eov_plugin_api::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Objects per host call (keeps single messages to a few megabytes).
const CHUNK: usize = 5_000;

pub fn host_layer_id(file_id: FileId, layer_id: &str) -> String {
    format!("ann.{file_id}.{layer_id}")
}

/// `(file id, annotation layer id)` of one of our host layers.
pub fn parse_host_layer_id(id: &str) -> Option<(FileId, &str)> {
    let rest = id.strip_prefix("ann.")?;
    let (file, layer) = rest.split_once('.')?;
    Some((file.parse().ok()?, layer))
}

pub fn reticle_layer_id(file_id: FileId) -> String {
    format!("hpf.{file_id}")
}

fn z_for(layer: &Layer, index: usize) -> i32 {
    match layer.role {
        LayerRole::Fields => 5,
        LayerRole::Normal => 10 + index.min(50) as i32,
        LayerRole::Counts => 70,
    }
}

pub fn layer_spec(file_id: FileId, layer: &Layer, index: usize) -> LayerSpec {
    LayerSpec {
        layer_id: host_layer_id(file_id, &layer.id),
        file_id,
        name: layer.name.clone(),
        z: z_for(layer, index),
        visible: layer.visible,
        opacity: layer.opacity.clamp(0.0, 1.0),
        interactive: true,
        editable: !layer.locked,
        style: Style {
            stroke: layer.color.rgba(0xFF),
            ..Style::default()
        },
        selected_style: None,
        hover_style: None,
        min_zoom: None,
    }
}

fn layer_update(file_id: FileId, layer: &Layer, index: usize) -> LayerUpdate {
    let spec = layer_spec(file_id, layer, index);
    LayerUpdate {
        layer_id: spec.layer_id,
        name: Some(spec.name),
        z: Some(spec.z),
        visible: Some(spec.visible),
        opacity: Some(spec.opacity),
        interactive: Some(true),
        editable: Some(spec.editable),
        style: Some(spec.style),
        selected_style: None,
        hover_style: None,
    }
}

/// What styles objects; a snapshot so objects can be built without locks.
#[derive(Debug, Clone)]
pub struct Styler {
    pub classes: HashMap<String, Class>,
    pub point_radius: f32,
    pub fill_opacity: f32,
    pub show_labels: bool,
}

impl Styler {
    pub fn new(state: &State) -> Self {
        Self {
            classes: state
                .classes
                .iter()
                .map(|c| (c.id.clone(), c.clone()))
                .collect(),
            point_radius: state.settings.point_radius,
            fill_opacity: state.settings.fill_opacity,
            show_labels: state.settings.show_labels,
        }
    }

    /// The host object of an annotation, or `None` when its class is hidden
    /// (or its shape is unusable).
    pub fn object(&self, a: &Annotation, layer: &Layer) -> Option<OverlayObject> {
        let class = a.class_id.as_ref().and_then(|id| self.classes.get(id));
        if class.is_some_and(|c| !c.visible) {
            return None;
        }
        let shape = a.geometry.to_shape(&a.name);
        if !shape.is_valid() || a.geometry.vertex_count() > crate::model::MAX_VERTICES {
            return None;
        }
        let color = class.map_or(layer.color, |c| c.color);
        let opacity = class.map_or(1.0, |c| c.opacity).clamp(0.0, 1.0);
        let alpha = |fraction: f32| (255.0 * opacity * fraction).round().clamp(0.0, 255.0) as u8;
        let kind = a.geometry.kind();
        let mut style = Style::default();
        match kind {
            GeometryKind::Point => {
                style.fill = color.rgba(alpha(0.85));
                style.stroke = color.rgba(alpha(1.0));
                style.stroke_width = 1.5;
                style.point_radius = self.point_radius;
            }
            GeometryKind::Polyline | GeometryKind::Arrow => {
                style.fill = Rgba::TRANSPARENT;
                style.stroke = color.rgba(alpha(1.0));
                style.stroke_width = 2.0;
            }
            GeometryKind::Polygon | GeometryKind::Rectangle | GeometryKind::Ellipse => {
                let fill = if layer.role == LayerRole::Fields {
                    0.0
                } else {
                    self.fill_opacity
                };
                style.fill = color.rgba(alpha(fill));
                style.stroke = color.rgba(alpha(1.0));
                style.stroke_width = 1.5;
                if layer.role == LayerRole::Fields {
                    style.dash = Dash::Dashed;
                }
            }
            GeometryKind::Text => {
                style.fill = Rgba::TRANSPARENT;
                style.stroke = Rgba::TRANSPARENT;
                style.text_color = color.rgba(alpha(1.0).max(0x40));
                style.font_size = 14.0;
            }
        }
        match a.status {
            Status::Draft => {}
            Status::Verified => style.stroke_width += 0.5,
            Status::Rejected => {
                style.dash = Dash::Dotted;
                style.fill = style.fill.with_alpha(style.fill.3 / 2);
            }
        }
        let mut object = OverlayObject::new(a.id.clone(), shape).with_style(style);
        if self.show_labels && kind != GeometryKind::Text && !a.name.trim().is_empty() {
            object.label = Some(a.name.clone());
        }
        // The tooltip lists the class, the name and the state, each in the
        // interface's language.
        let class_name = class.map_or_else(|| tr!("Unclassified"), |c| c.name.clone());
        let mut tooltip = format!("{class_name} · {}", a.shown_name());
        if a.status != Status::Draft {
            tooltip.push_str(&format!(" · {}", a.status.label()));
        }
        if a.locked {
            // Translators: in the tooltip of a locked annotation, after its class and name.
            tooltip.push_str(&format!(" · {}", tr!("locked")));
        }
        object.tooltip = Some(tooltip);
        if a.locked {
            object.editable = Some(false);
        }
        Some(object)
    }
}

/// A host overlay call.
#[derive(Debug, Clone, PartialEq)]
pub enum HostOp {
    CreateLayer(LayerSpec),
    UpdateLayer(LayerUpdate),
    RemoveLayer(String),
    Upsert(String, Vec<OverlayObject>),
    Replace(String, Vec<OverlayObject>),
    Remove(String, Vec<String>),
    Select(String, Vec<String>),
    /// Draws objects with the layer's hover style (empty: none).
    Highlight(String, Vec<String>),
    /// Shows or hides a layer in one pane.
    PaneVisibility(String, PaneId, bool),
}

/// Performs host calls; returns the errors.
pub fn exec(host: &Host, ops: Vec<HostOp>) -> Vec<String> {
    let mut errors = Vec::new();
    let mut note = |r: HostResult<()>| {
        if let Err(e) = r {
            errors.push(e.message);
        }
    };
    for op in ops {
        match op {
            HostOp::CreateLayer(spec) => note(host.create_layer(spec).map(|_| ())),
            HostOp::UpdateLayer(update) => {
                let layer = host.layer(update.layer_id.clone());
                note(layer.update(update));
            }
            HostOp::RemoveLayer(id) => note(host.layer(id).remove_layer()),
            HostOp::Upsert(id, objects) => {
                let layer = host.layer(id);
                let mut objects = objects;
                while !objects.is_empty() {
                    let rest = objects.split_off(objects.len().min(CHUNK));
                    note(layer.upsert(objects));
                    objects = rest;
                }
            }
            HostOp::Replace(id, objects) => {
                let layer = host.layer(id);
                let mut objects = objects;
                let rest = objects.split_off(objects.len().min(CHUNK));
                note(layer.replace_all(objects));
                let mut objects = rest;
                while !objects.is_empty() {
                    let rest = objects.split_off(objects.len().min(CHUNK));
                    note(layer.upsert(objects));
                    objects = rest;
                }
            }
            HostOp::Remove(id, ids) => {
                if !ids.is_empty() {
                    note(host.layer(id).remove(ids));
                }
            }
            HostOp::Select(id, ids) => note(host.layer(id).select(ids)),
            HostOp::Highlight(id, ids) => note(host.layer(id).highlight(ids)),
            HostOp::PaneVisibility(id, pane, visible) => {
                note(host.layer(id).set_visible_in_pane(pane, visible))
            }
        }
    }
    errors
}

enum Item {
    Op(HostOp),
    Objects {
        host_layer: String,
        layer: Layer,
        annotations: Vec<Arc<Annotation>>,
        replace: bool,
    },
}

/// Host calls prepared under the state lock; [`Batch::build`] turns
/// annotations into objects afterwards, without the lock.
pub struct Batch {
    styler: Styler,
    items: Vec<Item>,
}

impl Batch {
    pub fn new(state: &State) -> Self {
        Self {
            styler: Styler::new(state),
            items: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn op(&mut self, op: HostOp) {
        self.items.push(Item::Op(op));
    }

    fn objects(
        &mut self,
        host_layer: String,
        layer: &Layer,
        annotations: Vec<Arc<Annotation>>,
        replace: bool,
    ) {
        self.items.push(Item::Objects {
            host_layer,
            layer: layer.clone(),
            annotations,
            replace,
        });
    }

    pub fn append(&mut self, other: Batch) {
        self.items.extend(other.items);
    }

    pub fn build(self) -> Vec<HostOp> {
        let styler = self.styler;
        let mut ops = Vec::with_capacity(self.items.len());
        for item in self.items {
            match item {
                Item::Op(op) => ops.push(op),
                Item::Objects {
                    host_layer,
                    layer,
                    annotations,
                    replace,
                } => {
                    let mut objects = Vec::with_capacity(annotations.len());
                    let mut hidden = Vec::new();
                    for a in &annotations {
                        match styler.object(a, &layer) {
                            Some(object) => objects.push(object),
                            None => hidden.push(a.id.clone()),
                        }
                    }
                    if replace {
                        ops.push(HostOp::Replace(host_layer, objects));
                    } else {
                        if !hidden.is_empty() {
                            ops.push(HostOp::Remove(host_layer.clone(), hidden));
                        }
                        if !objects.is_empty() {
                            ops.push(HostOp::Upsert(host_layer, objects));
                        }
                    }
                }
            }
        }
        ops
    }
}

/// Creates (or re-creates) every host layer of a file with its objects, and
/// removes host layers of layers that no longer exist.
pub fn sync_file(state: &mut State, file_id: FileId) -> Batch {
    let mut batch = Batch::new(state);
    let Some(file) = state.files.get(&file_id) else {
        return batch;
    };
    let hidden = file.hidden_panes.clone();
    let Some(slide) = file.slide_id().and_then(|id| state.slides.get(&id)) else {
        return batch;
    };
    let mut created = BTreeSet::new();
    for (index, layer) in slide.layers.iter().enumerate() {
        let spec = layer_spec(file_id, layer, index);
        created.insert(spec.layer_id.clone());
        let host_layer = spec.layer_id.clone();
        batch.op(HostOp::CreateLayer(spec));
        hide_in_panes(&mut batch, &hidden, &host_layer);
        let contents: Vec<Arc<Annotation>> = slide.in_layer(&layer.id).cloned().collect();
        batch.objects(host_layer, layer, contents, true);
    }
    let file = state.files.get_mut(&file_id).expect("checked above");
    for stale in file.host_layers.difference(&created) {
        batch.op(HostOp::RemoveLayer(stale.clone()));
    }
    file.host_layers = created;
    batch
}

/// Re-syncs every open file (after display settings changed).
pub fn sync_all(state: &mut State) -> Batch {
    let files: Vec<FileId> = state.files.keys().copied().collect();
    let mut batch = Batch::new(state);
    for file_id in files {
        batch.append(sync_file(state, file_id));
    }
    batch
}

/// Host calls for committed changes. Annotation updates are grouped per
/// host layer, so an import of many objects is a few calls, not one per
/// object.
pub fn for_changes(state: &mut State, changes: &[Change]) -> Batch {
    let mut batch = Batch::new(state);
    let mut classes_changed = Vec::new();
    // host layer → ids to remove / (layer, annotations) to upsert.
    let mut removals: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut upserts: BTreeMap<String, (Layer, Vec<Arc<Annotation>>)> = BTreeMap::new();
    for change in changes {
        match change {
            Change::Annotation { before, after } => {
                let Some(any) = after.as_ref().or(before.as_ref()) else {
                    continue;
                };
                let Some(slide_id) = state.slide_of_layer(&any.layer_id) else {
                    continue;
                };
                for file_id in state.files_of(slide_id) {
                    if let Some(before) = before
                        && after.as_ref().is_none_or(|a| a.layer_id != before.layer_id)
                    {
                        let host_layer = host_layer_id(file_id, &before.layer_id);
                        if let Some((_, list)) = upserts.get_mut(&host_layer) {
                            list.retain(|a| a.id != before.id);
                        }
                        removals
                            .entry(host_layer)
                            .or_default()
                            .push(before.id.clone());
                    }
                    if let Some(after) = after
                        && let Some(layer) = state
                            .slides
                            .get(&slide_id)
                            .and_then(|s| s.layer(&after.layer_id))
                    {
                        // The current version, in case later changes in the
                        // batch touched it again.
                        let Some(current) = state.annotation(slide_id, &after.id).cloned() else {
                            continue;
                        };
                        let host_layer = host_layer_id(file_id, &after.layer_id);
                        if let Some(ids) = removals.get_mut(&host_layer) {
                            ids.retain(|id| *id != current.id);
                        }
                        let entry = upserts
                            .entry(host_layer)
                            .or_insert_with(|| (layer.clone(), Vec::new()));
                        entry.1.retain(|a| a.id != current.id);
                        entry.1.push(current);
                    }
                }
            }
            Change::Layer { before, after } => {
                let Some(any) = after.as_ref().or(before.as_ref()) else {
                    continue;
                };
                let slide_id = any.slide_id;
                for file_id in state.files_of(slide_id) {
                    let hidden = hidden_panes(state, file_id);
                    let host_layer = host_layer_id(file_id, &any.id);
                    match after {
                        Some(after) => {
                            let index = state
                                .slides
                                .get(&slide_id)
                                .and_then(|s| s.layers.iter().position(|l| l.id == after.id))
                                .unwrap_or(0);
                            let exists = state
                                .files
                                .get(&file_id)
                                .is_some_and(|f| f.host_layers.contains(&host_layer));
                            if exists {
                                batch.op(HostOp::UpdateLayer(layer_update(file_id, after, index)));
                                if before.as_ref().is_some_and(|b| b.color != after.color) {
                                    let contents: Vec<Arc<Annotation>> = state
                                        .slides
                                        .get(&slide_id)
                                        .map(|s| s.in_layer(&after.id).cloned().collect())
                                        .unwrap_or_default();
                                    batch.objects(host_layer.clone(), after, contents, true);
                                }
                            } else {
                                batch.op(HostOp::CreateLayer(layer_spec(file_id, after, index)));
                                hide_in_panes(&mut batch, &hidden, &host_layer);
                                if let Some(f) = state.files.get_mut(&file_id) {
                                    f.host_layers.insert(host_layer.clone());
                                }
                            }
                        }
                        None => {
                            batch.op(HostOp::RemoveLayer(host_layer.clone()));
                            if let Some(f) = state.files.get_mut(&file_id) {
                                f.host_layers.remove(&host_layer);
                            }
                        }
                    }
                }
            }
            Change::RemoveLayer { layer, .. } => {
                for file_id in state.files_of(layer.slide_id) {
                    let host_layer = host_layer_id(file_id, &layer.id);
                    batch.op(HostOp::RemoveLayer(host_layer.clone()));
                    if let Some(f) = state.files.get_mut(&file_id) {
                        f.host_layers.remove(&host_layer);
                    }
                }
            }
            Change::RestoreLayer { layer, .. } => {
                restore_layer(state, &mut batch, layer.slide_id, &layer.id);
            }
            Change::MoveLayers {
                from,
                to,
                layer_ids,
            } => {
                for id in layer_ids {
                    for file_id in state.files_of(*from) {
                        let host_layer = host_layer_id(file_id, id);
                        batch.op(HostOp::RemoveLayer(host_layer.clone()));
                        if let Some(f) = state.files.get_mut(&file_id) {
                            f.host_layers.remove(&host_layer);
                        }
                    }
                    restore_layer(state, &mut batch, *to, id);
                }
            }
            Change::Class { before, after } => {
                if let Some(id) = after.as_ref().or(before.as_ref()).map(|c| c.id.clone()) {
                    classes_changed.push(id);
                }
            }
        }
    }
    // Layers removed in this batch need no object updates.
    let live = |host_layer: &String| {
        parse_host_layer_id(host_layer).is_some_and(|(file_id, _)| {
            state
                .files
                .get(&file_id)
                .is_some_and(|f| f.host_layers.contains(host_layer))
        })
    };
    for (host_layer, ids) in removals {
        if !ids.is_empty() && live(&host_layer) {
            batch.op(HostOp::Remove(host_layer, ids));
        }
    }
    for (host_layer, (layer, annotations)) in upserts {
        if !annotations.is_empty() && live(&host_layer) {
            batch.objects(host_layer, &layer, annotations, false);
        }
    }
    for class_id in classes_changed {
        batch.append(for_class(state, &class_id));
    }
    batch
}

fn restore_layer(state: &mut State, batch: &mut Batch, slide_id: SlideId, layer_id: &str) {
    let Some(slide) = state.slides.get(&slide_id) else {
        return;
    };
    let Some(index) = slide.layers.iter().position(|l| l.id == layer_id) else {
        return;
    };
    let layer = slide.layers[index].clone();
    let contents: Vec<Arc<Annotation>> = slide.in_layer(layer_id).cloned().collect();
    for file_id in state.files_of(slide_id) {
        let hidden = hidden_panes(state, file_id);
        let spec = layer_spec(file_id, &layer, index);
        let host_layer = spec.layer_id.clone();
        batch.op(HostOp::CreateLayer(spec));
        hide_in_panes(batch, &hidden, &host_layer);
        batch.objects(host_layer.clone(), &layer, contents.clone(), true);
        if let Some(f) = state.files.get_mut(&file_id) {
            f.host_layers.insert(host_layer);
        }
    }
}

/// Re-pushes every annotation of a class (its colour, visibility or
/// opacity changed), across open files.
pub fn for_class(state: &State, class_id: &str) -> Batch {
    let mut batch = Batch::new(state);
    for (file_id, file) in &state.files {
        let Some(slide) = file.slide_id().and_then(|id| state.slides.get(&id)) else {
            continue;
        };
        for layer in &slide.layers {
            let members: Vec<Arc<Annotation>> = slide
                .in_layer(&layer.id)
                .filter(|a| a.class_id.as_deref() == Some(class_id))
                .cloned()
                .collect();
            if !members.is_empty() {
                batch.objects(host_layer_id(*file_id, &layer.id), layer, members, false);
            }
        }
    }
    batch
}

/// Re-pushes the stored version of annotations (reverting an edit the host
/// showed optimistically but that could not be saved).
pub fn for_annotations(state: &State, slide_id: SlideId, ids: &[String]) -> Batch {
    let mut batch = Batch::new(state);
    let Some(slide) = state.slides.get(&slide_id) else {
        return batch;
    };
    for file_id in state.files_of(slide_id) {
        for id in ids {
            match slide.annotations.get(id) {
                Some(a) => {
                    if let Some(layer) = slide.layer(&a.layer_id) {
                        batch.objects(
                            host_layer_id(file_id, &a.layer_id),
                            layer,
                            vec![a.clone()],
                            false,
                        );
                    }
                }
                None => {
                    for layer in &slide.layers {
                        batch.op(HostOp::Remove(
                            host_layer_id(file_id, &layer.id),
                            vec![id.clone()],
                        ));
                    }
                }
            }
        }
    }
    batch
}

/// Mirrors a file's selection into its host layers.
pub fn select(state: &State, file_id: FileId) -> Vec<HostOp> {
    let Some(file) = state.files.get(&file_id) else {
        return Vec::new();
    };
    let Some(slide) = file.slide_id().and_then(|id| state.slides.get(&id)) else {
        return Vec::new();
    };
    let mut by_layer: HashMap<&str, Vec<String>> = HashMap::new();
    for id in &file.selection {
        if let Some(a) = slide.annotations.get(id) {
            by_layer
                .entry(a.layer_id.as_str())
                .or_default()
                .push(id.clone());
        }
    }
    slide
        .layers
        .iter()
        .map(|layer| {
            HostOp::Select(
                host_layer_id(file_id, &layer.id),
                by_layer.remove(layer.id.as_str()).unwrap_or_default(),
            )
        })
        .collect()
}

/// Shows or hides every annotation layer of a file in one pane (the HUD
/// button of that pane).
pub fn pane_visibility(state: &State, file_id: FileId, pane: PaneId, visible: bool) -> Vec<HostOp> {
    state
        .files
        .get(&file_id)
        .map(|file| {
            file.host_layers
                .iter()
                .map(|layer| HostOp::PaneVisibility(layer.clone(), pane, visible))
                .collect()
        })
        .unwrap_or_default()
}

fn hidden_panes(state: &State, file_id: FileId) -> BTreeSet<PaneId> {
    state
        .files
        .get(&file_id)
        .map(|f| f.hidden_panes.clone())
        .unwrap_or_default()
}

/// Creating a layer resets its per-pane visibility: hide it again where the
/// HUD button hid the file's annotations.
fn hide_in_panes(batch: &mut Batch, hidden: &BTreeSet<PaneId>, host_layer: &str) {
    for pane in hidden {
        batch.op(HostOp::PaneVisibility(host_layer.to_string(), *pane, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Color, Geometry};

    fn layer() -> Layer {
        Layer {
            id: "L".into(),
            slide_id: 1,
            name: "Layer".into(),
            notes: String::new(),
            color: Color(10, 20, 30),
            visible: true,
            opacity: 1.0,
            locked: false,
            role: LayerRole::Normal,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn annotation(class: Option<&str>) -> Annotation {
        Annotation {
            id: "a".into(),
            layer_id: "L".into(),
            class_id: class.map(str::to_string),
            geometry: Geometry::Polygon {
                exterior: vec![[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]],
                holes: vec![],
            },
            name: "Region".into(),
            description: String::new(),
            status: Status::Rejected,
            author: String::new(),
            locked: true,
            properties: vec![],
            comments: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }

    fn styler(visible: bool) -> Styler {
        let mut classes = HashMap::new();
        classes.insert(
            "c".to_string(),
            Class {
                id: "c".into(),
                name: "Tumour".into(),
                color: Color(200, 0, 0),
                hotkey: None,
                visible,
                opacity: 0.5,
                sort_order: 0,
                created_at: 0,
                updated_at: 0,
            },
        );
        Styler {
            classes,
            point_radius: 4.0,
            fill_opacity: 0.2,
            show_labels: true,
        }
    }

    #[test]
    fn host_layer_ids_round_trip() {
        let id = host_layer_id(12, "0f8e-uuid-like");
        assert_eq!(parse_host_layer_id(&id), Some((12, "0f8e-uuid-like")));
        assert_eq!(parse_host_layer_id("hpf.3"), None);
        assert_eq!(parse_host_layer_id("ann.x.y"), None);
    }

    #[test]
    fn objects_follow_class_colour_status_and_lock() {
        let object = styler(true)
            .object(&annotation(Some("c")), &layer())
            .unwrap();
        let style = object.style.unwrap();
        assert_eq!(
            style.stroke,
            Rgba(200, 0, 0, 128),
            "class colour at class opacity"
        );
        assert_eq!(style.dash, Dash::Dotted, "rejected annotations are dotted");
        assert_eq!(object.editable, Some(false));
        assert_eq!(object.label.as_deref(), Some("Region"));
        assert!(object.tooltip.unwrap().starts_with("Tumour · Region"));
        // Unclassified annotations use the layer colour.
        let plain = styler(true).object(&annotation(None), &layer()).unwrap();
        assert_eq!(plain.style.unwrap().stroke, Rgba(10, 20, 30, 255));
        // Hidden classes are not drawn.
        assert!(
            styler(false)
                .object(&annotation(Some("c")), &layer())
                .is_none()
        );
    }

    #[test]
    fn invalid_shapes_are_never_sent() {
        let mut bad = annotation(None);
        bad.geometry = Geometry::Polygon {
            exterior: vec![[0.0, 0.0], [1.0, 1.0]],
            holes: vec![],
        };
        assert!(styler(true).object(&bad, &layer()).is_none());
    }
}
