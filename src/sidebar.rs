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
use crate::compare::Subject;
use crate::core::{Core, label};
use crate::counting;
use crate::measure::{self, Calibration};
use crate::model::{
    Annotation, Class, Color, Field, GeometryKind, LayerRole, Status, clean_name, clean_properties,
    clean_text,
};
use crate::state::{DbStatus, ExportFormat, Load, Pending, State};
use crate::stats::FieldSize;
use crate::transfer;
use eov_plugin_api::i18n;
use eov_plugin_api::prelude::*;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
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
    ("comment-add", "ss"),
    ("comment-remove", "ss"),
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
    ("count-immune", "n"),
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

/// Sidebar state that lives only in the UI. (Which layers are collapsed
/// is saved with each slide: `SlideData::collapsed`.)
#[derive(Debug, Clone)]
pub struct SidebarView {
    pub filter: String,
    pub limit: usize,
}

impl Default for SidebarView {
    fn default() -> Self {
        Self {
            filter: String::new(),
            limit: ROW_STEP,
        }
    }
}

/// Names the list shows for many rows, in the interface's language, worked
/// out once per refresh rather than once per row.
struct Names {
    kinds: HashMap<GeometryKind, String>,
    unclassified: String,
}

impl Names {
    fn new() -> Self {
        let kinds = [
            GeometryKind::Point,
            GeometryKind::Polyline,
            GeometryKind::Arrow,
            GeometryKind::Polygon,
            GeometryKind::Rectangle,
            GeometryKind::Ellipse,
            GeometryKind::Text,
        ]
        .into_iter()
        .map(|kind| (kind, kind.shown_label()))
        .collect();
        Self {
            kinds,
            unclassified: tr!("Unclassified"),
        }
    }

    fn kind(&self, kind: GeometryKind) -> &str {
        self.kinds.get(&kind).map_or(kind.label(), String::as_str)
    }

    /// An annotation's name, else its kind (see [`Annotation::shown_name`]).
    fn shown<'a>(&'a self, a: &'a Annotation) -> &'a str {
        if a.name.trim().is_empty() {
            self.kind(a.geometry.kind())
        } else {
            &a.name
        }
    }
}

/// An annotation as the list orders it.
struct Listed<'a> {
    annotation: &'a Arc<Annotation>,
    class: Option<&'a Class>,
    /// What the list calls it (worked out once, not per comparison).
    name: &'a str,
    /// Where it is on the slide (worked out once, not per comparison).
    anchor: [f64; 2],
}

/// The order of a layer's annotations in the list: by name as people read
/// it, then by class, then by where they are on the slide (top to bottom,
/// left to right), then by age and id. The order never depends on chance,
/// such as the random ids that break ties in creation time.
fn list_order(a: &Listed, b: &Listed) -> Ordering {
    let (a_name, b_name) = (a.name, b.name);
    let a_class = a.class.map_or("", |c| c.name.as_str());
    let b_class = b.class.map_or("", |c| c.name.as_str());
    natural_cmp(a_name, b_name)
        .then_with(|| a_name.cmp(b_name))
        .then_with(|| natural_cmp(a_class, b_class))
        .then_with(|| a.anchor[1].total_cmp(&b.anchor[1]))
        .then_with(|| a.anchor[0].total_cmp(&b.anchor[0]))
        .then_with(|| a.annotation.created_at.cmp(&b.annotation.created_at))
        .then_with(|| a.annotation.id.cmp(&b.annotation.id))
}

/// Compares names as people read them: ignoring case, with runs of digits
/// compared by value ("Region 2" before "Region 10"; "2" before "02").
fn natural_cmp(mut a: &str, mut b: &str) -> Ordering {
    loop {
        let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) else {
            // A name that is the start of the other comes first.
            return b.is_empty().cmp(&a.is_empty());
        };
        let ordering = if x.is_ascii_digit() && y.is_ascii_digit() {
            let (x_digits, a_rest) = split_digits(a);
            let (y_digits, b_rest) = split_digits(b);
            (a, b) = (a_rest, b_rest);
            let x_value = x_digits.trim_start_matches('0');
            let y_value = y_digits.trim_start_matches('0');
            x_value
                .len()
                .cmp(&y_value.len())
                .then_with(|| x_value.cmp(y_value))
                .then_with(|| x_digits.len().cmp(&y_digits.len()))
        } else {
            (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
            x.to_lowercase().cmp(y.to_lowercase())
        };
        if ordering.is_ne() {
            return ordering;
        }
    }
}

/// The leading digits of `text`, and the rest.
fn split_digits(text: &str) -> (&str, &str) {
    text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    )
}

/// Whether a layer's annotations are folded away in the tree: what the
/// user chose for it, or folded when it is big.
pub fn layer_collapsed(slide: &crate::state::SlideData, layer_id: &str) -> bool {
    slide
        .collapsed
        .get(layer_id)
        .copied()
        .unwrap_or_else(|| slide.count_in(layer_id) > AUTO_COLLAPSE)
}

/// Collapses or expands a layer and saves the choice with its slide.
fn toggle_collapsed(core: &Arc<Core>, layer_id: &str) {
    let saved = {
        let mut state = core.state();
        let Some(slide_id) = state.layer_slide.get(layer_id).copied() else {
            return;
        };
        let Some(slide) = state.slides.get_mut(&slide_id) else {
            return;
        };
        let collapsed = !layer_collapsed(slide, layer_id);
        slide.collapsed.insert(layer_id.to_string(), collapsed);
        let layers: HashSet<&str> = slide.layers.iter().map(|l| l.id.as_str()).collect();
        slide
            .collapsed
            .retain(|layer, _| layers.contains(layer.as_str()));
        (slide_id > 0).then(|| (slide_id, slide.collapsed.clone()))
    };
    if let Some((slide_id, collapsed)) = saved {
        core.db_run(move |core, ctx| {
            if let Some(db) = ctx.db.as_mut()
                && let Err(e) = db.set_collapsed_layers(slide_id, &collapsed)
                && e != crate::db::DbError::ReadOnly
            {
                // A convenience lost; for the log.
                core.host
                    .info(format!("cannot remember the collapsed layers: {e}"));
            }
        });
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
pub struct CommentRow {
    pub id: String,
    /// "Author · date".
    pub byline: String,
    pub text: String,
}

/// One class in the comparison with another reader.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CompareRow {
    pub name: String,
    pub color: String,
    /// "" when neither reader has points of the class.
    pub points: String,
    /// "" when neither reader has regions of the class.
    pub regions: String,
}

fn compare_row(row: &crate::compare::Row, area: &dyn Fn(f64) -> String) -> CompareRow {
    let ratio = |v: Option<f64>| v.map_or_else(|| "–".to_string(), |v| i18n::format_decimal(v, 2));
    let points = if row.points == (0, 0) {
        String::new()
    } else {
        tr!(
            // Translators: this slide's points against the other reader's; F1 is the F1 score.
            "Points {ours} vs {theirs} · {matched} matched · F1 {f1}",
            ours = row.points.0,
            theirs = row.points.1,
            matched = row.matched,
            f1 = ratio(row.point_agreement())
        )
    };
    let regions = if row.regions == (0, 0) {
        String::new()
    } else {
        tr!(
            // Translators: this slide's regions and their area against the other reader's;
            // Dice is the Dice coefficient of the areas.
            "Regions {ours} vs {theirs} · {our_area} vs {their_area} · Dice {dice}",
            ours = row.regions.0,
            theirs = row.regions.1,
            our_area = i18n::isolate(area(row.areas.0)),
            their_area = i18n::isolate(area(row.areas.1)),
            dice = ratio(row.dice())
        )
    };
    CompareRow {
        name: match row.subject {
            Subject::Class => row.name.clone(),
            Subject::Unclassified => tr!("Unclassified"),
            Subject::AllRegions => tr!("All regions"),
        },
        color: row
            .color
            .map_or_else(|| "#00000000".to_string(), |c| c.to_hex()),
        points,
        regions,
    }
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

/// A time as the sidebar shows it ("2026-10-15 09:30 UTC"), isolated for
/// insertion into a text: in right-to-left languages its parts keep their
/// order (see [`i18n::isolate`]).
fn date(ms: i64) -> String {
    let text = crate::formats::iso8601(ms);
    // 2026-10-15T09:30:00Z → 2026-10-15 09:30 UTC
    i18n::isolate(format!("{} {} UTC", &text[..10], &text[11..16]))
}

/// "in 3 layers", for the texts that count annotations and their layers
/// ("12 annotations in 3 layers"): a text of its own, so that the
/// layers take the plural form of their own number in every language.
pub fn in_layers(layers: usize) -> String {
    tr!(
        // Translators: {layers} in texts such as "{n} annotations {layers}" ("12 annotations in 3 layers").
        "in {n} layer" | "in {n} layers" % layers
    )
}

/// Every sidebar property.
pub fn properties(state: &State, view: &SidebarView) -> Map<String, Value> {
    let mut p = Map::new();
    let file = state
        .active_file
        .and_then(|id| state.files.get(&id).map(|f| (id, f)));
    let slide = state.active_slide();
    let names = Names::new();
    put(&mut p, "tab", state.settings.tab);
    put(
        &mut p,
        "slide-title",
        file.map_or_else(|| tr!("No slide open"), |(_, f)| f.info.filename.clone()),
    );

    // Banner: the most important problem.
    let (banner, level, retry) = match (&state.db, file.map(|(_, f)| &f.load)) {
        (DbStatus::Failed { error }, _) => {
            (tr!("Annotation database unavailable: {}", error), 2, true)
        }
        (DbStatus::Opening, _) => (tr!("Opening the annotation database…"), 0, false),
        (_, Some(Load::Failed { error, .. })) => (
            tr!("Could not load this slide's annotations: {}", error),
            2,
            true,
        ),
        (_, Some(Load::Pending)) => (tr!("Loading annotations…"), 0, false),
        (
            DbStatus::Ready {
                read_only: true, ..
            },
            _,
        ) => (
            tr!(
                "The annotation database is read-only: annotations can be viewed and exported, not edited."
            ),
            1,
            false,
        ),
        _ => match slide {
            Some(s) if s.record.identity.is_none() && s.record.legacy_fingerprint.is_none() => (
                tr!(
                    "This slide has no stable identity; its annotations are keyed by file name and size."
                ),
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
    let mut class_names = vec![names.unclassified.clone()];
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
            let collapsed = layer_collapsed(slide, &layer.id);
            let detail = match layer.role {
                LayerRole::Normal => count.to_string(),
                // Translators: beside a layer of count points: the number of points.
                LayerRole::Counts => tr!("{} · counts", count),
                // Translators: beside the layer of HPF fields: the number of fields.
                LayerRole::Fields => tr!("{} · HPF fields", count),
            };
            rows.push(TreeRow {
                id: layer.id.clone(),
                layer_id: layer.id.clone(),
                kind: 0,
                label: layer.name.clone(),
                detail,
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
            let mut listed: Vec<_> = slide
                .in_layer(&layer.id)
                .map(|a| (a, a.class_id.as_deref().and_then(|id| state.class(id))))
                .filter(|(a, class)| {
                    filter.is_empty() || {
                        // What the list shows, and the English words.
                        let kind = a.geometry.kind();
                        let hay = format!(
                            "{} {} {} {} {} {}",
                            names.shown(a).to_lowercase(),
                            class.map_or_else(
                                || format!("{} unclassified", names.unclassified.to_lowercase()),
                                |c| c.name.to_lowercase()
                            ),
                            names.kind(kind).to_lowercase(),
                            kind.label().to_lowercase(),
                            a.status.label().to_lowercase(),
                            a.status.as_str()
                        );
                        hay.contains(&filter)
                    }
                })
                .map(|(annotation, class)| Listed {
                    annotation,
                    class,
                    name: names.shown(annotation),
                    anchor: annotation.geometry.anchor(),
                })
                .collect();
            if !filter.is_empty() {
                matched += listed.len();
            }
            // Only the rows within the limit are put in order; the others
            // are counted.
            let shown = budget.min(listed.len());
            if shown < listed.len() {
                hidden_by_limit += listed.len() - shown;
                listed.select_nth_unstable_by(shown, list_order);
                listed.truncate(shown);
            }
            listed.sort_by(list_order);
            budget -= shown;
            for Listed {
                annotation: a,
                class,
                ..
            } in listed
            {
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
                if !a.comments.is_empty() {
                    let comments = tr!("{n} comment" | "{n} comments" % a.comments.len());
                    detail = if detail.is_empty() {
                        comments
                    } else {
                        format!("{detail} · {comments}")
                    };
                }
                rows.push(TreeRow {
                    id: a.id.clone(),
                    layer_id: layer.id.clone(),
                    kind: 1,
                    label: names.shown(a).to_string(),
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
                label: tr!(
                    "Show more ({n} not shown)" | "Show more ({n} not shown)" % hidden_by_limit
                ),
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
            note = tr!("{n} match" | "{n} matches" % matched);
        } else if slide.layers.is_empty() {
            note = tr!(
                "No annotations yet. Pick a tool in the toolbar (P polygon, . point, C count) and draw on the slide."
            );
        }
    } else if file.is_none() {
        note = tr!("Open a slide to see and draw its annotations.");
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
    let mut comments: Vec<CommentRow> = Vec::new();
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
                    names.kind(first.geometry.kind()),
                    class.map_or(names.unclassified.as_str(), |c| c.name.as_str())
                );
                name = first.name.clone();
                description = first.description.clone();
                meta = if first.author.is_empty() {
                    tr!(
                        "created {created} · edited {edited}",
                        created = date(first.created_at),
                        edited = date(first.updated_at)
                    )
                } else {
                    tr!(
                        "By {author} · created {created} · edited {edited}",
                        author = i18n::isolate(&first.author),
                        created = date(first.created_at),
                        edited = date(first.updated_at)
                    )
                };
                let calibration = Calibration::new(slide.record.mpp_x, slide.record.mpp_y);
                let m = measure::measure(&first.geometry, &calibration);
                measurements = measure::rows(&m, first.geometry.kind().is_line())
                    .into_iter()
                    .map(|(key, value)| KeyValue { key, value })
                    .collect();
                comments = first
                    .comments
                    .iter()
                    .map(|c| CommentRow {
                        id: c.id.clone(),
                        byline: format!(
                            "{} · {}",
                            if c.author.trim().is_empty() {
                                tr!("Unknown author")
                            } else {
                                c.author.clone()
                            },
                            date(c.created_at)
                        ),
                        text: c.text.clone(),
                    })
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
                    warning = tr!(
                        "This outline crosses itself (it was drawn with an older version). Its area uses the \
                         even-odd rule, like the display; Repair turns it into simple parts."
                    );
                }
            } else {
                detail_id = format!("multi:{}", selection.join(","));
                title = tr!("{n} annotation selected" | "{n} annotations selected" % chosen.len());
                let classes: HashSet<_> = chosen.iter().map(|a| a.class_id.clone()).collect();
                if classes.len() > 1 {
                    meta = tr!("Mixed classes; changes apply to all selected.");
                } else {
                    meta = tr!("Changes apply to all selected.");
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
    put(&mut p, "detail-comments", &comments);

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
                .map(|refused| {
                    // Translators: under a class whose key does nothing; {} is
                    // eov's reason, a whole sentence such as "Hotkey 'q' of …
                    // was ignored: it is a key eov already uses."
                    tr!("eov refused the hotkey: {}", refused.shown())
                })
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
    put(
        &mut p,
        "count-immune-index",
        class_index(state.settings.immune_class.as_deref()),
    );
    let mut tally_rows = Vec::new();
    let (mut scope, mut index_line) = (String::new(), String::new());
    let mut field_count = 0;
    if let Some(slide) = slide {
        field_count = slide
            .layer_by_role(LayerRole::Fields)
            .map_or(0, |l| slide.count_in(&l.id)) as i32;
        let t = counting::tally_cached(state, slide, &selection);
        scope = match (t.regions, t.area_mm2) {
            (0, _) => tr!(
                "Whole slide. Select regions or HPF fields to count inside them and get densities."
            ),
            (n, Some(area)) => {
                let area = i18n::isolate(format!(
                    "{}{}",
                    if t.approximate { "≈ " } else { "" },
                    measure::format_area_um2(area * 1_000_000.0)
                ));
                if t.fields > 0 {
                    tr!(
                        // Translators: {fields} is "1 HPF field" or "3 HPF fields"
                        // (translated on its own; HPF is a high-power field).
                        "Inside {n} selected region ({fields}): {area}"
                            | "Inside {n} selected regions ({fields}): {area}" % n,
                        fields = tr!(
                            // Translators: {fields} in "Inside {n} selected regions ({fields}): {area}".
                            "{n} HPF field" | "{n} HPF fields" % t.fields
                        ),
                        area = area
                    )
                } else {
                    tr!(
                        "Inside {n} selected region: {area}"
                            | "Inside {n} selected regions: {area}" % n,
                        area = area
                    )
                }
            }
            (n, None) => tr!(
                "Inside {n} selected region: {area} (no pixel size, so no density)"
                    | "Inside {n} selected regions: {area} (no pixel size, so no density)" % n,
                area = i18n::isolate(measure::format_px2(t.area_px))
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
                // Translators: cells per high-power field.
                tr!(
                    "{} /HPF field",
                    i18n::format_decimal(row.count as f64 / t.fields as f64, 1)
                )
            } else {
                per_hpf
            };
            tally_rows.push(TallyRow {
                // Points without a (known) class.
                name: if row.class_id.is_none() {
                    names.unclassified.clone()
                } else {
                    row.name.clone()
                },
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
            // The combined positive score, with the click and Shift+click
            // classes as positive and negative tumour cells.
            if let Some(c) = &state.settings.immune_class
                && c != a
                && c != b
                && let (Some(ca), Some(cb), Some(cc)) =
                    (state.class(a), state.class(b), state.class(c))
                && let Some(text) =
                    counting::cps_text(&ca.name, &cb.name, &cc.name, count(a), count(b), count(c))
            {
                if !index_line.is_empty() {
                    index_line.push('\n');
                }
                index_line.push_str(&text);
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
            // Numbers to edit, as the language writes them (`parse_decimal`
            // reads them back, and `.` or `,` as well).
            put(
                &mut p,
                "field-number",
                crate::stats::trim_decimal(field_number),
            );
            put(
                &mut p,
                "field-objective",
                crate::stats::trim_decimal(objective),
            );
            put(
                &mut p,
                "field-area",
                i18n::format_decimal(state.settings.field.area_mm2().unwrap_or(0.0), 4),
            );
        }
        FieldSize::Area { mm2 } => {
            put(&mut p, "field-mode", 1);
            put(&mut p, "field-number", "22");
            put(&mut p, "field-objective", "40");
            put(&mut p, "field-area", crate::stats::trim_decimal(mm2));
        }
    }

    // Data and settings.
    let (path, status) = match &state.db {
        DbStatus::Ready { read_only, path } => (
            path.clone(),
            if *read_only {
                // Translators: the state of the annotation database.
                tr!("Read-only")
            } else {
                // Translators: the state of the annotation database.
                tr!("Ready")
            },
        ),
        // Translators: the state of the annotation database.
        DbStatus::Opening => (String::new(), tr!("Opening…")),
        DbStatus::Failed { error } => (String::new(), tr!("Unavailable: {}", error)),
    };
    // A second reader.
    let comparison = state
        .comparison
        .as_ref()
        .filter(|c| Some(c.file_id) == state.active_file);
    let spacing = file.and_then(|(_, f)| Calibration::new(f.info.mpp_x, f.info.mpp_y).spacing());
    let area = |px2: f64| match spacing {
        Some((x, y, _)) => measure::format_area_um2(px2 * x * y),
        None => measure::format_px2(px2),
    };
    let compare_rows: Vec<CompareRow> = comparison
        .map(|c| c.rows.iter().map(|r| compare_row(r, &area)).collect())
        .unwrap_or_default();
    put(
        &mut p,
        "compare-title",
        comparison
            .map(crate::state::Comparison::title)
            .unwrap_or_default(),
    );
    put(
        &mut p,
        "compare-note",
        comparison
            .map(crate::state::Comparison::note)
            .unwrap_or_default(),
    );
    put(&mut p, "compare-rows", &compare_rows);

    put(&mut p, "db-path", path);
    put(&mut p, "db-status", status);
    put(
        &mut p,
        "db-notes",
        state
            .db_notes
            .iter()
            .map(crate::db::DbNote::text)
            .collect::<Vec<_>>()
            .join("\n"),
    );
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
                // Translators: a slide whose file name an older eov did not record; {} is its number in the annotation database.
                tr!("Slide #{} (from an older version)", s.record.id)
            } else {
                s.record.filename.clone()
            },
            detail: {
                let contents = tr!(
                    // Translators: {layers} is "in 1 layer" or "in 3 layers" (translated on its own).
                    "{n} annotation {layers} · last opened {date}"
                        | "{n} annotations {layers} · last opened {date}" % s.annotations,
                    layers = in_layers(s.layers),
                    date = date(s.record.last_opened_at)
                );
                if s.record.width > 0 {
                    // Isolated, so that the translated text sets the
                    // direction of the line.
                    let size = format!("{} × {} px", s.record.width, s.record.height);
                    format!("{} · {contents}", i18n::isolate(size))
                } else {
                    contents
                }
            },
        })
        .collect();
    let orphans_note = match &state.orphans {
        // Translators: "List" is the label of the button beside this text.
        None => tr!("Press \"List\" to see the other slides in the database."),
        Some(list) if list.is_empty() => tr!("No other slides have annotations."),
        Some(_) => tr!(
            "If a slide was re-saved or de-identified, its annotations stay with the old file. \
             Attach them to the open slide here."
        ),
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
    put(&mut p, "setting-brush-size", state.settings.brush_size);
    put(&mut p, "setting-brush-eraser", state.settings.brush_eraser);
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
        Err(e) => core.error(tr!("Cannot ask for confirmation"), e.message),
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
    let need_file = || file_id.ok_or_else(|| tr!("Open a slide first."));
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
                "export-dicom" => transfer::start_export(core, need_file()?, ExportFormat::Dicom),
                "export-blinded" => {
                    transfer::start_export(core, need_file()?, ExportFormat::Blinded)
                }
                "compare" => transfer::start_compare(core, need_file()?),
                "compare-again" => {
                    let file_id = need_file()?;
                    let path = core
                        .state()
                        .comparison
                        .as_ref()
                        .filter(|c| c.file_id == file_id)
                        .map(|c| c.path.clone())
                        .ok_or_else(|| tr!("Compare with another reader's file first."))?;
                    transfer::compare_file(core, file_id, path);
                }
                "compare-clear" => core.state().comparison = None,
                "export-all" => transfer::save_dialog(
                    core,
                    &tr!("Export all annotations"),
                    "annotations-archive.json",
                    FileFilter {
                        name: tr!("eov annotation archive"),
                        extensions: vec!["json".into()],
                    },
                    Pending::ExportAll,
                ),
                "backup" => transfer::save_dialog(
                    core,
                    &tr!("Back up the annotation database"),
                    "annotations-backup.db",
                    FileFilter {
                        name: tr!("SQLite database"),
                        extensions: vec!["db".into()],
                    },
                    Pending::Backup,
                ),
                "merge-db" => match core.host.open_file_dialog(FileDialogArgs {
                    title: Some(tr!(
                        "Merge an annotation database (a backup, or annotations.db of eov before 1.0)"
                    )),
                    filters: vec![FileFilter {
                        name: tr!("SQLite database"),
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
        "toggle-collapsed" => toggle_collapsed(core, &s(args, 0)),
        "layer-visible" => {
            let visible = b(args, 1);
            actions::layer_update(
                core,
                need_file()?,
                s(args, 0),
                label!(tr!("Show or hide layer")),
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
                label!(tr!("Layer opacity")),
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
                if locked {
                    label!(tr!("Lock layer"))
                } else {
                    label!(tr!("Unlock layer"))
                },
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
                label!(tr!("Change layer colour")),
                true,
                move |l| {
                    l.color = Color::parse(&color)
                        .ok_or_else(|| tr!("\"{}\" is not a colour.", color))?;
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
                label!(tr!("Edit layer notes")),
                true,
                move |l| {
                    l.notes = clean_text(&notes, Field::Notes)?;
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
                    .ok_or_else(|| tr!("The slide is still loading."))?;
                let layer = slide
                    .layer(&layer_id)
                    .ok_or_else(|| tr!("That layer no longer exists."))?;
                (layer.name.clone(), slide.count_in(&layer_id))
            };
            confirm(
                core,
                ConfirmArgs {
                    title: tr!("Dialog title" => "Delete layer"),
                    message: tr!(
                        "Delete layer \"{name}\" and its {n} annotation? You can undo this with Ctrl+Z (⌘Z)."
                            | "Delete layer \"{name}\" and its {n} annotations? You can undo this with Ctrl+Z (⌘Z)."
                                % count,
                        name = i18n::isolate(&name)
                    ),
                    confirm_label: tr!("Delete"),
                    cancel_label: tr!("Cancel"),
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
                let value = clean_name(&text, Field::Name, true)?;
                actions::edit(
                    core,
                    file_id,
                    ids,
                    label!(tr!("Rename annotation")),
                    move |a| {
                        a.name = value.clone();
                        Ok(())
                    },
                );
            } else {
                let value = clean_text(&text, Field::Description)?;
                actions::edit(
                    core,
                    file_id,
                    ids,
                    label!(tr!("Edit description")),
                    move |a| {
                        a.description = value.clone();
                        Ok(())
                    },
                );
            }
        }
        "detail-class-chosen" => {
            let file_id = need_file()?;
            let class = class_by_index(&core.state(), n(args, 0));
            let ids = selection(core, file_id);
            actions::edit(core, file_id, ids, label!(tr!("Set class")), move |a| {
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
                match status {
                    Status::Draft => label!(tr!("Mark draft")),
                    Status::Verified => label!(tr!("Mark verified")),
                    Status::Rejected => label!(tr!("Mark rejected")),
                },
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
                .ok_or_else(|| tr!("That layer no longer exists."))?;
            let ids = selection(core, file_id);
            actions::edit(core, file_id, ids, label!(tr!("Move to layer")), move |a| {
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
                if locked {
                    label!(tr!("Lock"))
                } else {
                    label!(tr!("Unlock"))
                },
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
            actions::edit(
                core,
                file_id,
                ids,
                label!(tr!("Edit properties")),
                move |a| {
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
                },
            );
        }
        "comment-add" => {
            let file_id = need_file()?;
            let (owner, text) = (s(args, 0), s(args, 1));
            if owner.is_empty() || owner.starts_with("multi:") {
                return Err(tr!("Select one annotation to comment on."));
            }
            actions::comment_add(core, file_id, owner, text);
        }
        "comment-remove" => {
            let file_id = need_file()?;
            actions::comment_remove(core, file_id, s(args, 0), s(args, 1));
        }
        "copy-measurements" => {
            let file_id = need_file()?;
            let text = measurement_text(&core.state(), file_id);
            if !text.is_empty() {
                core.host.copy_text(text).map_err(String::from)?;
                core.notify(NotifyLevel::Info, tr!("Measurements copied."));
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
                label!(tr!("Show or hide class")),
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
                label!(tr!("Class opacity")),
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
                    .ok_or_else(|| tr!("That class no longer exists."))?;
                (class.name.clone(), actions::class_usage(&state, &class_id))
            };
            confirm(
                core,
                ConfirmArgs {
                    title: tr!("Dialog title" => "Delete class"),
                    message: tr!(
                        "Delete class \"{name}\"? {n} annotation on open slides uses it; it will show as \
                         unclassified. You can undo this."
                            | "Delete class \"{name}\"? {n} annotations on open slides use it; they will show as \
                         unclassified. You can undo this."
                                % used,
                        name = i18n::isolate(&name)
                    ),
                    confirm_label: tr!("Delete"),
                    cancel_label: tr!("Cancel"),
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
        "count-immune" => {
            let class = class_by_index(&core.state(), n(args, 0));
            core.state().settings.immune_class = class;
            host.save_settings();
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
                    title: tr!("Dialog title" => "Clear counts"),
                    message: tr!(
                        // Translators: the question of the "Clear counts" dialog, whose
                        // button is "Clear": use the same verb.
                        "Clear the {n} count point of this slide? You can undo this."
                            | "Clear all {n} count points of this slide? You can undo this."
                                % count
                    ),
                    // Translators: the button that removes the count points, in the
                    // "Clear counts" dialog.
                    confirm_label: tr!("Clear"),
                    cancel_label: tr!("Cancel"),
                    destructive: true,
                },
                Pending::ClearCounts { file_id },
            );
        }
        "fields-select" => actions::select_fields(core, need_file()?),
        "field-config" => {
            let mode = n(args, 0) as i32;
            // As typed: with the language's decimal separator, `.` or `,`.
            let parse = |v: &str| i18n::parse_decimal(v);
            let field = if mode == 1 {
                FieldSize::Area {
                    mm2: parse(&s(args, 3)).ok_or_else(|| {
                        tr!(
                            "Enter the field area in mm², e.g. {}.",
                            i18n::format_decimal(0.237, 3)
                        )
                    })?,
                }
            } else {
                FieldSize::FieldNumber {
                    field_number: parse(&s(args, 1))
                        .ok_or_else(|| tr!("Enter the field number in mm, e.g. 22."))?,
                    objective: parse(&s(args, 2))
                        .ok_or_else(|| tr!("Enter the objective magnification, e.g. 40."))?,
                }
            };
            if field.area_mm2().is_none() {
                return Err(tr!("That field size is not usable."));
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
                .ok_or_else(|| tr!("There is no database from before eov 1.0 to merge."))?;
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
                    .ok_or_else(|| tr!("That slide is not listed any more."))?;
                (
                    orphan.record.filename.clone(),
                    orphan.record.width > 0
                        && target.is_some_and(|(w, h)| {
                            (w, h) != (orphan.record.width, orphan.record.height)
                        }),
                )
            };
            let mut message = if title.is_empty() {
                tr!(
                    "Attach the annotations of an older slide to the open slide? Use this when the slide file was re-saved, \
                     converted or de-identified. You can undo this."
                )
            } else {
                tr!(
                    "Attach the annotations of \"{}\" to the open slide? Use this when the slide file was re-saved, \
                     converted or de-identified. You can undo this.",
                    i18n::isolate(&title)
                )
            };
            if dims_differ {
                message = i18n::join_sentences([
                    message,
                    tr!(
                        "Warning: the two slides have different sizes, so the annotations may not line up."
                    ),
                ]);
            }
            confirm(
                core,
                ConfirmArgs {
                    title: tr!("Attach annotations"),
                    message,
                    confirm_label: tr!("Attach"),
                    cancel_label: tr!("Cancel"),
                    destructive: dims_differ,
                },
                Pending::Relink { file_id, from },
            );
        }
        "setting-text" => {
            let (key, value) = (s(args, 0), s(args, 1));
            let value = clean_name(&value, Field::Setting, true)?;
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
                    "brush-eraser" => {
                        state.settings.brush_eraser = value;
                        false
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
            let restyle = {
                let mut state = core.state();
                match key.as_str() {
                    "point-radius" => {
                        state.settings.point_radius = value.clamp(1.0, 20.0);
                        true
                    }
                    "fill-opacity" => {
                        state.settings.fill_opacity = value.clamp(0.0, 1.0);
                        true
                    }
                    "brush-size" => {
                        let sizes = crate::state::BRUSH_SIZES;
                        state.settings.brush_size =
                            value.round().clamp(*sizes.start(), *sizes.end());
                        false
                    }
                    other => return Err(format!("unknown setting '{other}'")),
                }
            };
            host.save_settings();
            if restyle {
                restyle_all(core);
            } else {
                actions::refresh_brush(core);
            }
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

/// Rebuilds every object after a display setting (or the language of
/// their tooltips) changed.
pub fn restyle_all(core: &Arc<Core>) {
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
        out.push_str(&a.shown_name());
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

    #[test]
    fn names_compare_as_people_read_them() {
        let mut names = vec![
            "Region 10",
            "region 2",
            "Region 02",
            "Region 1",
            "point",
            "Point",
            "Region",
            "Area 9b",
            "Area 9a",
        ];
        names.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
        assert_eq!(
            names,
            [
                "Area 9a",
                "Area 9b",
                "Point",
                "point",
                "Region",
                "Region 1",
                "region 2",
                "Region 02",
                "Region 10",
            ]
        );
    }
}
