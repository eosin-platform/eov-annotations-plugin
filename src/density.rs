//! How many points of each layer fall inside the selected polygon, and how
//! many that is per mm² of it.
//!
//! The polygon area, the point-in-polygon test and the Poisson interval come
//! from the cellaris area and mitosis counter, by the same author.

use statrs::distribution::{ChiSquared, ContinuousCDF};

use crate::model::{Annotation, PolygonAnnotation};
use crate::state::{PluginState, active_file_from_snapshot, host_snapshot};

/// One microscope field at field number 22 with a 40x objective. Field size
/// varies between microscopes, so the per-10-HPF line names this one.
const FIELD_NUMBER: f64 = 22.0;
const OBJECTIVE: f64 = 40.0;

fn hpf_area_mm2() -> f64 {
    let diameter_mm = FIELD_NUMBER / OBJECTIVE;
    std::f64::consts::PI * (diameter_mm / 2.0).powi(2)
}

/// Shoelace formula, so a concave outline is measured correctly.
fn polygon_area_px(vertices: &[(f64, f64)]) -> f64 {
    if vertices.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for (i, &(x1, y1)) in vertices.iter().enumerate() {
        let (x2, y2) = vertices[(i + 1) % vertices.len()];
        sum += x1 * y2 - x2 * y1;
    }
    sum.abs() / 2.0
}

fn px2_to_mm2(area_px: f64, mpp_x: f64, mpp_y: f64) -> f64 {
    area_px * mpp_x * mpp_y / 1_000_000.0
}

/// Even-odd rule.
fn inside(x: f64, y: f64, vertices: &[(f64, f64)]) -> bool {
    if vertices.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = vertices.len() - 1;
    for i in 0..vertices.len() {
        let (xi, yi) = vertices[i];
        let (xj, yj) = vertices[j];
        if (yi > y) != (yj > y) && x < xi + (y - yi) * (xj - xi) / (yj - yi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Exact 95% range for a count, which is wide when the count is small.
fn poisson_interval(k: u32) -> (f64, f64) {
    let alpha = 0.05;
    let upper = ChiSquared::new(2.0 * (k as f64 + 1.0))
        .map(|dist| 0.5 * dist.inverse_cdf(1.0 - alpha / 2.0))
        .unwrap_or(0.0);
    if k == 0 {
        return (0.0, upper);
    }
    let lower = ChiSquared::new(2.0 * k as f64)
        .map(|dist| 0.5 * dist.inverse_cdf(alpha / 2.0))
        .unwrap_or(0.0);
    (lower, upper)
}

fn vertices_of(polygon: &PolygonAnnotation) -> Vec<(f64, f64)> {
    polygon
        .vertices
        .iter()
        .map(|vertex| (vertex.x_level0, vertex.y_level0))
        .collect()
}

/// One line per layer that has points inside the region.
pub(crate) fn lines(state: &PluginState, mpp: Option<(f64, f64)>) -> Vec<String> {
    let Some(path) = state.active_file_path.as_deref() else {
        return Vec::new();
    };
    let Some(loaded) = state.files.get(path) else {
        return Vec::new();
    };
    let Some(selected) = state.selected_annotation_by_file.get(path) else {
        return vec!["Select a polygon to measure the layers inside it.".to_string()];
    };
    let region = loaded.annotation_layers.iter().find_map(|layer| {
        layer
            .annotations
            .iter()
            .find_map(|annotation| match annotation {
                Annotation::Polygon(polygon) if &polygon.id == selected => {
                    Some(vertices_of(polygon))
                }
                _ => None,
            })
    });
    let Some(region) = region else {
        return vec!["Select a polygon to measure the layers inside it.".to_string()];
    };

    let Some((mpp_x, mpp_y)) = mpp.filter(|&(x, y)| x > 0.0 && y > 0.0) else {
        return vec!["This slide has no pixel size, so no area can be measured.".to_string()];
    };
    let area_mm2 = px2_to_mm2(polygon_area_px(&region), mpp_x, mpp_y);
    if area_mm2 <= 0.0 {
        return vec!["That polygon encloses no area.".to_string()];
    }

    let mut out = vec![format!("Region: {area_mm2:.4} mm²")];
    let hpf = hpf_area_mm2();
    for layer in &loaded.annotation_layers {
        let count = layer
            .annotations
            .iter()
            .filter(|annotation| match annotation {
                Annotation::Point(point) => inside(point.x_level0, point.y_level0, &region),
                Annotation::Polygon(_) => false,
            })
            .count() as u32;
        if count == 0 {
            continue;
        }
        let (lo, hi) = poisson_interval(count);
        out.push(format!(
            "{}: {} points, {:.2} / mm² ({:.2}-{:.2}), {:.1} / 10 HPF",
            layer.name,
            count,
            count as f64 / area_mm2,
            lo / area_mm2,
            hi / area_mm2,
            count as f64 / area_mm2 * 10.0 * hpf,
        ));
    }
    if out.len() == 1 {
        out.push("No points of any layer are inside it.".to_string());
    } else {
        out.push(format!(
            "10 HPF assumes field number {FIELD_NUMBER:.0} at {OBJECTIVE:.0}x, {hpf:.4} mm²."
        ));
    }
    out
}

/// The sidebar's text, with the slide's pixel size read from the host.
pub(crate) fn summary(state: &PluginState) -> String {
    let mpp = host_snapshot()
        .ok()
        .as_ref()
        .and_then(active_file_from_snapshot)
        .and_then(|file| {
            let x = file.mpp_x.into_option()?;
            let y = file.mpp_y.into_option().unwrap_or(x);
            Some((x, y))
        });
    lines(state, mpp).join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnnotationLayer, LoadedFileAnnotations, PointAnnotation, PolygonVertex};

    fn point(id: &str, x: f64, y: f64) -> Annotation {
        Annotation::Point(PointAnnotation {
            id: id.to_string(),
            created_at: 0,
            updated_at: 0,
            x_level0: x,
            y_level0: y,
            metadata: Vec::new(),
        })
    }

    fn square(id: &str, side: f64) -> Annotation {
        Annotation::Polygon(PolygonAnnotation {
            id: id.to_string(),
            created_at: 0,
            updated_at: 0,
            vertices: [(0.0, 0.0), (side, 0.0), (side, side), (0.0, side)]
                .into_iter()
                .map(|(x_level0, y_level0)| PolygonVertex { x_level0, y_level0 })
                .collect(),
            metadata: Vec::new(),
        })
    }

    fn layer(name: &str, annotations: Vec<Annotation>) -> AnnotationLayer {
        AnnotationLayer {
            id: name.to_string(),
            name: name.to_string(),
            notes: None,
            color_hex: "#FFFFFF".to_string(),
            created_at: 0,
            updated_at: 0,
            annotations,
        }
    }

    /// A 1000px square at 0.325 um/px is 0.105625 mm².
    fn state_with(selected: &str) -> PluginState {
        let mut state = PluginState::default();
        let path = "E:/slides/case-1.svs".to_string();
        state.active_file_path = Some(path.clone());
        state
            .selected_annotation_by_file
            .insert(path.clone(), selected.to_string());
        state.files.insert(
            path.clone(),
            LoadedFileAnnotations {
                file_path: path,
                filename: "case-1.svs".to_string(),
                fingerprint: [0u8; 32],
                annotation_layers: vec![
                    layer("Regions", vec![square("region", 1000.0)]),
                    layer(
                        "Mitoses",
                        vec![
                            point("a", 100.0, 100.0),
                            point("b", 900.0, 900.0),
                            point("c", 2000.0, 100.0),
                        ],
                    ),
                    layer("Empty", vec![point("d", 5000.0, 5000.0)]),
                ],
            },
        );
        state
    }

    #[test]
    fn counts_only_the_points_inside_the_selected_polygon() {
        let lines = lines(&state_with("region"), Some((0.325, 0.325)));

        assert!(lines[0].contains("0.1056 mm²"), "{}", lines[0]);
        let mitoses = lines
            .iter()
            .find(|line| line.starts_with("Mitoses"))
            .unwrap();
        assert!(mitoses.contains("2 points"), "{mitoses}");
        assert!(mitoses.contains("18.93 / mm²"), "{mitoses}");
        assert!(
            !lines.iter().any(|line| line.starts_with("Empty")),
            "a layer with nothing inside was listed"
        );
    }

    #[test]
    fn nothing_selected_or_no_pixel_size_says_so() {
        let mut state = state_with("region");
        state.selected_annotation_by_file.clear();
        assert!(lines(&state, Some((0.325, 0.325)))[0].contains("Select a polygon"));

        let point_selected = state_with("a");
        assert!(lines(&point_selected, Some((0.325, 0.325)))[0].contains("Select a polygon"));

        assert!(lines(&state_with("region"), None)[0].contains("no pixel size"));
    }

    #[test]
    fn the_maths_matches_the_counter() {
        let square = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 4.0),
            (4.0, 4.0),
            (4.0, 10.0),
            (0.0, 10.0),
        ];
        assert_eq!(polygon_area_px(&square), 64.0);
        assert!(inside(2.0, 8.0, &square) && !inside(8.0, 8.0, &square));
        assert!((hpf_area_mm2() - 0.2376).abs() < 1e-4);

        let (lo, hi) = poisson_interval(10);
        assert!((lo - 4.795).abs() < 1e-3 && (hi - 18.390).abs() < 1e-3);
        assert_eq!(poisson_interval(0).0, 0.0);
    }
}
