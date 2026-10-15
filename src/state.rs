//! The in-memory model: loaded slides (keyed by their database record, so
//! the same slide opened twice shares one copy, ANN-7), open files, classes,
//! settings, undo records and UI state.

use crate::change::Change;
use crate::model::{Annotation, Class, Layer, LayerRole, SlideId, SlideRecord};
use crate::stats::FieldSize;
use eov_plugin_api::protocol::{FileId, FileInfo, PaneId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

/// Undo records kept (the host keeps at most 500 entries).
pub const MAX_RECORDS: usize = 1000;

/// Persisted preferences (host settings key `settings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Recorded as the author of new annotations and in exports.
    pub author: String,
    pub organization: String,
    pub project: String,
    pub license: String,
    /// Write the slide's file name into exports (off by default: file names
    /// often contain accession numbers or patient names, ANN-17).
    pub include_filename: bool,
    pub field: FieldSize,
    /// Radius of point markers in screen pixels.
    pub point_radius: f32,
    /// Fill opacity of regions (0–1).
    pub fill_opacity: f32,
    pub show_labels: bool,
    /// The class new annotations (and plain count clicks) get.
    pub active_class: Option<String>,
    /// The class Shift+click counts with.
    pub alternate_class: Option<String>,
    pub count_hidden_layers: bool,
    /// The sidebar tab shown last.
    pub tab: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            author: String::new(),
            organization: String::new(),
            project: String::new(),
            license: String::new(),
            include_filename: false,
            field: FieldSize::default(),
            point_radius: 4.5,
            fill_opacity: 0.18,
            show_labels: true,
            active_class: None,
            alternate_class: None,
            count_hidden_layers: false,
            tab: 0,
        }
    }
}

impl Settings {
    /// Clamps values read from a possibly hand-edited settings file.
    pub fn sanitized(mut self) -> Self {
        if !(self.point_radius.is_finite() && (1.0..=20.0).contains(&self.point_radius)) {
            self.point_radius = 4.5;
        }
        if !(self.fill_opacity.is_finite() && (0.0..=1.0).contains(&self.fill_opacity)) {
            self.fill_opacity = 0.18;
        }
        if self.field.area_mm2().is_none() {
            self.field = FieldSize::default();
        }
        for text in [
            &mut self.author,
            &mut self.organization,
            &mut self.project,
            &mut self.license,
        ] {
            let cleaned: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
            *text = cleaned;
        }
        self.tab = self.tab.clamp(0, 3);
        self
    }
}

/// One loaded slide.
#[derive(Debug, Clone)]
pub struct SlideData {
    pub record: SlideRecord,
    /// Display order.
    pub layers: Vec<Layer>,
    pub annotations: HashMap<String, Arc<Annotation>>,
    /// Per layer: `(created_at, id)` in creation order.
    pub order: HashMap<String, BTreeSet<(i64, String)>>,
    /// Polygons from older versions whose outline crosses itself.
    pub invalid: HashSet<String>,
    pub current_layer: Option<String>,
    /// Increases with every change (caches key on it).
    pub revision: u64,
}

impl SlideData {
    pub fn new(
        record: SlideRecord,
        layers: Vec<Layer>,
        annotations: Vec<Arc<Annotation>>,
        invalid: HashSet<String>,
        current_layer: Option<String>,
    ) -> Self {
        let mut slide = SlideData {
            record,
            layers,
            annotations: HashMap::new(),
            order: HashMap::new(),
            invalid,
            current_layer,
            revision: 1,
        };
        for annotation in annotations {
            slide.insert(annotation);
        }
        slide.sort_layers();
        slide
    }

    pub fn id(&self) -> SlideId {
        self.record.id
    }

    pub fn sort_layers(&mut self) {
        self.layers.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
    }

    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    pub fn layer_by_role(&self, role: LayerRole) -> Option<&Layer> {
        self.layers.iter().find(|l| l.role == role)
    }

    pub fn count_in(&self, layer_id: &str) -> usize {
        self.order.get(layer_id).map_or(0, BTreeSet::len)
    }

    /// Annotations of a layer in creation order.
    pub fn in_layer<'a>(
        &'a self,
        layer_id: &str,
    ) -> impl Iterator<Item = &'a Arc<Annotation>> + 'a {
        self.order
            .get(layer_id)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|(_, id)| self.annotations.get(id))
    }

    fn insert(&mut self, annotation: Arc<Annotation>) {
        if let Some(old) = self.annotations.get(&annotation.id).cloned() {
            self.remove(&old.id);
        }
        self.order
            .entry(annotation.layer_id.clone())
            .or_default()
            .insert((annotation.created_at, annotation.id.clone()));
        self.annotations.insert(annotation.id.clone(), annotation);
    }

    fn remove(&mut self, id: &str) -> Option<Arc<Annotation>> {
        let old = self.annotations.remove(id)?;
        if let Some(set) = self.order.get_mut(&old.layer_id) {
            set.remove(&(old.created_at, old.id.clone()));
        }
        self.invalid.remove(id);
        Some(old)
    }

    /// The layer new annotations go to: the remembered one, else the first
    /// normal layer.
    pub fn target_layer(&self) -> Option<&Layer> {
        self.current_layer
            .as_deref()
            .and_then(|id| self.layer(id))
            .filter(|l| l.role == LayerRole::Normal)
            .or_else(|| self.layers.iter().find(|l| l.role == LayerRole::Normal))
    }

    /// Applies a committed change to memory.
    pub fn apply(&mut self, change: &Change) {
        match change {
            Change::Annotation { before, after } => {
                if let Some(before) = before {
                    self.remove(&before.id);
                }
                if let Some(after) = after {
                    self.insert(after.clone());
                }
            }
            Change::Layer { before, after } => match after {
                Some(after) => {
                    match self.layer_mut(&after.id) {
                        Some(existing) => *existing = after.clone(),
                        None => self.layers.push(after.clone()),
                    }
                    self.sort_layers();
                }
                None => {
                    if let Some(before) = before {
                        self.drop_layer(&before.id);
                    }
                }
            },
            Change::RemoveLayer { layer, .. } => self.drop_layer(&layer.id),
            Change::RestoreLayer { layer, contents } => {
                match self.layer_mut(&layer.id) {
                    Some(existing) => *existing = layer.clone(),
                    None => self.layers.push(layer.clone()),
                }
                for annotation in contents {
                    self.insert(annotation.clone());
                }
                self.sort_layers();
            }
            Change::Class { .. } | Change::MoveLayers { .. } => {}
        }
        self.revision += 1;
    }

    fn drop_layer(&mut self, layer_id: &str) {
        self.layers.retain(|l| l.id != layer_id);
        if let Some(ids) = self.order.remove(layer_id) {
            for (_, id) in ids {
                self.annotations.remove(&id);
                self.invalid.remove(&id);
            }
        }
        if self.current_layer.as_deref() == Some(layer_id) {
            self.current_layer = None;
        }
    }
}

/// Loading state of an open file's annotations.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    Pending,
    Loaded(SlideId),
    /// Loading failed; retried on the next activity after `retry_at`
    /// (exponential backoff, ANN-13).
    Failed {
        error: String,
        attempts: u32,
        retry_at: Instant,
    },
}

#[derive(Debug, Clone)]
pub struct OpenFile {
    pub info: FileInfo,
    pub load: Load,
    /// Selected annotation ids (host selection, mirrored).
    pub selection: Vec<String>,
    /// All annotations hidden from the HUD button.
    pub hidden: bool,
    /// Host layers created for this file.
    pub host_layers: BTreeSet<String>,
    /// The view centre of each pane showing the file (for the HPF reticle).
    pub view_centres: BTreeMap<PaneId, [f64; 2]>,
}

impl OpenFile {
    pub fn new(info: FileInfo) -> Self {
        Self {
            info,
            load: Load::Pending,
            selection: Vec::new(),
            hidden: false,
            host_layers: BTreeSet::new(),
            view_centres: BTreeMap::new(),
        }
    }

    pub fn slide_id(&self) -> Option<SlideId> {
        match self.load {
            Load::Loaded(id) => Some(id),
            _ => None,
        }
    }
}

/// The database's state.
#[derive(Debug, Clone, PartialEq)]
pub enum DbStatus {
    Opening,
    Ready { read_only: bool, path: String },
    Failed { error: String },
}

/// One undoable operation.
#[derive(Debug, Clone)]
pub struct Record {
    pub slide: Option<SlideId>,
    pub file_id: Option<FileId>,
    pub label: String,
    pub changes: Vec<Change>,
    pub undone: bool,
}

#[derive(Debug, Default)]
pub struct History {
    records: HashMap<String, Record>,
    order: VecDeque<String>,
    next: u64,
}

impl History {
    pub fn next_id(&mut self) -> String {
        self.next += 1;
        format!("u{}", self.next)
    }

    pub fn insert(&mut self, entry_id: String, record: Record) {
        self.order.push_back(entry_id.clone());
        self.records.insert(entry_id, record);
        while self.order.len() > MAX_RECORDS {
            if let Some(old) = self.order.pop_front() {
                self.records.remove(&old);
            }
        }
    }

    pub fn get(&self, entry_id: &str) -> Option<&Record> {
        self.records.get(entry_id)
    }

    pub fn get_mut(&mut self, entry_id: &str) -> Option<&mut Record> {
        self.records.get_mut(entry_id)
    }

    pub fn remove(&mut self, entry_id: &str) {
        self.records.remove(entry_id);
        self.order.retain(|id| id != entry_id);
    }

    /// Forgets the records of a slide that is no longer open.
    pub fn forget_slide(&mut self, slide: SlideId) {
        self.records.retain(|_, r| r.slide != Some(slide));
        let records = &self.records;
        self.order.retain(|id| records.contains_key(id));
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }
}

/// Everything the plugin keeps in memory.
#[derive(Debug)]
pub struct State {
    pub db: DbStatus,
    /// Notes from opening the database (migration, read-only).
    pub db_notes: Vec<String>,
    pub classes: Vec<Class>,
    pub slides: HashMap<SlideId, SlideData>,
    /// Which loaded slide a layer belongs to.
    pub layer_slide: HashMap<String, SlideId>,
    pub files: BTreeMap<FileId, OpenFile>,
    pub active_file: Option<FileId>,
    pub active_pane: Option<PaneId>,
    /// The plugin's active tool, if any.
    pub active_tool: Option<String>,
    pub settings: Settings,
    pub history: History,
    /// Hovered annotation (from the viewport or the list).
    pub hovered: Option<(FileId, String)>,
    /// Virtual ids for slides of a read-only database without a record.
    pub next_virtual_slide: SlideId,
}

impl State {
    pub fn new(settings: Settings) -> Self {
        Self {
            db: DbStatus::Opening,
            db_notes: Vec::new(),
            classes: Vec::new(),
            slides: HashMap::new(),
            layer_slide: HashMap::new(),
            files: BTreeMap::new(),
            active_file: None,
            active_pane: None,
            active_tool: None,
            settings,
            history: History::default(),
            hovered: None,
            next_virtual_slide: -1,
        }
    }

    pub fn writable(&self) -> Result<(), String> {
        match &self.db {
            DbStatus::Ready {
                read_only: false, ..
            } => Ok(()),
            DbStatus::Ready {
                read_only: true, ..
            } => Err("The annotation database is read-only.".to_string()),
            DbStatus::Opening => Err("The annotation database is still opening.".to_string()),
            DbStatus::Failed { error } => {
                Err(format!("The annotation database is unavailable: {error}"))
            }
        }
    }

    pub fn class(&self, id: &str) -> Option<&Class> {
        self.classes.iter().find(|c| c.id == id)
    }

    pub fn class_by_name(&self, name: &str) -> Option<&Class> {
        self.classes
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
    }

    pub fn sort_classes(&mut self) {
        self.classes.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }

    /// The loaded slide shown by a file.
    pub fn slide_of(&self, file_id: FileId) -> Option<&SlideData> {
        self.files
            .get(&file_id)
            .and_then(OpenFile::slide_id)
            .and_then(|id| self.slides.get(&id))
    }

    pub fn active_slide(&self) -> Option<&SlideData> {
        self.slide_of(self.active_file?)
    }

    /// Files showing a slide.
    pub fn files_of(&self, slide: SlideId) -> Vec<FileId> {
        self.files
            .iter()
            .filter(|(_, f)| f.slide_id() == Some(slide))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The slide containing an annotation layer.
    pub fn slide_of_layer(&self, layer_id: &str) -> Option<SlideId> {
        self.layer_slide.get(layer_id).copied()
    }

    /// Finds an annotation in a loaded slide.
    pub fn annotation(&self, slide: SlideId, id: &str) -> Option<&Arc<Annotation>> {
        self.slides.get(&slide)?.annotations.get(id)
    }

    /// Inserts a loaded slide and indexes its layers.
    pub fn insert_slide(&mut self, slide: SlideData) {
        for layer in &slide.layers {
            self.layer_slide.insert(layer.id.clone(), slide.id());
        }
        self.slides.insert(slide.id(), slide);
    }

    /// Drops a slide no open file shows any more (ANN-11: per-file state
    /// does not outlive the file).
    pub fn evict_unused_slides(&mut self) -> Vec<SlideId> {
        let used: HashSet<SlideId> = self.files.values().filter_map(OpenFile::slide_id).collect();
        let unused: Vec<SlideId> = self
            .slides
            .keys()
            .filter(|id| !used.contains(id))
            .copied()
            .collect();
        for id in &unused {
            self.slides.remove(id);
            self.history.forget_slide(*id);
        }
        self.layer_slide.retain(|_, slide| !unused.contains(slide));
        unused
    }

    /// Applies committed changes to memory. `moved` carries the contents of
    /// layers moved from an unloaded slide.
    pub fn apply(&mut self, changes: &[Change], moved: &[(Layer, Vec<Arc<Annotation>>)]) {
        for change in changes {
            match change {
                Change::Class { before, after } => {
                    match after {
                        Some(after) => match self.classes.iter_mut().find(|c| c.id == after.id) {
                            Some(existing) => *existing = after.clone(),
                            None => self.classes.push(after.clone()),
                        },
                        None => {
                            if let Some(before) = before {
                                self.classes.retain(|c| c.id != before.id);
                                for id in [
                                    &mut self.settings.active_class,
                                    &mut self.settings.alternate_class,
                                ] {
                                    if id.as_deref() == Some(before.id.as_str()) {
                                        *id = None;
                                    }
                                }
                            }
                        }
                    }
                    self.sort_classes();
                }
                Change::MoveLayers {
                    from,
                    to,
                    layer_ids,
                } => {
                    if let Some(source) = self.slides.get_mut(from) {
                        for id in layer_ids {
                            source.apply(&Change::RemoveLayer {
                                layer: source.layer(id).cloned().unwrap_or_else(|| Layer {
                                    id: id.clone(),
                                    ..placeholder_layer(*from)
                                }),
                                contents: Vec::new(),
                            });
                        }
                    }
                    if let Some(target) = self.slides.get_mut(to) {
                        for (layer, contents) in
                            moved.iter().filter(|(l, _)| layer_ids.contains(&l.id))
                        {
                            let mut layer = layer.clone();
                            layer.slide_id = *to;
                            target.apply(&Change::RestoreLayer {
                                layer,
                                contents: contents.clone(),
                            });
                        }
                    }
                    for id in layer_ids {
                        if self.slides.contains_key(to) {
                            self.layer_slide.insert(id.clone(), *to);
                        } else {
                            self.layer_slide.remove(id);
                        }
                    }
                }
                other => {
                    let slide = other.slide().or_else(|| match other {
                        Change::Annotation { before, after } => after
                            .as_ref()
                            .or(before.as_ref())
                            .and_then(|a| self.slide_of_layer(&a.layer_id)),
                        _ => None,
                    });
                    let Some(slide_id) = slide else {
                        continue;
                    };
                    if let Some(slide) = self.slides.get_mut(&slide_id) {
                        slide.apply(other);
                    }
                    match other {
                        Change::Layer { after: Some(l), .. }
                        | Change::RestoreLayer { layer: l, .. } => {
                            self.layer_slide.insert(l.id.clone(), slide_id);
                        }
                        Change::Layer {
                            before: Some(l),
                            after: None,
                        }
                        | Change::RemoveLayer { layer: l, .. } => {
                            self.layer_slide.remove(&l.id);
                        }
                        Change::Annotation {
                            before: Some(b),
                            after: Some(a),
                        } if b.layer_id != a.layer_id => {}
                        _ => {}
                    }
                }
            }
        }
        // Drop selections of annotations that no longer exist.
        let slides = &self.slides;
        for file in self.files.values_mut() {
            if let Some(slide) = file.slide_id().and_then(|id| slides.get(&id)) {
                file.selection
                    .retain(|id| slide.annotations.contains_key(id));
            }
        }
    }
}

fn placeholder_layer(slide_id: SlideId) -> Layer {
    Layer {
        id: String::new(),
        slide_id,
        name: String::new(),
        notes: String::new(),
        color: crate::model::Color::default(),
        visible: true,
        opacity: 1.0,
        locked: false,
        role: LayerRole::Normal,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Color, Geometry, Status};

    fn layer(id: &str) -> Layer {
        Layer {
            id: id.into(),
            slide_id: 1,
            name: id.into(),
            ..placeholder_layer(1)
        }
    }

    fn point(id: &str, layer: &str, t: i64) -> Arc<Annotation> {
        Arc::new(Annotation {
            id: id.into(),
            layer_id: layer.into(),
            class_id: None,
            geometry: Geometry::Point { x: 1.0, y: 1.0 },
            name: String::new(),
            description: String::new(),
            status: Status::Draft,
            author: String::new(),
            locked: false,
            properties: vec![],
            created_at: t,
            updated_at: t,
        })
    }

    #[test]
    fn slide_data_tracks_layers_and_order() {
        let mut slide = SlideData::new(
            SlideRecord {
                id: 1,
                ..Default::default()
            },
            vec![layer("b"), layer("a")],
            vec![point("p2", "a", 2), point("p1", "a", 1)],
            HashSet::new(),
            None,
        );
        assert_eq!(slide.layers[0].id, "a", "layers are sorted");
        let ids: Vec<_> = slide.in_layer("a").map(|a| a.id.clone()).collect();
        assert_eq!(ids, ["p1", "p2"]);
        let mut moved = (*point("p1", "b", 1)).clone();
        moved.updated_at = 3;
        slide.apply(&Change::Annotation {
            before: Some(point("p1", "a", 1)),
            after: Some(Arc::new(moved)),
        });
        assert_eq!(slide.count_in("a"), 1);
        assert_eq!(slide.count_in("b"), 1);
        slide.apply(&Change::RemoveLayer {
            layer: layer("b"),
            contents: vec![],
        });
        assert!(slide.annotations.get("p1").is_none());
        assert_eq!(slide.layers.len(), 1);
        assert!(slide.revision > 1);
    }

    #[test]
    fn history_is_bounded_and_forgets_closed_slides() {
        let mut history = History::default();
        for i in 0..(MAX_RECORDS + 10) {
            let id = history.next_id();
            history.insert(
                id,
                Record {
                    slide: Some(i as i64 % 2),
                    file_id: None,
                    label: String::new(),
                    changes: vec![],
                    undone: false,
                },
            );
        }
        assert_eq!(history.len(), MAX_RECORDS);
        history.forget_slide(0);
        assert_eq!(history.len(), MAX_RECORDS / 2);
    }

    #[test]
    fn settings_are_sanitized() {
        let s = Settings {
            point_radius: f32::NAN,
            fill_opacity: 7.0,
            author: "A\u{7}B".into(),
            tab: 99,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(s.point_radius, 4.5);
        assert_eq!(s.fill_opacity, 0.18);
        assert_eq!(s.author, "AB");
        assert_eq!(s.tab, 3);
        let _ = Color::default();
    }
}
