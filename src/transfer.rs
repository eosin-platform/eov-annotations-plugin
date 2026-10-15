//! Import, export, backup and database merge.
//!
//! Imports are bound to the slide they were started on (ANN-9): the file
//! is read, validated and normalised on the I/O thread (a pre-1.0 export
//! is checked against the slide's SHA-256 in a cancellable job, ANN-14),
//! the user decides in a dialog, and everything is applied in one
//! transaction that is one undo step (ANN-5). Exports are written
//! atomically and carry no absolute path (ANN-17).

use crate::change::Change;
use crate::core::{Core, Op, Planned};
use crate::fileio;
use crate::formats::{self, ImportClass, ImportDoc, eov, geojson};
use crate::geometry;
use crate::model::{
    Annotation, Bounds, Class, Color, Geometry, Layer, LayerRole, SlideId, Status, clean_name,
    clean_properties, clean_text, new_id, now_ms, pick_color,
};
use crate::state::{ExportFormat, Pending, State};
use eov_plugin_api::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub const IMPORT_TOKEN_PREFIX: &str = "import:";

/// How imported layers that share a name with an existing layer are
/// treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Add their annotations to the existing layer.
    Merge,
    /// Delete the existing layer (undoable) and import the new one.
    Replace,
    /// Import as new layers ("Name (2)").
    KeepBoth,
}

impl Strategy {
    pub fn from_index(index: i64) -> Self {
        match index {
            1 => Strategy::Replace,
            2 => Strategy::KeepBoth,
            _ => Strategy::Merge,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedAnnotation {
    pub geometry: Geometry,
    pub class: Option<String>,
    pub name: String,
    pub description: String,
    pub status: Status,
    pub author: String,
    pub locked: bool,
    pub properties: Vec<(String, String)>,
    pub comments: Vec<crate::model::Comment>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedLayer {
    pub name: String,
    pub color: Option<Color>,
    pub notes: String,
    pub visible: Option<bool>,
    pub annotations: Vec<PreparedAnnotation>,
}

/// A validated import waiting for confirmation.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPlan {
    pub file_id: FileId,
    pub slide_id: SlideId,
    pub source: String,
    pub format: &'static str,
    pub layers: Vec<PreparedLayer>,
    pub classes: Vec<ImportClass>,
    /// Whether the file was made for this slide, as far as can be told.
    pub match_text: String,
    pub mismatch: bool,
    /// Skipped elements, repairs, clipping, warnings.
    pub notes: Vec<String>,
    /// Imported layer names that already exist on the slide.
    pub conflicts: Vec<String>,
    /// Why nothing can be imported (e.g. an NDPA on a slide that is not an
    /// NDPI): shown in the preview instead of an Import button.
    pub blocked: Option<String>,
}

impl ImportPlan {
    /// A preview that only explains why the file cannot be imported.
    pub fn blocked(
        file_id: FileId,
        slide_id: SlideId,
        source: &str,
        format: &'static str,
        reason: String,
    ) -> Self {
        ImportPlan {
            file_id,
            slide_id,
            source: source.to_string(),
            format,
            layers: Vec::new(),
            classes: Vec::new(),
            match_text: reason.clone(),
            mismatch: true,
            notes: Vec::new(),
            conflicts: Vec::new(),
            blocked: Some(reason),
        }
    }

    pub fn annotation_count(&self) -> usize {
        self.layers.iter().map(|l| l.annotations.len()).sum()
    }
}

/// Validates and normalises a parsed document for a slide.
pub fn prepare(
    doc: ImportDoc,
    file_id: FileId,
    slide_id: SlideId,
    info: &FileInfo,
    existing_layers: &[String],
    source: &str,
) -> Result<ImportPlan, String> {
    let mut doc = doc;
    let image = (info.width > 0 && info.height > 0).then_some(Bounds {
        min_x: 0.0,
        min_y: 0.0,
        max_x: info.width as f64,
        max_y: info.height as f64,
    });
    let mut notes = doc.warnings.clone();
    let (mut repaired, mut clipped, mut truncated) = (0usize, 0usize, 0usize);
    let mut layers = Vec::new();
    for layer in std::mem::take(&mut doc.layers) {
        let name = match clean_name(&layer.name, "layer name", false) {
            Ok(name) => name,
            Err(_) if layer.name.trim().is_empty() => "Imported".to_string(),
            Err(_) => {
                truncated += 1;
                layer
                    .name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect::<String>()
                    .trim()
                    .to_string()
            }
        };
        let mut out = PreparedLayer {
            name,
            color: layer.color,
            notes: clean_text(&layer.notes, "notes").unwrap_or_else(|_| {
                truncated += 1;
                layer
                    .notes
                    .chars()
                    .take(crate::model::MAX_TEXT_CHARS)
                    .collect()
            }),
            visible: layer.visible,
            annotations: Vec::new(),
        };
        for a in layer.annotations {
            let normalized = match geometry::normalize(a.geometry, image) {
                Ok(n) => n,
                Err(reason) => {
                    doc.skip(reason.trim_end_matches('.').to_string());
                    continue;
                }
            };
            repaired += usize::from(normalized.repaired);
            clipped += usize::from(normalized.clipped);
            let name = clean_name(&a.name, "name", true).unwrap_or_else(|_| {
                truncated += 1;
                a.name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect()
            });
            let description = clean_text(&a.description, "description").unwrap_or_else(|_| {
                truncated += 1;
                a.description
                    .chars()
                    .take(crate::model::MAX_TEXT_CHARS)
                    .collect()
            });
            let properties = clean_properties(&a.properties).unwrap_or_else(|_| {
                truncated += 1;
                let limited: Vec<(String, String)> = a
                    .properties
                    .iter()
                    .take(crate::model::MAX_PROPERTIES)
                    .map(|(k, v)| {
                        (
                            k.chars()
                                .take(crate::model::MAX_PROPERTY_KEY_CHARS)
                                .collect(),
                            v.chars()
                                .take(crate::model::MAX_PROPERTY_VALUE_CHARS)
                                .collect(),
                        )
                    })
                    .collect();
                clean_properties(&limited).unwrap_or_default()
            });
            let class = a
                .class
                .as_deref()
                .and_then(|c| clean_name(c, "class", false).ok());
            let author = clean_name(&a.author, "author", true).unwrap_or_default();
            for part in normalized.parts {
                out.annotations.push(PreparedAnnotation {
                    geometry: part,
                    class: class.clone(),
                    name: name.clone(),
                    description: description.clone(),
                    status: a.status,
                    author: author.clone(),
                    locked: a.locked,
                    properties: properties.clone(),
                    comments: clean_comments(&a.comments),
                    created_at: a.created_at,
                    updated_at: a.updated_at,
                });
            }
        }
        if !out.annotations.is_empty() {
            layers.push(out);
        }
    }
    if let Some(skipped) = doc.skipped_summary() {
        notes.push(format!("Skipped: {skipped}."));
    }
    if repaired > 0 {
        notes.push(format!(
            "{repaired} outline(s) crossed themselves and were repaired with the even-odd rule."
        ));
    }
    if clipped > 0 {
        notes.push(format!(
            "{clipped} shape(s) extended past the slide and were cut at its edge."
        ));
    }
    if truncated > 0 {
        notes.push(format!(
            "{truncated} over-long name(s) or text(s) were shortened."
        ));
    }
    if layers.is_empty() {
        return Err(match notes.is_empty() {
            true => "The file contains no annotations.".to_string(),
            false => format!("Nothing could be imported. {}", notes.join(" ")),
        });
    }
    let (match_text, mismatch) = slide_match(&doc, info);
    let conflicts = layers
        .iter()
        .filter(|l| {
            existing_layers
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&l.name))
        })
        .map(|l| l.name.clone())
        .collect();
    let classes = doc
        .classes
        .into_iter()
        .filter(|c| clean_name(&c.name, "class", false).is_ok())
        .collect();
    Ok(ImportPlan {
        file_id,
        slide_id,
        source: source.to_string(),
        format: doc.format,
        layers,
        classes,
        match_text,
        mismatch,
        notes,
        conflicts,
        blocked: None,
    })
}

/// Imported comments within the limits (overlong texts shortened).
fn clean_comments(comments: &[crate::model::Comment]) -> Vec<crate::model::Comment> {
    comments
        .iter()
        .take(crate::model::MAX_COMMENTS)
        .filter_map(|c| {
            let text: String = c
                .text
                .chars()
                .filter(|ch| !ch.is_control() || *ch == '\n')
                .take(crate::model::MAX_TEXT_CHARS)
                .collect();
            (!text.trim().is_empty()).then(|| crate::model::Comment {
                id: c.id.chars().take(64).collect(),
                author: c
                    .author
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(200)
                    .collect(),
                text,
                created_at: c.created_at,
            })
        })
        .collect()
}

/// Compares what the file says about its slide with the open slide.
fn slide_match(doc: &ImportDoc, info: &FileInfo) -> (String, bool) {
    let hint = &doc.slide;
    if let (Some(theirs), Some(ours)) = (&hint.identity, &info.identity) {
        return if theirs == ours {
            ("Made for this slide.".to_string(), false)
        } else {
            (
                "Made for a different slide (its identity does not match).".to_string(),
                true,
            )
        };
    }
    if let (Some(w), Some(h)) = (hint.width, hint.height)
        && (w != info.width || h != info.height)
    {
        return (
            format!(
                "Made for a slide of {w} × {h} px; this one is {} × {} px.",
                info.width, info.height
            ),
            true,
        );
    }
    if hint.width.is_some() {
        return ("Made for a slide of the same size.".to_string(), false);
    }
    ("The file does not say which slide it belongs to; coordinates are taken as level-0 pixels of this slide.".to_string(), false)
}

/// Starts an import: the file dialog.
pub fn start_import(core: &Arc<Core>, file_id: FileId) {
    let args = FileDialogArgs {
        title: Some("Import annotations".into()),
        filters: vec![
            FileFilter {
                name: "Annotations (GeoJSON, eov JSON, ASAP / ImageScope XML, NDPA)".into(),
                extensions: vec!["geojson".into(), "json".into(), "xml".into(), "ndpa".into()],
            },
            FileFilter {
                name: "All files".into(),
                extensions: vec!["*".into()],
            },
        ],
        ..Default::default()
    };
    match core.host.open_file_dialog(args) {
        Ok(request) => {
            core.state()
                .pending
                .insert(request, Pending::Import { file_id });
        }
        Err(e) => core.error("Cannot show the file dialog", e.message),
    }
}

/// The user picked a file to import.
pub fn import_file(core: &Arc<Core>, file_id: FileId, path: PathBuf) {
    let prepared = {
        let state = core.state();
        let Some(file) = state.files.get(&file_id) else {
            return;
        };
        let Some(slide) = state.slide_of(file_id) else {
            drop(state);
            core.notify(
                NotifyLevel::Warning,
                "The annotations of this slide are still loading.",
            );
            return;
        };
        (
            file.info.clone(),
            slide.id(),
            slide
                .layers
                .iter()
                .map(|l| l.name.clone())
                .collect::<Vec<_>>(),
        )
    };
    let (info, slide_id, existing) = prepared;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let job = core.host.start_job(format!("Importing {name}"), true).ok();
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(job) = &job {
        core.state()
            .jobs
            .insert(job.id().to_string(), cancel.clone());
    }
    core.io(move |core| {
        let result = (|| -> Result<Option<ImportPlan>, String> {
            let bytes = fileio::read_limited(&path)?;
            if let Some(job) = &job {
                job.update(None, Some("Reading"));
            }
            let doc = formats::parse(&bytes, &name)?;
            let mut doc = if doc.entries.is_empty() {
                doc
            } else {
                pick_archive_entry(doc, &info)?
            };
            let ndpa = doc.units == formats::Units::NdpaNanometres;
            if ndpa {
                // NDPA coordinates are nanometres from the slide centre: they
                // need the slide's NDPI offsets, or the preview says why not.
                let frame = core
                    .host
                    .file_properties(file_id)
                    .map_err(|e| e.message)
                    .and_then(|props| formats::ndpa::Frame::of_slide(&props, &info));
                match frame {
                    Ok(frame) => formats::ndpa::place(&mut doc, &frame),
                    Err(reason) => {
                        return Ok(Some(ImportPlan::blocked(file_id, slide_id, &name, doc.format, reason)));
                    }
                }
            }
            let legacy_sha = doc.slide.legacy_sha256.clone();
            let mut plan = prepare(doc, file_id, slide_id, &info, &existing, &name)?;
            if ndpa {
                plan.match_text =
                    "NDP.view annotations, placed with this slide's offsets from the slide centre and its pixel size."
                        .to_string();
                plan.mismatch = false;
            }
            if let Some(expected) = legacy_sha {
                // Exports of the pre-1.0 plugin carry the slide's SHA-256.
                let digest = fileio::sha256_with_progress(Path::new(&info.path), &cancel, |f| {
                    if let Some(job) = &job {
                        job.update(Some(f), Some("Checking the slide"));
                    }
                })
                .map_err(|e| format!("cannot read the slide to check the import: {e}"))?;
                let Some(digest) = digest else {
                    return Ok(None);
                };
                if fileio::hex(&digest).eq_ignore_ascii_case(expected.trim()) {
                    plan.match_text = "Made for this slide (checked by content).".to_string();
                    plan.mismatch = false;
                } else {
                    plan.match_text =
                        "Made for a different slide (its checksum does not match).".to_string();
                    plan.mismatch = true;
                }
            }
            Ok(Some(plan))
        })();
        if let Some(job) = job {
            core.state().jobs.remove(job.id());
            match &result {
                Err(e) => job.finish(Some(e.clone())),
                Ok(_) => job.finish(None),
            }
        }
        match result {
            Ok(Some(plan)) => {
                let id = {
                    let mut state = core.state();
                    let id = state.next_import;
                    state.next_import += 1;
                    state.imports.insert(id, plan);
                    id
                };
                let _ = core.host.post(format!("{IMPORT_TOKEN_PREFIX}{id}"));
            }
            Ok(None) => core.notify(NotifyLevel::Info, "Import cancelled."),
            Err(e) => core.error(format!("Could not import {name}"), e),
        }
    });
}

fn pick_archive_entry(doc: ImportDoc, info: &FileInfo) -> Result<ImportDoc, String> {
    let classes = doc.classes.clone();
    let mut entries = doc.entries;
    let index = match &info.identity {
        Some(identity) => entries
            .iter()
            .position(|e| e.slide.identity.as_deref() == Some(identity.as_str())),
        None => None,
    };
    let index = index.or_else(|| (entries.len() == 1).then_some(0)).ok_or_else(|| {
        "This archive has no annotations for the current slide. Restore whole backups with \"Merge database\"."
            .to_string()
    })?;
    let mut entry = entries.swap_remove(index);
    for class in classes {
        entry.note_class(&class.name, class.color);
    }
    Ok(entry)
}

/// Applies a confirmed import in one transaction (one undo step).
pub fn apply_import(core: &Arc<Core>, plan: ImportPlan, strategy: Strategy) {
    let file_id = plan.file_id;
    let label = format!("Import {} annotation(s)", plan.annotation_count());
    core.submit(Op::new(label, Some(file_id), move |state| {
        state.writable()?;
        let slide = state
            .slide_of(file_id)
            .filter(|s| s.id() == plan.slide_id)
            .ok_or("The slide this import was started on is not open any more.")?;
        let now = now_ms();
        let mut changes = Vec::new();
        // Classes by name: existing ones are reused, missing ones created.
        let mut class_ids: Vec<(String, String)> = Vec::new();
        let mut used_colors: Vec<Color> = state.classes.iter().map(|c| c.color).collect();
        let mut sort = state
            .classes
            .iter()
            .map(|c| c.sort_order)
            .max()
            .unwrap_or(0);
        let needed: Vec<&String> = plan
            .layers
            .iter()
            .flat_map(|l| l.annotations.iter().filter_map(|a| a.class.as_ref()))
            .collect();
        for name in needed {
            if class_ids.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
                continue;
            }
            let id = match state.class_by_name(name) {
                Some(existing) => existing.id.clone(),
                None => {
                    let color = plan
                        .classes
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(name))
                        .and_then(|c| c.color)
                        .unwrap_or_else(|| pick_color(used_colors.iter()));
                    used_colors.push(color);
                    sort += 1;
                    let class = Class {
                        id: new_id(),
                        name: name.clone(),
                        color,
                        hotkey: None,
                        visible: true,
                        opacity: 1.0,
                        sort_order: sort,
                        created_at: now,
                        updated_at: now,
                    };
                    let id = class.id.clone();
                    changes.push(Change::Class {
                        before: None,
                        after: Some(class),
                    });
                    id
                }
            };
            class_ids.push((name.clone(), id));
        }
        let mut layer_names: Vec<String> = slide.layers.iter().map(|l| l.name.clone()).collect();
        let mut layer_colors: Vec<Color> = slide.layers.iter().map(|l| l.color).collect();
        let mut sort_order = slide.layers.iter().map(|l| l.sort_order).max().unwrap_or(0);
        let mut total = 0usize;
        for layer in &plan.layers {
            let existing = slide
                .layers
                .iter()
                .find(|l| l.role == LayerRole::Normal && l.name.eq_ignore_ascii_case(&layer.name))
                .cloned();
            let target = match (strategy, existing) {
                (Strategy::Merge, Some(existing)) => {
                    if existing.locked {
                        return Err(format!("Layer \"{}\" is locked.", existing.name));
                    }
                    existing
                }
                (strategy, existing) => {
                    if let (Strategy::Replace, Some(existing)) = (strategy, &existing) {
                        if existing.locked {
                            return Err(format!("Layer \"{}\" is locked.", existing.name));
                        }
                        let contents: Vec<Arc<Annotation>> =
                            slide.in_layer(&existing.id).cloned().collect();
                        changes.push(Change::RemoveLayer {
                            layer: existing.clone(),
                            contents,
                        });
                    }
                    let mut name = layer.name.clone();
                    if strategy == Strategy::KeepBoth {
                        let mut n = 2;
                        while layer_names.iter().any(|l| l.eq_ignore_ascii_case(&name)) {
                            name = format!("{} ({n})", layer.name);
                            n += 1;
                        }
                    }
                    let color = layer
                        .color
                        .unwrap_or_else(|| pick_color(layer_colors.iter()));
                    layer_colors.push(color);
                    layer_names.push(name.clone());
                    sort_order += 1;
                    let new = Layer {
                        id: new_id(),
                        slide_id: slide.id(),
                        name,
                        notes: layer.notes.clone(),
                        color,
                        visible: layer.visible.unwrap_or(true),
                        opacity: 1.0,
                        locked: false,
                        role: LayerRole::Normal,
                        sort_order,
                        created_at: now,
                        updated_at: now,
                    };
                    changes.push(Change::Layer {
                        before: None,
                        after: Some(new.clone()),
                    });
                    new
                }
            };
            for (i, a) in layer.annotations.iter().enumerate() {
                let class_id = a.class.as_ref().and_then(|name| {
                    class_ids
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(name))
                        .map(|(_, id)| id.clone())
                });
                // Keep creation order stable for lists.
                let created = a.created_at.unwrap_or(now + i as i64 % 1000);
                changes.push(Change::Annotation {
                    before: None,
                    after: Some(Arc::new(Annotation {
                        id: new_id(),
                        layer_id: target.id.clone(),
                        class_id,
                        geometry: a.geometry.clone(),
                        name: a.name.clone(),
                        description: a.description.clone(),
                        status: a.status,
                        author: a.author.clone(),
                        locked: a.locked,
                        properties: a.properties.clone(),
                        comments: a.comments.clone(),
                        created_at: created,
                        updated_at: a.updated_at.unwrap_or(created).max(created),
                    })),
                });
                total += 1;
            }
        }
        Ok(Planned {
            changes,
            notice: Some(format!(
                "Imported {total} annotation(s) from {}.",
                plan.source
            )),
            ..Default::default()
        })
    }));
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn stem(filename: &str) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "slide".to_string());
    if stem.is_empty() {
        "slide".to_string()
    } else {
        stem
    }
}

pub fn start_export(core: &Arc<Core>, file_id: FileId, format: ExportFormat) {
    let Some(filename) = core
        .state()
        .files
        .get(&file_id)
        .map(|f| f.info.filename.clone())
    else {
        return;
    };
    let (default_name, filter) = match format {
        ExportFormat::GeoJson => (
            format!("{}.geojson", stem(&filename)),
            FileFilter {
                name: "GeoJSON".into(),
                extensions: vec!["geojson".into(), "json".into()],
            },
        ),
        ExportFormat::Eov => (
            format!("{}.annotations.json", stem(&filename)),
            FileFilter {
                name: "eov annotations".into(),
                extensions: vec!["json".into()],
            },
        ),
        ExportFormat::Csv => (
            format!("{}.measurements.csv", stem(&filename)),
            FileFilter {
                name: "CSV".into(),
                extensions: vec!["csv".into()],
            },
        ),
        ExportFormat::Asap => (
            format!("{}.asap.xml", stem(&filename)),
            FileFilter {
                name: "ASAP XML".into(),
                extensions: vec!["xml".into()],
            },
        ),
        ExportFormat::ImageScope => (
            format!("{}.xml", stem(&filename)),
            FileFilter {
                name: "Aperio ImageScope XML".into(),
                extensions: vec!["xml".into()],
            },
        ),
    };
    save_dialog(
        core,
        "Export annotations",
        &default_name,
        filter,
        Pending::Export { file_id, format },
    );
}

pub fn save_dialog(
    core: &Arc<Core>,
    title: &str,
    default_name: &str,
    filter: FileFilter,
    pending: Pending,
) {
    let args = FileDialogArgs {
        title: Some(title.to_string()),
        filters: vec![filter],
        default_name: Some(default_name.to_string()),
        ..Default::default()
    };
    match core.host.save_file_dialog(args) {
        Ok(request) => {
            core.state().pending.insert(request, pending);
        }
        Err(e) => core.error("Cannot show the file dialog", e.message),
    }
}

pub fn export_options(core: &Core, state: &State) -> eov::ExportOptions {
    eov::ExportOptions {
        include_filename: state.settings.include_filename,
        author: state.settings.author.clone(),
        organization: state.settings.organization.clone(),
        project: state.settings.project.clone(),
        license: state.settings.license.clone(),
        generator: format!("eov annotations plugin {}", core.plugin_version),
        exported_at: now_ms(),
    }
}

/// Writes the export chosen in the save dialog.
pub fn export_file(core: &Arc<Core>, file_id: FileId, format: ExportFormat, path: PathBuf) {
    let quickhash = core
        .host
        .file_properties(file_id)
        .ok()
        .and_then(|p| p.get("openslide.quickhash-1").cloned());
    let prepared = {
        let state = core.state();
        let Some(file) = state.files.get(&file_id) else {
            return;
        };
        let Some(slide) = state.slide_of(file_id) else {
            return;
        };
        let layers: Vec<(Layer, Vec<Arc<Annotation>>)> = slide
            .layers
            .iter()
            .map(|l| (l.clone(), slide.in_layer(&l.id).cloned().collect()))
            .collect();
        let mut record = slide.record.clone();
        record.filename = file.info.filename.clone();
        (
            eov::ExportSlide {
                record,
                quickhash1: quickhash,
                bounds: file.info.bounds,
                layers,
            },
            state.classes.clone(),
            export_options(core, &state),
        )
    };
    let (slide, classes, options) = prepared;
    core.io(move |core| {
        let count: usize = slide.layers.iter().map(|(_, a)| a.len()).sum();
        let bytes = match format {
            ExportFormat::GeoJson => {
                serde_json::to_vec_pretty(&geojson::export(&slide, &classes, &options))
            }
            ExportFormat::Eov => {
                serde_json::to_vec_pretty(&eov::export(&slide, &classes, &options))
            }
            ExportFormat::Csv => Ok(formats::csv::export(&slide, &classes).into_bytes()),
            ExportFormat::Asap => Ok(formats::xml::export_asap(&slide, &classes).into_bytes()),
            ExportFormat::ImageScope => {
                Ok(formats::xml::export_imagescope(&slide, &classes).into_bytes())
            }
        };
        let result = bytes
            .map_err(|e| e.to_string())
            .and_then(|bytes| fileio::write_atomically(&path, &bytes).map_err(|e| e.to_string()));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match result {
            Ok(()) => core.notify(
                NotifyLevel::Success,
                format!("Exported {count} annotation(s) to {name}."),
            ),
            Err(e) => core.error(format!("Could not export to {name}"), e),
        }
    });
}

/// Every slide in the database, as one archive.
pub fn export_all(core: &Arc<Core>, path: PathBuf) {
    let job = core.host.start_job("Exporting all annotations", false).ok();
    core.db_run(move |core, ctx| {
        let Some(db) = ctx.db.as_mut() else {
            return;
        };
        let slides = match db.all_slides() {
            Ok(slides) => slides,
            Err(e) => {
                if let Some(job) = job {
                    job.finish(Some(e.to_string()));
                }
                return;
            }
        };
        let (classes, options) = {
            let state = core.state();
            (state.classes.clone(), export_options(core, &state))
        };
        let exports: Vec<eov::ExportSlide> = slides
            .into_iter()
            .map(|(record, loaded)| {
                let mut by_layer: std::collections::HashMap<String, Vec<Arc<Annotation>>> =
                    Default::default();
                for a in loaded.annotations {
                    by_layer.entry(a.layer_id.clone()).or_default().push(a);
                }
                eov::ExportSlide {
                    layers: loaded
                        .layers
                        .into_iter()
                        .map(|l| {
                            let list = by_layer.remove(&l.id).unwrap_or_default();
                            (l, list)
                        })
                        .collect(),
                    record,
                    quickhash1: None,
                    bounds: None,
                }
            })
            .collect();
        core.io(move |core| {
            let slides = exports.len();
            let result = serde_json::to_vec(&eov::archive(&exports, &classes, &options))
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    fileio::write_atomically(&path, &bytes).map_err(|e| e.to_string())
                });
            match result {
                Ok(()) => {
                    core.notify(
                        NotifyLevel::Success,
                        format!("Exported the annotations of {slides} slide(s)."),
                    );
                    if let Some(job) = job {
                        job.finish(None);
                    }
                }
                Err(e) => {
                    core.error("Could not export all annotations", e.clone());
                    if let Some(job) = job {
                        job.finish(Some(e));
                    }
                }
            }
        });
    });
}

/// A consistent copy of the database.
pub fn backup(core: &Arc<Core>, path: PathBuf) {
    core.db_run(move |core, ctx| {
        let Some(db) = ctx.db.as_ref() else {
            return;
        };
        match db.backup_to(&path) {
            Ok(()) => core.notify(NotifyLevel::Success, "Annotation database backed up."),
            Err(e) => core.error("Could not back up the annotation database", e.to_string()),
        }
    });
}

/// Merges another database (a backup or a pre-1.0 `annotations.db`).
/// Merges another annotation database into the open one (`legacy`: the
/// pre-1.0 database the host's legacy folder holds, which is then not
/// offered again).
pub fn merge_database(core: &Arc<Core>, path: PathBuf, legacy: bool) {
    let job = core
        .host
        .start_job("Merging annotation database", false)
        .ok();
    core.db_run(move |core, ctx| {
        let scratch = core.paths.cache_dir.join("db");
        let Some(db) = ctx.db.as_mut() else {
            return;
        };
        let result = db.merge_from(&path, &scratch);
        match &result {
            Ok((slides, layers, annotations)) => {
                core.notify(
                    NotifyLevel::Success,
                    format!("Merged {annotations} annotation(s) in {layers} layer(s) ({slides} new slide(s))."),
                );
                if legacy {
                    if let Err(e) = db.set_setting(crate::core::LEGACY_SETTING, "merged") {
                        core.host.warn(format!("cannot record the merge: {e}"));
                    }
                    core.state().legacy_db = None;
                }
            }
            Err(e) => core.error("Could not merge the database", e.to_string()),
        }
        if let Some(job) = job {
            job.finish(result.as_ref().err().map(|e| e.to_string()));
        }
        crate::core::reload_all(core, ctx);
        core.state().orphans = None;
    });
}

/// Remembers that the user did not want the pre-1.0 database merged now
/// (the Data tab still offers it).
pub fn decline_legacy(core: &Arc<Core>) {
    core.db_run(|core, ctx| {
        if let Some(db) = ctx.db.as_mut()
            && let Err(e) = db.set_setting(crate::core::LEGACY_SETTING, "declined")
        {
            core.host.warn(format!("cannot record the answer: {e}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::{ImportAnnotation, ImportLayer, SlideHint};

    fn info() -> FileInfo {
        FileInfo {
            file_id: 1,
            width: 1000,
            height: 1000,
            identity: Some("eov-slide-1:me".into()),
            ..Default::default()
        }
    }

    fn doc(geometries: Vec<Geometry>) -> ImportDoc {
        ImportDoc {
            format: "test",
            layers: vec![ImportLayer {
                name: "Imported".into(),
                annotations: geometries.into_iter().map(ImportAnnotation::new).collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn imports_are_validated_before_anything_is_written() {
        let d = doc(vec![
            Geometry::Point { x: 10.0, y: 10.0 },
            Geometry::Point { x: -10.0, y: 10.0 },
            Geometry::Polygon {
                exterior: vec![[0.0, 0.0], [1.0, 1.0]],
                holes: vec![],
            },
            Geometry::Polygon {
                exterior: vec![[0.0, 0.0], [100.0, 100.0], [100.0, 0.0], [0.0, 100.0]],
                holes: vec![],
            },
        ]);
        let plan = prepare(d, 1, 7, &info(), &["imported".to_string()], "x.geojson").unwrap();
        // The bowtie becomes two parts; two invalid shapes are skipped.
        assert_eq!(plan.annotation_count(), 3);
        assert!(
            plan.notes.iter().any(|n| n.contains("Skipped")),
            "{:?}",
            plan.notes
        );
        assert!(plan.notes.iter().any(|n| n.contains("repaired")));
        assert_eq!(plan.conflicts, vec!["Imported".to_string()]);
        assert!(!plan.mismatch);
    }

    #[test]
    fn slide_mismatches_are_reported() {
        let mut d = doc(vec![Geometry::Point { x: 1.0, y: 1.0 }]);
        d.slide = SlideHint {
            identity: Some("eov-slide-1:other".into()),
            ..Default::default()
        };
        let plan = prepare(d, 1, 7, &info(), &[], "x").unwrap();
        assert!(plan.mismatch);
        let mut d = doc(vec![Geometry::Point { x: 1.0, y: 1.0 }]);
        d.slide.width = Some(5);
        d.slide.height = Some(5);
        assert!(prepare(d, 1, 7, &info(), &[], "x").unwrap().mismatch);
    }

    #[test]
    fn nothing_usable_is_an_error_with_reasons() {
        let d = doc(vec![Geometry::Point { x: -1.0, y: -1.0 }]);
        let error = prepare(d, 1, 7, &info(), &[], "x").unwrap_err();
        assert!(error.contains("outside the slide"), "{error}");
    }

    #[test]
    fn over_long_names_are_shortened_not_fatal() {
        let mut d = doc(vec![Geometry::Point { x: 1.0, y: 1.0 }]);
        d.layers[0].name = "x".repeat(500);
        d.layers[0].annotations[0].name = "y".repeat(500);
        let plan = prepare(d, 1, 7, &info(), &[], "x").unwrap();
        assert_eq!(plan.layers[0].name.chars().count(), 200);
        assert_eq!(plan.layers[0].annotations[0].name.chars().count(), 200);
        assert!(plan.notes.iter().any(|n| n.contains("shortened")));
    }
}
