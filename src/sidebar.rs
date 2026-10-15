//! The sidebar's view model (properties pushed to `ui/annotations-sidebar.slint`)
//! and its callbacks.
//!
//! - Properties are plain data; the host applies only changed values and
//!   updates list models row by row, so rows (and fields being edited)
//!   survive refreshes (ANN-10).
//! - The annotation list is bounded: big layers start collapsed and at most
//!   a few hundred rows are sent, with "show more" and a filter (ANN-12).
//! - Hover and selection follow the viewport through host events and are
//!   pushed back with layer selection; no magic property names.

use crate::actions;
use crate::core::Core;
use crate::counting;
use crate::measure::{self, Calibration};
use crate::model::{Class, Color, LayerRole, Status, clean_name, clean_properties, clean_text};
use crate::state::{DbStatus, ExportFormat, Load, Pending, State};
use crate::stats::FieldSize;
use crate::transfer;
use eov_plugin_api::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

pub const SIDEBAR_UI: &str = "ui/annotations-sidebar.slint";
pub const SIDEBAR_COMPONENT: &str = "AnnotationsSidebar";
pub const IMPORT_UI: &str = "ui/import-dialog.slint";
pub const IMPORT_COMPONENT: &str = "ImportDialog";

/// Every callback of the sidebar surface with its arguments (`s` string,
/// `n` number, `b` bool); `tools/ui-check` checks the `.slint` file
/// declares exactly these.
pub const CALLBACKS: &[(&str, &str)] = &[
    ("tab-changed", "n"),
    ("retry", ""),
    ("menu", "s"),
    ("tool", "s"),
    ("filter-edited", "s"),
    ("show-more", ""),
    ("row-clicked", "ssb"),
    ("row-double-clicked", "ss"),
    ("row-hovered", "s"),
    ("toggle-collapsed", "s"),
    ("layer-new", ""),
    ("layer-visible", "sb"),
    ("layer-locked", "sb"),
    ("layer-opacity", "sn"),
    ("layer-color", "ss"),
    ("layer-rename", "ss"),
    ("layer-notes", "ss"),
    ("layer-current", "s"),
    ("layer-delete", "s"),
    ("detail-name-committed", "ss"),
    ("detail-description-committed", "ss"),
    ("detail-class-chosen", "n"),
    ("detail-status-chosen", "n"),
    ("detail-layer-chosen", "n"),
    ("detail-locked-toggled", "b"),
    ("detail-delete", ""),
    ("detail-frame", ""),
    ("detail-repair", ""),
    ("copy-measurements", ""),
    ("property-add", "s"),
    ("property-remove", "sn"),
    ("property-committed", "snss"),
    ("class-new", ""),
    ("class-activate", "s"),
    ("class-alternate", "s"),
    ("class-rename", "ss"),
    ("class-color", "ss"),
    ("class-hotkey", "ss"),
    ("class-visible", "sb"),
    ("class-opacity", "sn"),
    ("class-delete", "s"),
    ("count-active", "n"),
    ("count-alternate", "n"),
    ("count-hidden-toggled", "b"),
    ("count-clear", ""),
    ("fields-select", ""),
    ("field-config", "nsss"),
    ("orphans-refresh", ""),
    ("legacy-merge", ""),
    ("orphan-relink", "s"),
    ("setting-text", "ss"),
    ("setting-bool", "sb"),
    ("setting-number", "sn"),
];

/// The import dialog's callbacks (0 merge, 1 replace, 2 keep both).
pub const IMPORT_CALLBACKS: &[(&str, &str)] = &[("import-confirm", "n"), ("import-cancel", "")];

/// Rows sent per "page" of the annotation list.
pub const ROW_STEP: usize = 300;
/// Layers with more annotations start collapsed.
pub const AUTO_COLLAPSE: usize = 200;

pub const TAB_LAYERS: i32 = 0;
pub const TAB_CLASSES: i32 = 1;
pub const TAB_COUNT: i32 = 2;
pub const TAB_DATA: i32 = 3;

/// Sidebar state that lives only in the UI.
#[derive(Debug, Clone)]
pub struct SidebarView {
    pub filter: String,
    /// Layers whose collapsed state the user toggled (relative to the
    /// automatic default).
    pub toggled: HashSet<String>,
    pub limit: usize,
}

impl Default for SidebarView {
    fn default() -> Self {
        Self {
            filter: String::new(),
            toggled: HashSet::new(),
            limit: ROW_STEP,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TreeRow {
    pub id: String,
    pub layer_id: String,
    /// 0 layer, 1 annotation, 2 "show more".
    pub kind: i32,
    pub label: String,
    pub detail: String,
    pub color: String,
    pub icon: String,
    pub indent: i32,
    pub selected: bool,
    pub hovered: bool,
    pub visible: bool,
    pub locked: bool,
    pub collapsed: bool,
    pub current: bool,
    pub status: i32,
    /// Layer rows only.
    pub opacity: f32,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ClassRow {
    pub id: String,
    pub name: String,
    pub color: String,
    pub hotkey: String,
    pub visible: bool,
    pub opacity: f32,
    pub count: i32,
    pub active: bool,
    pub alternate: bool,
    /// Why eov refused the hotkey (empty when it is in use).
    pub hotkey_note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TallyRow {
    pub name: String,
    pub color: String,
    pub count: String,
    pub density: String,
    pub per_hpf: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct OrphanRow {
    pub id: String,
    pub title: String,
    pub detail: String,
}

fn color_of(state: &State, class: Option<&str>, fallback: Color) -> Color {
    class
        .and_then(|id| state.class(id))
        .map_or(fallback, |c| c.color)
}

fn put(map: &mut Map<String, Value>, key: &str, value: impl Serialize) {
    map.insert(
        key.to_string(),
        serde_json::to_value(value).unwrap_or(Value::Null),
    );
}

fn date(ms: i64) -> String {
    let text = crate::formats::iso8601(ms);
    // 2026-10-15T09:30:00Z → 2026-10-15 09:30 UTC
    format!("{} {} UTC", &text[..10], &text[11..16])
}

/// Every sidebar property.
pub fn properties(state: &State, view: &SidebarView) -> Map<String, Value> {
    let mut p = Map::new();
    let file = state
        .active_file
        .and_then(|id| state.files.get(&id).map(|f| (id, f)));
    let slide = state.active_slide();
    put(&mut p, "tab", state.settings.tab);
    put(
        &mut p,
        "slide-title",
        file.map_or("No slide open".to_string(), |(_, f)| {
            f.info.filename.clone()
        }),
    );

    // Banner: the most important problem.
    let (banner, level, retry) = match (&state.db, file.map(|(_, f)| &f.load)) {
        (DbStatus::Failed { error }, _) => (format!("Annotation database unavailable: {error}"), 2, true),
        (DbStatus::Opening, _) => ("Opening the annotation database…".to_string(), 0, false),
        (_, Some(Load::Failed { error, .. })) => (format!("Could not load this slide's annotations: {error}"), 2, true),
        (_, Some(Load::Pending)) => ("Loading annotations…".to_string(), 0, false),
        (DbStatus::Ready { read_only: true, .. }, _) => (
            "The annotation database is read-only: annotations can be viewed and exported, not edited.".to_string(),
            1,
            false,
        ),
        _ => match slide {
            Some(s) if s.record.identity.is_none() && s.record.legacy_fingerprint.is_none() => (
                "This slide has no stable identity; its annotations are keyed by file name and size.".to_string(),
                1,
                false,
            ),
            _ => (String::new(), 0, false),
        },
    };
    put(&mut p, "banner", banner);
    put(&mut p, "banner-level", level);
    put(&mut p, "banner-retry", retry);
    let editable = state.writable().is_ok() && slide.is_some();
    put(&mut p, "editable", editable);
    put(&mut p, "has-slide", slide.is_some());

    // Classes for combo boxes: "Unclassified" first.
    let mut class_names = vec!["Unclassified".to_string()];
    class_names.extend(state.classes.iter().map(|c| c.name.clone()));
    let class_index = |id: Option<&str>| -> i32 {
        id.and_then(|id| state.classes.iter().position(|c| c.id == id))
            .map_or(0, |i| i as i32 + 1)
    };
    put(&mut p, "class-names", &class_names);

    // Layers and annotations.
    let mut rows = Vec::new();
    let mut note = String::new();
    let mut layer_names = Vec::new();
    let selection: Vec<String> = file.map(|(_, f)| f.selection.clone()).unwrap_or_default();
    let selected: HashSet<&str> = selection.iter().map(String::as_str).collect();
    let hovered = state
        .hovered
        .as_ref()
        .filter(|(f, _)| Some(*f) == state.active_file)
        .map(|(_, id)| id.as_str());
    if let Some(slide) = slide {
        let calibration = Calibration::new(slide.record.mpp_x, slide.record.mpp_y);
        let filter = view.filter.trim().to_lowercase();
        let mut budget = view.limit;
        let mut matched = 0usize;
        let mut hidden_by_limit = 0usize;
        let current = slide.target_layer().map(|l| l.id.clone());
        for layer in &slide.layers {
            layer_names.push(layer.name.clone());
            let count = slide.count_in(&layer.id);
            let auto = count > AUTO_COLLAPSE;
            let collapsed = auto != view.toggled.contains(&layer.id);
            let role = match layer.role {
                LayerRole::Normal => "",
                LayerRole::Counts => " · counts",
                LayerRole::Fields => " · HPF fields",
            };
            rows.push(TreeRow {
                id: layer.id.clone(),
                layer_id: layer.id.clone(),
                kind: 0,
                label: layer.name.clone(),
                detail: format!("{count}{role}"),
                color: layer.color.to_hex(),
                icon: String::new(),
                indent: 0,
                selected: false,
                hovered: false,
                visible: layer.visible,
                locked: layer.locked,
                collapsed,
                current: current.as_deref() == Some(layer.id.as_str()),
                status: 0,
                opacity: layer.opacity,
                notes: layer.notes.clone(),
            });
            // While filtering, matches are listed even in collapsed layers.
            if collapsed && filter.is_empty() {
                continue;
            }
            for a in slide.in_layer(&layer.id) {
                let class = a.class_id.as_deref().and_then(|id| state.class(id));
                if !filter.is_empty() {
                    let hay = format!(
                        "{} {} {} {}",
                        a.display_name().to_lowercase(),
                        class.map_or("unclassified".to_string(), |c| c.name.to_lowercase()),
                        a.geometry.kind().label().to_lowercase(),
                        a.status.as_str()
                    );
                    if !hay.contains(&filter) {
                        continue;
                    }
                    matched += 1;
                }
                if budget == 0 {
                    hidden_by_limit += 1;
                    continue;
                }
                budget -= 1;
                let mut detail = class.map_or(String::new(), |c| c.name.clone());
                if a.geometry.kind().is_area() {
                    let area_px = crate::geometry::area(&a.geometry);
                    let area = match calibration.mm2_per_px2() {
                        Some(k) => measure::format_area_um2(area_px * k * 1_000_000.0),
                        None => measure::format_px2(area_px),
                    };
                    detail = if detail.is_empty() {
                        area
                    } else {
                        format!("{detail} · {area}")
                    };
                }
                rows.push(TreeRow {
                    id: a.id.clone(),
                    layer_id: layer.id.clone(),
                    kind: 1,
                    label: a.display_name(),
                    detail,
                    color: color_of(state, a.class_id.as_deref(), layer.color).to_hex(),
                    icon: actions::kind_glyph(a.geometry.kind()).to_string(),
                    indent: 1,
                    selected: selected.contains(a.id.as_str()),
                    hovered: hovered == Some(a.id.as_str()),
                    visible: layer.visible && class.is_none_or(|c| c.visible),
                    locked: a.locked || layer.locked,
                    collapsed: false,
                    current: false,
                    status: a.status.index(),
                    opacity: 1.0,
                    notes: String::new(),
                });
            }
        }
        if hidden_by_limit > 0 {
            rows.push(TreeRow {
                id: "more".to_string(),
                layer_id: String::new(),
                kind: 2,
                label: format!("Show more ({hidden_by_limit} not shown)"),
                detail: String::new(),
                color: "#000000".to_string(),
                icon: String::new(),
                indent: 1,
                selected: false,
                hovered: false,
                visible: true,
                locked: false,
                collapsed: false,
                current: false,
                status: 0,
                opacity: 1.0,
                notes: String::new(),
            });
        }
        if !filter.is_empty() {
            note = format!("{matched} match(es)");
        } else if slide.layers.is_empty() {
            note = "No annotations yet. Pick a tool in the toolbar (P polygon, . point, C count) and draw on the slide."
                .to_string();
        }
    } else if file.is_none() {
        note = "Open a slide to see and draw its annotations.".to_string();
    }
    put(&mut p, "rows", &rows);
    put(&mut p, "rows-note", note);
    put(&mut p, "filter", &view.filter);
    put(&mut p, "layer-names", &layer_names);

    // Details of the selection.
    let mut has_detail = false;
    let mut detail_id = String::new();
    let mut title = String::new();
    let (mut name, mut description, mut meta, mut warning, mut kind) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    let (mut class_i, mut status_i, mut layer_i, mut locked) = (0, 0, 0, false);
    let mut measurements: Vec<KeyValue> = Vec::new();
    let mut props: Vec<KeyValue> = Vec::new();
    if let Some(slide) = slide {
        let chosen: Vec<_> = selection
            .iter()
            .filter_map(|id| slide.annotations.get(id))
            .collect();
        if let Some(first) = chosen.first() {
            has_detail = true;
            let class = first.class_id.as_deref().and_then(|id| state.class(id));
            class_i = class_index(first.class_id.as_deref());
            status_i = first.status.index();
            layer_i = slide
                .layers
                .iter()
                .position(|l| l.id == first.layer_id)
                .unwrap_or(0) as i32;
            locked = first.locked;
            kind = first.geometry.kind().as_str().to_string();
            if chosen.len() == 1 {
                detail_id = first.id.clone();
                title = format!(
                    "{} · {}",
                    first.geometry.kind().label(),
                    class.map_or("Unclassified", |c| c.name.as_str())
                );
                name = first.name.clone();
                description = first.description.clone();
                let by = if first.author.is_empty() {
                    String::new()
                } else {
                    format!("By {} · ", first.author)
                };
                meta = format!(
                    "{by}created {} · edited {}",
                    date(first.created_at),
                    date(first.updated_at)
                );
                let calibration = Calibration::new(slide.record.mpp_x, slide.record.mpp_y);
                let m = measure::measure(&first.geometry, &calibration);
                measurements = measure::rows(&m, first.geometry.kind().is_line())
                    .into_iter()
                    .map(|(key, value)| KeyValue { key, value })
                    .collect();
                props = first
                    .properties
                    .iter()
                    .map(|(key, value)| KeyValue {
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .collect();
                if slide.invalid.contains(&first.id) {
                    warning = "This outline crosses itself (it was drawn with an older version). Its area uses the \
                               even-odd rule, like the display; Repair turns it into simple parts."
                        .to_string();
                }
            } else {
                detail_id = format!("multi:{}", selection.join(","));
                title = format!("{} annotations selected", chosen.len());
                let classes: HashSet<_> = chosen.iter().map(|a| a.class_id.clone()).collect();
                if classes.len() > 1 {
                    meta = "Mixed classes; changes apply to all selected.".to_string();
                } else {
                    meta = "Changes apply to all selected.".to_string();
                }
            }
        }
    }
    put(&mut p, "selection-count", selection.len() as i32);
    put(&mut p, "has-detail", has_detail);
    put(&mut p, "detail-id", detail_id);
    put(&mut p, "detail-title", title);
    put(&mut p, "detail-kind", kind);
    put(&mut p, "detail-name", name);
    put(&mut p, "detail-description", description);
    put(&mut p, "detail-class", class_i);
    put(&mut p, "detail-status", status_i);
    put(&mut p, "detail-layer", layer_i);
    put(&mut p, "detail-locked", locked);
    put(&mut p, "detail-meta", meta);
    put(&mut p, "detail-warning", warning);
    put(&mut p, "detail-measurements", &measurements);
    put(&mut p, "detail-properties", &props);

    // Classes.
    let class_rows: Vec<ClassRow> = state
        .classes
        .iter()
        .map(|c| ClassRow {
            id: c.id.clone(),
            name: c.name.clone(),
            color: c.color.to_hex(),
            hotkey: c.hotkey.clone().unwrap_or_default(),
            visible: c.visible,
            opacity: c.opacity,
            count: slide.map_or(0, |s| {
                s.annotations
                    .values()
                    .filter(|a| a.class_id.as_deref() == Some(c.id.as_str()))
                    .count() as i32
            }),
            active: state.settings.active_class.as_deref() == Some(c.id.as_str()),
            alternate: state.settings.alternate_class.as_deref() == Some(c.id.as_str()),
            hotkey_note: state
                .refused_hotkeys
                .get(&c.id)
                .map(|reason| format!("eov refused the hotkey: {reason}"))
                .unwrap_or_default(),
        })
        .collect();
    put(&mut p, "classes", &class_rows);

    // Counting.
    put(
        &mut p,
        "count-active-index",
        class_index(state.settings.active_class.as_deref()),
    );
    put(
        &mut p,
        "count-alternate-index",
        class_index(state.settings.alternate_class.as_deref()),
    );
    let mut tally_rows = Vec::new();
    let (mut scope, mut index_line) = (String::new(), String::new());
    let mut field_count = 0;
    if let Some(slide) = slide {
        field_count = slide
            .layer_by_role(LayerRole::Fields)
            .map_or(0, |l| slide.count_in(&l.id)) as i32;
        let t = counting::tally(state, slide, &selection);
        scope = match (t.regions, t.area_mm2) {
            (0, _) => {
                "Whole slide. Select regions or HPF fields to count inside them and get densities."
                    .to_string()
            }
            (n, Some(area)) => format!(
                "Inside {n} selected region(s){}: {}{}",
                if t.fields > 0 {
                    format!(" ({} HPF field(s))", t.fields)
                } else {
                    String::new()
                },
                if t.approximate { "≈ " } else { "" },
                measure::format_area_um2(area * 1_000_000.0)
            ),
            (n, None) => format!(
                "Inside {n} selected region(s): {} (no pixel size, so no density)",
                measure::format_px2(t.area_px)
            ),
        };
        for row in &t.rows {
            let (density, per_hpf) = match t.area_mm2 {
                Some(area) if area > 0.0 => {
                    counting::density_text(row.count, area, &state.settings.field)
                }
                _ => (String::new(), String::new()),
            };
            let per_field = if t.fields > 0 && t.fields == t.regions {
                format!("{:.1} /HPF field", row.count as f64 / t.fields as f64)
            } else {
                per_hpf
            };
            tally_rows.push(TallyRow {
                name: row.name.clone(),
                color: row.color.to_hex(),
                count: row.count.to_string(),
                density,
                per_hpf: per_field,
            });
        }
        if let (Some(a), Some(b)) = (
            &state.settings.active_class,
            &state.settings.alternate_class,
        ) {
            let count = |id: &str| {
                t.rows
                    .iter()
                    .find(|r| r.class_id.as_deref() == Some(id))
                    .map_or(0, |r| r.count)
            };
            if let (Some(ca), Some(cb)) = (state.class(a), state.class(b))
                && let Some(text) = counting::index_text(&ca.name, &cb.name, count(a), count(b))
            {
                index_line = text;
            }
        }
    }
    put(&mut p, "tally", &tally_rows);
    put(&mut p, "count-scope", scope);
    put(&mut p, "count-index", index_line);
    put(&mut p, "field-count", field_count);
    put(&mut p, "count-hidden", state.settings.count_hidden_layers);
    put(&mut p, "field-text", state.settings.field.describe());
    match state.settings.field {
        FieldSize::FieldNumber {
            field_number,
            objective,
        } => {
            put(&mut p, "field-mode", 0);
            put(
                &mut p,
                "field-number",
                crate::stats::trim_number(field_number),
            );
            put(
                &mut p,
                "field-objective",
                crate::stats::trim_number(objective),
            );
            put(
                &mut p,
                "field-area",
                format!("{:.4}", state.settings.field.area_mm2().unwrap_or(0.0)),
            );
        }
        FieldSize::Area { mm2 } => {
            put(&mut p, "field-mode", 1);
            put(&mut p, "field-number", "22");
            put(&mut p, "field-objective", "40");
            put(&mut p, "field-area", crate::stats::trim_number(mm2));
        }
    }

    // Data and settings.
    let (path, status) = match &state.db {
        DbStatus::Ready { read_only, path } => (
            path.clone(),
            if *read_only { "Read-only" } else { "Ready" }.to_string(),
        ),
        DbStatus::Opening => (String::new(), "Opening…".to_string()),
        DbStatus::Failed { error } => (String::new(), format!("Unavailable: {error}")),
    };
    put(&mut p, "db-path", path);
    put(&mut p, "db-status", status);
    put(&mut p, "db-notes", state.db_notes.join("\n"));
    put(
        &mut p,
        "legacy-db",
        state
            .legacy_db
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let orphans: Vec<OrphanRow> = state
        .orphans
        .iter()
        .flatten()
        .map(|s| OrphanRow {
            id: s.record.id.to_string(),
            title: if s.record.filename.is_empty() {
                format!("Slide #{} (from an older version)", s.record.id)
            } else {
                s.record.filename.clone()
            },
            detail: format!(
                "{}{} annotation(s) in {} layer(s) · last opened {}",
                if s.record.width > 0 {
                    format!("{} × {} px · ", s.record.width, s.record.height)
                } else {
                    String::new()
                },
                s.annotations,
                s.layers,
                date(s.record.last_opened_at)
            ),
        })
        .collect();
    let orphans_note = match &state.orphans {
        None => "Press \"List\" to see the other slides in the database.".to_string(),
        Some(list) if list.is_empty() => "No other slides have annotations.".to_string(),
        Some(_) => {
            "If a slide was re-saved or de-identified, its annotations stay with the old file. \
                    Attach them to the open slide here."
                .to_string()
        }
    };
    put(&mut p, "orphans", &orphans);
    put(&mut p, "orphans-note", orphans_note);
    put(&mut p, "setting-author", &state.settings.author);
    put(&mut p, "setting-organization", &state.settings.organization);
    put(&mut p, "setting-project", &state.settings.project);
    put(&mut p, "setting-license", &state.settings.license);
    put(
        &mut p,
        "setting-include-filename",
        state.settings.include_filename,
    );
    put(&mut p, "setting-point-radius", state.settings.point_radius);
    put(&mut p, "setting-fill-opacity", state.settings.fill_opacity);
    put(&mut p, "setting-show-labels", state.settings.show_labels);
    p
}

// ---------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------

fn s(args: &[Value], i: usize) -> String {
    args.get(i)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn b(args: &[Value], i: usize) -> bool {
    args.get(i).and_then(Value::as_bool).unwrap_or(false)
}

fn n(args: &[Value], i: usize) -> f64 {
    args.get(i).and_then(Value::as_f64).unwrap_or(0.0)
}

/// What a callback needs from the plugin besides the core.
pub trait SidebarHost {
    fn view(&self) -> std::sync::MutexGuard<'_, SidebarView>;
    fn refresh(&self);
    fn save_settings(&self);
}

fn class_by_index(state: &State, index: f64) -> Option<String> {
    let index = index as i64;
    if index <= 0 {
        return None;
    }
    state
        .classes
        .get((index - 1) as usize)
        .map(|c| c.id.clone())
}

fn confirm(core: &Arc<Core>, args: ConfirmArgs, pending: Pending) {
    match core.host.confirm(args) {
        Ok(request) => {
            core.state().pending.insert(request, pending);
        }
        Err(e) => core.error("Cannot ask for confirmation", e.message),
    }
}

/// Handles a sidebar callback.
pub fn callback(
    core: &Arc<Core>,
    host: &dyn SidebarHost,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    let file_id = core.state().active_file;
    let need_file = || file_id.ok_or_else(|| "Open a slide first.".to_string());
    match name {
        "tab-changed" => {
            core.state().settings.tab = (n(args, 0) as i32).clamp(0, 3);
            if n(args, 0) as i32 == TAB_DATA && core.state().orphans.is_none() {
                actions::list_orphans(core);
            }
            host.save_settings();
        }
        "retry" => {
            if let Some(file_id) = file_id {
                crate::core::retry_if_due(core, file_id, true);
            } else {
                core.db(crate::core::DbJob::Open);
            }
        }
        "menu" => {
            let item = s(args, 0);
            match item.as_str() {
                "import" => transfer::start_import(core, need_file()?),
                "export-geojson" => transfer::start_export(core, need_file()?, ExportFormat::GeoJson),
                "export-eov" => transfer::start_export(core, need_file()?, ExportFormat::Eov),
                "export-csv" => transfer::start_export(core, need_file()?, ExportFormat::Csv),
                "export-asap" => transfer::start_export(core, need_file()?, ExportFormat::Asap),
                "export-imagescope" => {
                    transfer::start_export(core, need_file()?, ExportFormat::ImageScope)
                }
                "export-all" => transfer::save_dialog(
                    core,
                    "Export all annotations",
                    "annotations-archive.json",
                    FileFilter {
                        name: "eov annotation archive".into(),
                        extensions: vec!["json".into()],
                    },
                    Pending::ExportAll,
                ),
                "backup" => transfer::save_dialog(
                    core,
                    "Back up the annotation database",
                    "annotations-backup.db",
                    FileFilter {
                        name: "SQLite database".into(),
                        extensions: vec!["db".into()],
                    },
                    Pending::Backup,
                ),
                "merge-db" => match core.host.open_file_dialog(FileDialogArgs {
                    title: Some("Merge an annotation database (a backup, or annotations.db of eov before 1.0)".into()),
                    filters: vec![FileFilter {
                        name: "SQLite database".into(),
                        extensions: vec!["db".into(), "sqlite".into()],
                    }],
                    ..Default::default()
                }) {
                    Ok(request) => {
                        core.state().pending.insert(request, Pending::MergeDb);
                    }
                    Err(e) => return Err(e.message),
                },
                other => return Err(format!("unknown menu item '{other}'")),
            }
        }
        "filter-edited" => {
            let mut view = host.view();
            view.filter = s(args, 0).chars().take(200).collect();
            view.limit = ROW_STEP;
        }
        "show-more" => host.view().limit += ROW_STEP,
        "row-clicked" => {
            let file_id = need_file()?;
            let (id, layer, additive) = (s(args, 0), s(args, 1), b(args, 2));
            if id == layer {
                actions::set_current_layer(core, file_id, layer);
            } else {
                let mut selection = core
                    .state()
                    .files
                    .get(&file_id)
                    .map(|f| f.selection.clone())
                    .unwrap_or_default();
                if additive {
                    if let Some(pos) = selection.iter().position(|x| *x == id) {
                        selection.remove(pos);
                    } else {
                        selection.push(id);
                    }
                } else {
                    selection = vec![id];
                }
                actions::select(core, file_id, selection);
            }
        }
        "row-double-clicked" => {
            let file_id = need_file()?;
            let (id, layer) = (s(args, 0), s(args, 1));
            if id != layer {
                actions::select(core, file_id, vec![id.clone()]);
                actions::frame(core, file_id, &id);
            }
        }
        "row-hovered" => {
            let id = s(args, 0);
            let mut state = core.state();
            state.hovered = match (file_id, id.is_empty()) {
                (Some(f), false) => Some((f, id)),
                _ => None,
            };
        }
        "toggle-collapsed" => {
            let id = s(args, 0);
            let mut view = host.view();
            if !view.toggled.remove(&id) {
                view.toggled.insert(id);
            }
        }
        "layer-visible" => {
            let visible = b(args, 1);
            actions::layer_update(
                core,
                need_file()?,
                s(args, 0),
                "Show or hide layer",
                false,
                move |l| {
                    l.visible = visible;
                    Ok(())
                },
            );
        }
        "layer-opacity" => {
            let opacity = crate::model::clean_opacity(n(args, 1));
            actions::layer_update(
                core,
                need_file()?,
                s(args, 0),
                "Layer opacity",
                false,
                move |l| {
                    l.opacity = opacity;
                    Ok(())
                },
            );
        }
        "layer-locked" => {
            let locked = b(args, 1);
            actions::layer_update(
                core,
                need_file()?,
                s(args, 0),
                if locked { "Lock layer" } else { "Unlock layer" },
                true,
                move |l| {
                    l.locked = locked;
                    Ok(())
                },
            );
        }
        "layer-color" => {
            let color = s(args, 1);
            actions::layer_update(
                core,
                need_file()?,
                s(args, 0),
                "Change layer colour",
                true,
                move |l| {
                    l.color = Color::parse(&color)
                        .ok_or_else(|| format!("\"{color}\" is not a colour."))?;
                    Ok(())
                },
            );
        }
        "layer-rename" => actions::layer_rename(core, need_file()?, s(args, 0), s(args, 1)),
        "layer-notes" => {
            let notes = s(args, 1);
            actions::layer_update(
                core,
                need_file()?,
                s(args, 0),
                "Edit layer notes",
                true,
                move |l| {
                    l.notes = clean_text(&notes, "notes")?;
                    Ok(())
                },
            );
        }
        "layer-current" => actions::set_current_layer(core, need_file()?, s(args, 0)),
        "layer-new" => actions::layer_new(core, need_file()?),
        "layer-delete" => {
            let file_id = need_file()?;
            let layer_id = s(args, 0);
            let (name, count) = {
                let state = core.state();
                let slide = state
                    .slide_of(file_id)
                    .ok_or("The slide is still loading.")?;
                let layer = slide
                    .layer(&layer_id)
                    .ok_or("That layer no longer exists.")?;
                (layer.name.clone(), slide.count_in(&layer_id))
            };
            confirm(
                core,
                ConfirmArgs {
                    title: "Delete layer".into(),
                    message: format!(
                        "Delete layer \"{name}\" and its {count} annotation(s)? You can undo this with Ctrl+Z (⌘Z)."
                    ),
                    confirm_label: "Delete".into(),
                    cancel_label: "Cancel".into(),
                    destructive: true,
                },
                Pending::DeleteLayer { file_id, layer_id },
            );
        }
        "detail-name-committed" | "detail-description-committed" => {
            let file_id = need_file()?;
            let (owner, text) = (s(args, 0), s(args, 1));
            let ids = owner_ids(core, file_id, &owner);
            if name == "detail-name-committed" {
                let value = clean_name(&text, "name", true)?;
                actions::edit(core, file_id, ids, "Rename annotation", move |a| {
                    a.name = value.clone();
                    Ok(())
                });
            } else {
                let value = clean_text(&text, "description")?;
                actions::edit(core, file_id, ids, "Edit description", move |a| {
                    a.description = value.clone();
                    Ok(())
                });
            }
        }
        "detail-class-chosen" => {
            let file_id = need_file()?;
            let class = class_by_index(&core.state(), n(args, 0));
            let ids = selection(core, file_id);
            actions::edit(core, file_id, ids, "Set class", move |a| {
                a.class_id = class.clone();
                Ok(())
            });
        }
        "detail-status-chosen" => {
            let file_id = need_file()?;
            let status = Status::from_index(n(args, 0) as i64).unwrap_or_default();
            let ids = selection(core, file_id);
            actions::edit(
                core,
                file_id,
                ids,
                format!("Mark {}", status.label().to_lowercase()),
                move |a| {
                    a.status = status;
                    Ok(())
                },
            );
        }
        "detail-layer-chosen" => {
            let file_id = need_file()?;
            let layer = core
                .state()
                .slide_of(file_id)
                .and_then(|s| s.layers.get(n(args, 0) as usize).map(|l| l.id.clone()))
                .ok_or("That layer no longer exists.")?;
            let ids = selection(core, file_id);
            actions::edit(core, file_id, ids, "Move to layer", move |a| {
                a.layer_id = layer.clone();
                Ok(())
            });
        }
        "detail-locked-toggled" => {
            let file_id = need_file()?;
            let locked = b(args, 0);
            let ids = selection(core, file_id);
            actions::edit(
                core,
                file_id,
                ids,
                if locked { "Lock" } else { "Unlock" },
                move |a| {
                    a.locked = locked;
                    Ok(())
                },
            );
        }
        "detail-delete" => {
            let file_id = need_file()?;
            actions::delete(core, file_id, selection(core, file_id));
        }
        "detail-frame" => {
            let file_id = need_file()?;
            if let Some(id) = selection(core, file_id).first() {
                actions::frame(core, file_id, id);
            }
        }
        "detail-repair" => {
            let file_id = need_file()?;
            if let Some(id) = selection(core, file_id).into_iter().next() {
                actions::repair(core, file_id, id);
            }
        }
        "property-add" | "property-remove" | "property-committed" => {
            let file_id = need_file()?;
            let (owner, row) = (s(args, 0), n(args, 1) as usize);
            let ids = owner_ids(core, file_id, &owner);
            if ids.len() != 1 {
                return Ok(Value::Null);
            }
            let (key, value) = (s(args, 2), s(args, 3));
            let op = name.to_string();
            actions::edit(core, file_id, ids, "Edit properties", move |a| {
                match op.as_str() {
                    "property-add" => a.properties.push((String::new(), String::new())),
                    "property-remove" => {
                        if row < a.properties.len() {
                            a.properties.remove(row);
                        }
                    }
                    _ => {
                        if let Some(entry) = a.properties.get_mut(row) {
                            *entry = (key.clone(), value.clone());
                        }
                    }
                }
                a.properties = clean_properties(&a.properties)?;
                Ok(())
            });
        }
        "copy-measurements" => {
            let file_id = need_file()?;
            let text = measurement_text(&core.state(), file_id);
            if !text.is_empty() {
                core.host.copy_text(text).map_err(String::from)?;
                core.notify(NotifyLevel::Info, "Measurements copied.");
            }
        }
        "class-new" => actions::class_new(core),
        "class-rename" => actions::class_rename(core, s(args, 0), s(args, 1)),
        "class-color" => actions::class_color(core, s(args, 0), s(args, 1)),
        "class-hotkey" => actions::class_hotkey(core, s(args, 0), s(args, 1)),
        "class-visible" => {
            let visible = b(args, 1);
            actions::class_update(
                core,
                s(args, 0),
                "Show or hide class",
                false,
                move |_, c: &mut Class| {
                    c.visible = visible;
                    Ok(())
                },
            );
        }
        "class-opacity" => {
            let opacity = crate::model::clean_opacity(n(args, 1));
            actions::class_update(
                core,
                s(args, 0),
                "Class opacity",
                false,
                move |_, c: &mut Class| {
                    c.opacity = opacity;
                    Ok(())
                },
            );
        }
        "class-delete" => {
            let class_id = s(args, 0);
            let (name, used) = {
                let state = core.state();
                let class = state
                    .class(&class_id)
                    .ok_or("That class no longer exists.")?;
                (class.name.clone(), actions::class_usage(&state, &class_id))
            };
            confirm(
                core,
                ConfirmArgs {
                    title: "Delete class".into(),
                    message: format!(
                        "Delete class \"{name}\"? {used} annotation(s) on open slides use it; they will show as \
                         unclassified. You can undo this."
                    ),
                    confirm_label: "Delete".into(),
                    cancel_label: "Cancel".into(),
                    destructive: true,
                },
                Pending::DeleteClass { class_id },
            );
        }
        "class-activate" => {
            let id = s(args, 0);
            actions::set_active_class(core, (!id.is_empty()).then_some(id));
        }
        "class-alternate" => {
            let id = s(args, 0);
            actions::set_alternate_class(core, (!id.is_empty()).then_some(id));
        }
        "count-active" => {
            let class = class_by_index(&core.state(), n(args, 0));
            actions::set_active_class(core, class);
        }
        "count-alternate" => {
            let class = class_by_index(&core.state(), n(args, 0));
            actions::set_alternate_class(core, class);
        }
        "count-hidden-toggled" => {
            core.state().settings.count_hidden_layers = b(args, 0);
            host.save_settings();
        }
        "count-clear" => {
            let file_id = need_file()?;
            let count = core
                .state()
                .slide_of(file_id)
                .and_then(|s| {
                    s.layer_by_role(LayerRole::Counts)
                        .map(|l| s.count_in(&l.id))
                })
                .unwrap_or(0);
            if count == 0 {
                return Ok(Value::Null);
            }
            confirm(
                core,
                ConfirmArgs {
                    title: "Clear counts".into(),
                    message: format!(
                        "Remove all {count} count point(s) of this slide? You can undo this."
                    ),
                    confirm_label: "Clear".into(),
                    cancel_label: "Cancel".into(),
                    destructive: true,
                },
                Pending::ClearCounts { file_id },
            );
        }
        "fields-select" => actions::select_fields(core, need_file()?),
        "field-config" => {
            let mode = n(args, 0) as i32;
            let parse = |v: &str| v.trim().replace(',', ".").parse::<f64>().ok();
            let field = if mode == 1 {
                FieldSize::Area {
                    mm2: parse(&s(args, 3)).ok_or("Enter the field area in mm², e.g. 0.237.")?,
                }
            } else {
                FieldSize::FieldNumber {
                    field_number: parse(&s(args, 1))
                        .ok_or("Enter the field number in mm, e.g. 22.")?,
                    objective: parse(&s(args, 2))
                        .ok_or("Enter the objective magnification, e.g. 40.")?,
                }
            };
            if field.area_mm2().is_none() {
                return Err("That field size is not usable.".to_string());
            }
            core.state().settings.field = field;
            host.save_settings();
            actions::refresh_reticles(core);
        }
        "orphans-refresh" => actions::list_orphans(core),
        "legacy-merge" => {
            let path = core
                .state()
                .legacy_db
                .clone()
                .ok_or("There is no database from before eov 1.0 to merge.")?;
            transfer::merge_database(core, path, true);
        }
        "orphan-relink" => {
            let file_id = need_file()?;
            let from: i64 = s(args, 0).parse().map_err(|_| "unknown slide")?;
            let (title, dims_differ) = {
                let state = core.state();
                let target = state
                    .files
                    .get(&file_id)
                    .map(|f| (f.info.width, f.info.height));
                let orphan = state
                    .orphans
                    .iter()
                    .flatten()
                    .find(|o| o.record.id == from)
                    .ok_or("That slide is not listed any more.")?;
                (
                    orphan.record.filename.clone(),
                    orphan.record.width > 0
                        && target.is_some_and(|(w, h)| {
                            (w, h) != (orphan.record.width, orphan.record.height)
                        }),
                )
            };
            let mut message = format!(
                "Attach the annotations of \"{}\" to the open slide? Use this when the slide file was re-saved, \
                 converted or de-identified. You can undo this.",
                if title.is_empty() {
                    "an older slide"
                } else {
                    title.as_str()
                }
            );
            if dims_differ {
                message.push_str(" Warning: the two slides have different sizes, so the annotations may not line up.");
            }
            confirm(
                core,
                ConfirmArgs {
                    title: "Attach annotations".into(),
                    message,
                    confirm_label: "Attach".into(),
                    cancel_label: "Cancel".into(),
                    destructive: dims_differ,
                },
                Pending::Relink { file_id, from },
            );
        }
        "setting-text" => {
            let (key, value) = (s(args, 0), s(args, 1));
            let value = clean_name(&value, "setting", true)?;
            {
                let mut state = core.state();
                match key.as_str() {
                    "author" => state.settings.author = value,
                    "organization" => state.settings.organization = value,
                    "project" => state.settings.project = value,
                    "license" => state.settings.license = value,
                    other => return Err(format!("unknown setting '{other}'")),
                }
            }
            host.save_settings();
        }
        "setting-bool" => {
            let (key, value) = (s(args, 0), b(args, 1));
            let restyle = {
                let mut state = core.state();
                match key.as_str() {
                    "include-filename" => {
                        state.settings.include_filename = value;
                        false
                    }
                    "show-labels" => {
                        state.settings.show_labels = value;
                        true
                    }
                    other => return Err(format!("unknown setting '{other}'")),
                }
            };
            host.save_settings();
            if restyle {
                restyle_all(core);
            }
        }
        "setting-number" => {
            let (key, value) = (s(args, 0), n(args, 1) as f32);
            {
                let mut state = core.state();
                match key.as_str() {
                    "point-radius" => state.settings.point_radius = value.clamp(1.0, 20.0),
                    "fill-opacity" => state.settings.fill_opacity = value.clamp(0.0, 1.0),
                    other => return Err(format!("unknown setting '{other}'")),
                }
            }
            host.save_settings();
            restyle_all(core);
        }
        "tool" => {
            let tool = s(args, 0);
            if crate::tools::is_tool(&tool) {
                core.host.activate_tool(&tool).map_err(String::from)?;
            }
        }
        _ => return Ok(Value::Null),
    }
    host.refresh();
    Ok(Value::Null)
}

/// Rebuilds every object after a display setting changed.
fn restyle_all(core: &Arc<Core>) {
    // Off the UI thread: this touches every object of every open slide.
    let task = move |core: &Arc<Core>, _: &mut crate::core::DbCtx| {
        let batch = crate::overlay::sync_all(&mut core.state());
        core.exec(batch.build());
    };
    core.db_run(task);
}

fn selection(core: &Arc<Core>, file_id: FileId) -> Vec<String> {
    core.state()
        .files
        .get(&file_id)
        .map(|f| f.selection.clone())
        .unwrap_or_default()
}

/// The annotations a draft belonged to (`detail-id` when it was typed).
fn owner_ids(core: &Arc<Core>, file_id: FileId, owner: &str) -> Vec<String> {
    if let Some(list) = owner.strip_prefix("multi:") {
        return list
            .split(',')
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect();
    }
    if owner.is_empty() {
        return selection(core, file_id);
    }
    vec![owner.to_string()]
}

/// Tab-separated measurements of the selection.
pub fn measurement_text(state: &State, file_id: FileId) -> String {
    let Some(slide) = state.slide_of(file_id) else {
        return String::new();
    };
    let Some(file) = state.files.get(&file_id) else {
        return String::new();
    };
    let calibration = Calibration::new(slide.record.mpp_x, slide.record.mpp_y);
    let mut out = String::new();
    for id in &file.selection {
        let Some(a) = slide.annotations.get(id) else {
            continue;
        };
        let m = measure::measure(&a.geometry, &calibration);
        out.push_str(&a.display_name());
        for (key, value) in measure::rows(&m, a.geometry.kind().is_line()) {
            out.push_str(&format!("\t{key}: {value}"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Settings;

    #[test]
    fn an_empty_state_has_every_property() {
        let state = State::new(Settings::default());
        let p = properties(&state, &SidebarView::default());
        for key in [
            "rows",
            "classes",
            "tally",
            "detail-properties",
            "banner",
            "orphans",
            "field-text",
        ] {
            assert!(p.contains_key(key), "{key}");
        }
        assert_eq!(p["slide-title"], "No slide open");
        assert_eq!(p["editable"], false);
    }
}
