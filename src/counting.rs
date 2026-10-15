//! Cell counting: tallies per class, density per mm² and per HPF with exact
//! intervals, and a two-class index (e.g. Ki-67 % positive).
//!
//! The counting region is the union of the selected area annotations
//! (polygons, rectangles, ellipses, HPF fields) of the slide; with nothing
//! selected the whole slide is counted and no density is given. Counts are
//! recomputed from the current data on every refresh, so they follow edits
//! immediately (ANN-2), and areas use the same even-odd rule as the
//! containment test (ANN-3).

use crate::geometry::Region;
use crate::measure::Calibration;
use crate::model::{Color, Geometry, LayerRole};
use crate::state::{SlideData, State};
use crate::stats::{FieldSize, binomial_interval, poisson_interval};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct TallyRow {
    pub class_id: Option<String>,
    pub name: String,
    pub color: Color,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tally {
    pub rows: Vec<TallyRow>,
    /// Number of regions counted in (0: whole slide).
    pub regions: usize,
    /// How many of them are HPF fields.
    pub fields: usize,
    pub area_px: f64,
    pub area_mm2: Option<f64>,
    pub approximate: bool,
    /// Points not inside any visible layer were skipped.
    pub total: u64,
}

/// Counts points per class in the selection of `file_id` (or the whole
/// slide).
pub fn tally(state: &State, slide: &SlideData, selection: &[String]) -> Tally {
    let include_hidden = state.settings.count_hidden_layers;
    let region_parts: Vec<(Geometry, bool)> = selection
        .iter()
        .filter_map(|id| slide.annotations.get(id))
        .filter(|a| a.geometry.kind().is_area())
        .map(|a| {
            let is_field = slide
                .layer(&a.layer_id)
                .is_some_and(|l| l.role == LayerRole::Fields);
            (a.geometry.clone(), is_field)
        })
        .collect();
    let fields = region_parts.iter().filter(|(_, f)| *f).count();
    let region = Region::new(region_parts.into_iter().map(|(g, _)| g).collect());
    let bounds = region.bounds();
    let mut counts: HashMap<Option<String>, u64> = HashMap::new();
    for layer in &slide.layers {
        if !layer.visible && !include_hidden {
            continue;
        }
        for a in slide.in_layer(&layer.id) {
            let Geometry::Point { x, y } = a.geometry else {
                continue;
            };
            if !region.is_empty() && !(bounds.contains(x, y) && region.contains([x, y])) {
                continue;
            }
            *counts.entry(a.class_id.clone()).or_default() += 1;
        }
    }
    let mut rows: Vec<TallyRow> = state
        .classes
        .iter()
        .filter(|c| {
            counts.contains_key(&Some(c.id.clone()))
                || state.settings.active_class.as_deref() == Some(c.id.as_str())
                || state.settings.alternate_class.as_deref() == Some(c.id.as_str())
        })
        .map(|c| TallyRow {
            class_id: Some(c.id.clone()),
            name: c.name.clone(),
            color: c.color,
            count: counts.get(&Some(c.id.clone())).copied().unwrap_or(0),
        })
        .collect();
    // Points of deleted classes count as unclassified.
    let known: u64 = rows.iter().map(|r| r.count).sum();
    let total: u64 = counts.values().sum();
    if total > known {
        rows.push(TallyRow {
            class_id: None,
            name: "Unclassified".to_string(),
            color: Color(0xCC, 0xCC, 0xCC),
            count: total - known,
        });
    }
    let calibration = Calibration::new(slide.record.mpp_x, slide.record.mpp_y);
    let area_px = region.area_px();
    let area_mm2 = (!region.is_empty())
        .then(|| calibration.mm2_per_px2().map(|k| area_px * k))
        .flatten();
    Tally {
        rows,
        regions: region.len(),
        fields,
        area_px,
        area_mm2,
        approximate: calibration.spacing().is_some_and(|(_, _, approx)| approx),
        total,
    }
}

/// Lines describing a class's count in the region.
pub fn density_text(count: u64, area_mm2: f64, field: &FieldSize) -> (String, String) {
    let (lo, hi) = poisson_interval(count);
    let per_mm2 = count as f64 / area_mm2;
    let density = format!(
        "{} /mm² (95% {}–{})",
        short(per_mm2),
        short(lo / area_mm2),
        short(hi / area_mm2)
    );
    let per_hpf = field
        .area_mm2()
        .map(|hpf| format!("{} /10 HPF", short(per_mm2 * 10.0 * hpf)))
        .unwrap_or_default();
    (density, per_hpf)
}

/// `k / (k + m)` as a percentage with its exact interval.
pub fn index_text(numerator: &str, denominator: &str, k: u64, m: u64) -> Option<String> {
    let n = k + m;
    let (lo, hi) = binomial_interval(k, n)?;
    Some(format!(
        "{numerator} index: {:.1} % (95% {:.1}–{:.1} %) — {k} of {n} ({numerator} + {denominator})",
        100.0 * k as f64 / n as f64,
        100.0 * lo,
        100.0 * hi
    ))
}

fn short(value: f64) -> String {
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// Points inside a circle-ish field (the HPF reticle).
pub fn count_in(slide: &SlideData, field: &Geometry) -> u64 {
    let b = field.bounds();
    slide
        .layers
        .iter()
        .filter(|l| l.visible)
        .flat_map(|l| slide.in_layer(&l.id))
        .filter(|a| match a.geometry {
            Geometry::Point { x, y } => b.contains(x, y) && crate::geometry::contains(field, [x, y]),
            _ => false,
        })
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Annotation, Class, Layer, SlideRecord, Status};
    use crate::state::Settings;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn class(id: &str, name: &str) -> Class {
        Class {
            id: id.into(),
            name: name.into(),
            color: Color(1, 1, 1),
            hotkey: None,
            visible: true,
            opacity: 1.0,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn layer(id: &str, role: LayerRole) -> Layer {
        Layer {
            id: id.into(),
            slide_id: 1,
            name: id.into(),
            notes: String::new(),
            color: Color(0, 0, 0),
            visible: true,
            opacity: 1.0,
            locked: false,
            role,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn ann(id: &str, layer: &str, class: Option<&str>, geometry: Geometry) -> Arc<Annotation> {
        Arc::new(Annotation {
            id: id.into(),
            layer_id: layer.into(),
            class_id: class.map(str::to_string),
            geometry,
            name: String::new(),
            description: String::new(),
            status: Status::Draft,
            author: String::new(),
            locked: false,
            properties: vec![],
            created_at: 0,
            updated_at: 0,
        })
    }

    fn setup() -> (State, SlideData) {
        let mut state = State::new(Settings::default());
        state.classes = vec![class("pos", "Positive"), class("neg", "Negative")];
        state.settings.active_class = Some("pos".into());
        state.settings.alternate_class = Some("neg".into());
        // 1000 px square at 0.5 µm/px = 0.25 mm².
        let region = ann(
            "r",
            "regions",
            None,
            Geometry::Polygon {
                exterior: vec![[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]],
                holes: vec![],
            },
        );
        let mut annotations = vec![region];
        for i in 0..30 {
            annotations.push(ann(&format!("p{i}"), "counts", Some("pos"), Geometry::Point { x: 10.0 + i as f64, y: 10.0 }));
        }
        for i in 0..10 {
            annotations.push(ann(&format!("n{i}"), "counts", Some("neg"), Geometry::Point { x: 10.0 + i as f64, y: 20.0 }));
        }
        // Outside the region.
        annotations.push(ann("out", "counts", Some("pos"), Geometry::Point { x: 5000.0, y: 5000.0 }));
        let slide = SlideData::new(
            SlideRecord {
                id: 1,
                mpp_x: Some(0.5),
                mpp_y: Some(0.5),
                ..Default::default()
            },
            vec![layer("regions", LayerRole::Normal), layer("counts", LayerRole::Counts)],
            annotations,
            HashSet::new(),
            None,
        );
        (state, slide)
    }

    #[test]
    fn counts_inside_the_selected_region_with_density() {
        let (state, slide) = setup();
        let t = tally(&state, &slide, &["r".to_string()]);
        assert_eq!(t.regions, 1);
        assert!((t.area_mm2.unwrap() - 0.25).abs() < 1e-12);
        let pos = t.rows.iter().find(|r| r.name == "Positive").unwrap();
        assert_eq!(pos.count, 30);
        let (density, per_hpf) = density_text(pos.count, t.area_mm2.unwrap(), &FieldSize::default());
        assert!(density.starts_with("120 /mm²"), "{density}");
        assert!(per_hpf.contains("/10 HPF"));
        let index = index_text("Positive", "Negative", 30, 10).unwrap();
        assert!(index.contains("75.0 %"), "{index}");
    }

    #[test]
    fn whole_slide_without_selection_and_hidden_layers_excluded() {
        let (mut state, mut slide) = setup();
        let t = tally(&state, &slide, &[]);
        assert_eq!(t.regions, 0);
        assert_eq!(t.area_mm2, None);
        assert_eq!(t.rows.iter().find(|r| r.name == "Positive").unwrap().count, 31);
        slide.layers.iter_mut().find(|l| l.id == "counts").unwrap().visible = false;
        let hidden = tally(&state, &slide, &[]);
        assert_eq!(hidden.total, 0);
        state.settings.count_hidden_layers = true;
        assert_eq!(tally(&state, &slide, &[]).total, 41);
    }

    #[test]
    fn reticle_counts_points_in_the_field() {
        let (_, slide) = setup();
        let field = Geometry::Ellipse {
            cx: 15.0,
            cy: 15.0,
            rx: 6.0,
            ry: 6.0,
        };
        assert!(count_in(&slide, &field) > 0);
    }
}
