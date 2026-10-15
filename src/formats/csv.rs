//! Measurements as CSV (one row per annotation).

use super::eov::ExportSlide;
use crate::measure::{Calibration, measure};
use crate::model::Class;

/// Quotes a field when needed and defuses spreadsheet formulas (a value
/// starting with `=`, `+`, `-` or `@` is prefixed with `'`).
pub fn field(value: &str) -> String {
    let value = if value.starts_with(['=', '+', '-', '@']) && value.parse::<f64>().is_err() {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

fn number(value: Option<f64>) -> String {
    value.map(|v| format!("{v:.4}")).unwrap_or_default()
}

pub const HEADER: &[&str] = &[
    "layer",
    "annotation_id",
    "name",
    "class",
    "kind",
    "status",
    "author",
    "created_at",
    "updated_at",
    "centroid_x_px",
    "centroid_y_px",
    "area_um2",
    "perimeter_or_length_um",
    "max_feret_um",
    "min_caliper_um",
    "area_px2",
    "perimeter_or_length_px",
    "calibration",
];

pub fn export(slide: &ExportSlide, classes: &[Class]) -> String {
    let calibration = Calibration::new(slide.record.mpp_x, slide.record.mpp_y);
    let calibration_text = match calibration.spacing() {
        Some((_, _, false)) => "calibrated",
        Some((_, _, true)) => "approximate",
        None => "uncalibrated",
    };
    let mut out = HEADER.join(",");
    out.push('\n');
    for (layer, annotations) in &slide.layers {
        for a in annotations {
            let m = measure(&a.geometry, &calibration);
            let class = a
                .class_id
                .as_deref()
                .and_then(|id| classes.iter().find(|c| c.id == id))
                .map(|c| c.name.as_str())
                .unwrap_or("");
            let row = [
                field(&layer.name),
                field(&a.id),
                field(&a.name),
                field(class),
                a.geometry.kind().as_str().to_string(),
                a.status.as_str().to_string(),
                field(&a.author),
                super::iso8601(a.created_at),
                super::iso8601(a.updated_at),
                format!("{:.2}", m.centroid[0]),
                format!("{:.2}", m.centroid[1]),
                number(m.area_um2),
                number(m.length_um),
                number(m.feret_um),
                number(m.min_caliper_um),
                number(m.area_px2),
                number(m.length_px),
                calibration_text.to_string(),
            ];
            out.push_str(&row.join(","));
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_escaped_and_formulas_defused() {
        assert_eq!(field("plain"), "plain");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("=HYPERLINK(1)"), "'=HYPERLINK(1)");
        assert_eq!(field("-12.5"), "-12.5");
        assert_eq!(field("@cmd"), "'@cmd");
    }
}
