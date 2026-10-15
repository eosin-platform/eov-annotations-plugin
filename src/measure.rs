//! Physical measurements of annotations.
//!
//! Lengths and areas are computed in micrometre space: every vertex is
//! scaled by the slide's pixel spacing `(mpp_x, mpp_y)` first, so
//! anisotropic pixels are handled exactly. With one axis missing the other
//! is assumed for both and the value is marked approximate; with no spacing
//! at all only pixel values are reported.

use crate::geometry::{self, P};
use crate::model::Geometry;

/// Pixel spacing of a slide in µm per level-0 pixel.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Calibration {
    pub mpp_x: Option<f64>,
    pub mpp_y: Option<f64>,
}

impl Calibration {
    pub fn new(mpp_x: Option<f64>, mpp_y: Option<f64>) -> Self {
        let valid = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
        Self {
            mpp_x: valid(mpp_x),
            mpp_y: valid(mpp_y),
        }
    }

    /// `(mpp_x, mpp_y, approximate)`, or `None` without any spacing.
    pub fn spacing(&self) -> Option<(f64, f64, bool)> {
        match (self.mpp_x, self.mpp_y) {
            (Some(x), Some(y)) => Some((x, y, false)),
            (Some(v), None) | (None, Some(v)) => Some((v, v, true)),
            (None, None) => None,
        }
    }

    pub fn is_anisotropic(&self) -> bool {
        matches!(self.spacing(), Some((x, y, _)) if (x - y).abs() > 1e-9 * x.max(y))
    }

    /// Converts µm to level-0 pixels along x and y.
    pub fn um_to_px(&self, um: f64) -> Option<(f64, f64)> {
        self.spacing().map(|(x, y, _)| (um / x, um / y))
    }

    /// mm² per px², or `None` when uncalibrated.
    pub fn mm2_per_px2(&self) -> Option<f64> {
        self.spacing().map(|(x, y, _)| x * y / 1_000_000.0)
    }
}

/// Measurements of one annotation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Measurements {
    /// Level-0 pixel values.
    pub area_px2: Option<f64>,
    pub length_px: Option<f64>,
    /// Physical values (µm, µm²), when the slide is calibrated.
    pub area_um2: Option<f64>,
    /// Perimeter of closed shapes (including hole outlines), length of lines.
    pub length_um: Option<f64>,
    /// Largest distance between two points of the outline (max Feret).
    pub feret_um: Option<f64>,
    /// Smallest width over all directions (min caliper / min Feret).
    pub min_caliper_um: Option<f64>,
    pub feret_px: Option<f64>,
    pub min_caliper_px: Option<f64>,
    pub centroid: P,
    /// One spacing axis was missing.
    pub approximate: bool,
    pub calibrated: bool,
}

fn scale(points: &[P], sx: f64, sy: f64) -> Vec<P> {
    points.iter().map(|p| [p[0] * sx, p[1] * sy]).collect()
}

fn ellipse_perimeter(a: f64, b: f64) -> f64 {
    let (a, b) = (a.abs(), b.abs());
    if a + b <= 0.0 {
        return 0.0;
    }
    let h = ((a - b) / (a + b)).powi(2);
    std::f64::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()))
}

/// Geometry-only values in a coordinate space scaled by `(sx, sy)`:
/// `(area, length, feret, min caliper)`.
fn scaled_values(
    geometry: &Geometry,
    sx: f64,
    sy: f64,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    match geometry {
        Geometry::Point { .. } | Geometry::Text { .. } => (None, None, None, None),
        Geometry::Polyline { points } => {
            let pts = scale(points, sx, sy);
            let (feret, _) = geometry::calipers(&pts);
            (
                None,
                Some(geometry::polyline_length(&pts)),
                Some(feret),
                None,
            )
        }
        Geometry::Polygon { exterior, holes } => {
            let ext = scale(exterior, sx, sy);
            let hs: Vec<Vec<P>> = holes.iter().map(|h| scale(h, sx, sy)).collect();
            let area = geometry::area(&Geometry::Polygon {
                exterior: ext.clone(),
                holes: hs.clone(),
            });
            let perimeter = geometry::ring_perimeter(&ext)
                + hs.iter().map(|h| geometry::ring_perimeter(h)).sum::<f64>();
            let (feret, min) = geometry::calipers(&ext);
            (Some(area), Some(perimeter), Some(feret), Some(min))
        }
        Geometry::Rectangle { width, height, .. } => {
            let (w, h) = (width.abs() * sx, height.abs() * sy);
            (
                Some(w * h),
                Some(2.0 * (w + h)),
                Some(w.hypot(h)),
                Some(w.min(h)),
            )
        }
        Geometry::Ellipse { rx, ry, .. } => {
            // Axis-aligned scaling keeps an axis-aligned ellipse.
            let (a, b) = (rx.abs() * sx, ry.abs() * sy);
            (
                Some(std::f64::consts::PI * a * b),
                Some(ellipse_perimeter(a, b)),
                Some(2.0 * a.max(b)),
                Some(2.0 * a.min(b)),
            )
        }
    }
}

/// Measures a geometry.
pub fn measure(geometry: &Geometry, calibration: &Calibration) -> Measurements {
    let (area_px2, length_px, feret_px, min_caliper_px) = scaled_values(geometry, 1.0, 1.0);
    let mut m = Measurements {
        area_px2,
        length_px,
        feret_px,
        min_caliper_px,
        centroid: centroid(geometry),
        ..Default::default()
    };
    if let Some((sx, sy, approximate)) = calibration.spacing() {
        let (area, length, feret, min) = scaled_values(geometry, sx, sy);
        m.area_um2 = area;
        m.length_um = length;
        m.feret_um = feret;
        m.min_caliper_um = min;
        m.approximate = approximate;
        m.calibrated = true;
    }
    m
}

/// Area centroid of closed shapes, vertex mean of lines, the point itself
/// otherwise.
pub fn centroid(geometry: &Geometry) -> P {
    match geometry {
        Geometry::Polygon { exterior, holes } => {
            let ring_centroid = |ring: &[P]| -> (f64, P) {
                let a = geometry::ring_signed_area(ring);
                if a.abs() <= f64::EPSILON {
                    return (0.0, [0.0, 0.0]);
                }
                let o = ring[0];
                let (mut cx, mut cy) = (0.0, 0.0);
                for i in 0..ring.len() {
                    let p = [ring[i][0] - o[0], ring[i][1] - o[1]];
                    let q = [
                        ring[(i + 1) % ring.len()][0] - o[0],
                        ring[(i + 1) % ring.len()][1] - o[1],
                    ];
                    let cross = p[0] * q[1] - q[0] * p[1];
                    cx += (p[0] + q[0]) * cross;
                    cy += (p[1] + q[1]) * cross;
                }
                (a.abs(), [cx / (6.0 * a) + o[0], cy / (6.0 * a) + o[1]])
            };
            let (outer_area, outer_c) = ring_centroid(exterior);
            let mut total = outer_area;
            let mut cx = outer_c[0] * outer_area;
            let mut cy = outer_c[1] * outer_area;
            for hole in holes {
                let (a, c) = ring_centroid(hole);
                total -= a;
                cx -= c[0] * a;
                cy -= c[1] * a;
            }
            if total > f64::EPSILON {
                [cx / total, cy / total]
            } else {
                geometry.bounds().center()
            }
        }
        Geometry::Polyline { points } => {
            let n = points.len().max(1) as f64;
            let (sx, sy) = points
                .iter()
                .fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
            [sx / n, sy / n]
        }
        other => other.anchor(),
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Formats `value` with about four significant digits.
pub fn significant(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return "0".to_string();
    }
    let magnitude = value.abs().log10().floor() as i32;
    let decimals = (3 - magnitude).clamp(0, 6) as usize;
    format!("{value:.decimals$}")
}

/// A length in µm, switching to mm above 1000 µm.
pub fn format_length_um(um: f64) -> String {
    if um.abs() >= 1000.0 {
        format!("{} mm", significant(um / 1000.0))
    } else {
        format!("{} µm", significant(um))
    }
}

/// An area in µm², switching to mm² above 0.01 mm² (so cell-sized regions
/// are not shown as "0.0000 mm²", ANN-18).
pub fn format_area_um2(um2: f64) -> String {
    if um2.abs() >= 10_000.0 {
        format!("{} mm²", significant(um2 / 1_000_000.0))
    } else {
        format!("{} µm²", significant(um2))
    }
}

pub fn format_px(px: f64) -> String {
    format!("{} px", significant(px))
}

pub fn format_px2(px2: f64) -> String {
    format!("{} px²", significant(px2))
}

/// `(label, value)` rows for the details panel.
pub fn rows(m: &Measurements, kind_is_line: bool) -> Vec<(String, String)> {
    let approx = if m.approximate { "≈ " } else { "" };
    let mut rows = Vec::new();
    let both = |um: Option<f64>, px: Option<f64>, area: bool| -> Option<String> {
        match (um, px) {
            (Some(um), _) if m.calibrated => Some(format!(
                "{approx}{}",
                if area {
                    format_area_um2(um)
                } else {
                    format_length_um(um)
                }
            )),
            (_, Some(px)) => Some(if area { format_px2(px) } else { format_px(px) }),
            _ => None,
        }
    };
    if let Some(v) = both(m.area_um2, m.area_px2, true) {
        rows.push(("Area".to_string(), v));
    }
    if let Some(v) = both(m.length_um, m.length_px, false) {
        rows.push((
            if kind_is_line { "Length" } else { "Perimeter" }.to_string(),
            v,
        ));
    }
    if let Some(v) = both(m.feret_um, m.feret_px, false) {
        rows.push(("Max diameter (Feret)".to_string(), v));
    }
    if let Some(v) = both(m.min_caliper_um, m.min_caliper_px, false) {
        rows.push(("Min caliper".to_string(), v));
    }
    rows.push((
        "Centre (px)".to_string(),
        format!("{:.1}, {:.1}", m.centroid[0], m.centroid[1]),
    ));
    if !m.calibrated && (m.area_px2.is_some() || m.length_px.is_some()) {
        rows.push((
            "Calibration".to_string(),
            "No pixel size: values in level-0 pixels".to_string(),
        ));
    } else if m.approximate {
        rows.push((
            "Calibration".to_string(),
            "One pixel axis missing; assumed square pixels".to_string(),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anisotropic_spacing_is_exact() {
        // A 100×100 px square with 0.5 µm/px horizontally and 0.25 µm/px
        // vertically is 50 µm × 25 µm.
        let g = Geometry::Polygon {
            exterior: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
            holes: vec![],
        };
        let m = measure(&g, &Calibration::new(Some(0.5), Some(0.25)));
        assert!((m.area_um2.unwrap() - 1250.0).abs() < 1e-9);
        assert!((m.length_um.unwrap() - 150.0).abs() < 1e-9);
        assert!((m.feret_um.unwrap() - (50f64.hypot(25.0))).abs() < 1e-9);
        assert!((m.min_caliper_um.unwrap() - 25.0).abs() < 1e-9);
        assert!(!m.approximate && m.calibrated);
    }

    #[test]
    fn missing_spacing_is_explicit() {
        let g = Geometry::Rectangle {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 20.0,
        };
        let none = measure(&g, &Calibration::new(None, None));
        assert_eq!(none.area_um2, None);
        assert_eq!(none.area_px2, Some(200.0));
        assert!(!none.calibrated);
        let rows = rows(&none, false);
        assert!(rows.iter().any(|(_, v)| v.contains("level-0 pixels")));
        let half = measure(&g, &Calibration::new(Some(0.5), None));
        assert!(half.approximate);
        assert_eq!(half.area_um2, Some(50.0));
        // Invalid values are ignored.
        assert_eq!(Calibration::new(Some(-1.0), Some(f64::NAN)).spacing(), None);
    }

    #[test]
    fn ellipse_and_line_measurements() {
        let e = Geometry::Ellipse {
            cx: 0.0,
            cy: 0.0,
            rx: 10.0,
            ry: 10.0,
        };
        let m = measure(&e, &Calibration::new(Some(1.0), Some(2.0)));
        assert!((m.area_um2.unwrap() - std::f64::consts::PI * 200.0).abs() < 1e-9);
        assert_eq!(m.feret_um, Some(40.0));
        assert_eq!(m.min_caliper_um, Some(20.0));
        let line = Geometry::Polyline {
            points: vec![[0.0, 0.0], [3.0, 0.0], [3.0, 4.0]],
        };
        let m = measure(&line, &Calibration::new(Some(1.0), Some(1.0)));
        assert_eq!(m.length_um, Some(7.0));
        assert_eq!(m.feret_um, Some(5.0));
        assert_eq!(m.area_um2, None);
    }

    #[test]
    fn units_switch_for_readability() {
        assert_eq!(format_area_um2(123.4), "123.4 µm²");
        assert_eq!(format_area_um2(850_000.0), "0.8500 mm²");
        assert_eq!(format_length_um(12.345), "12.35 µm");
        assert_eq!(format_length_um(2500.0), "2.500 mm");
        assert_eq!(significant(0.0), "0");
    }

    #[test]
    fn polygon_centroid() {
        let g = Geometry::Polygon {
            exterior: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            holes: vec![],
        };
        let c = centroid(&g);
        assert!((c[0] - 5.0).abs() < 1e-9 && (c[1] - 5.0).abs() < 1e-9);
    }
}
