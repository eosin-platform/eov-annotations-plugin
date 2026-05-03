use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub(crate) const SET_COLOR_PALETTE: [&str; 16] = [
    "#FF355E", "#FF7A00", "#39FF14", "#0057FF", "#6C63FF", "#9D4EDD", "#FF4FD8", "#F15BB5",
    "#43AA8B", "#577590", "#FF8A5B", "#FF6B6B", "#C77DFF", "#B8F2E6", "#F4A261", "#FFFFFF",
];

#[derive(Clone)]
pub(crate) struct LoadedFileAnnotations {
    pub(crate) file_path: String,
    pub(crate) filename: String,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) annotation_layers: Vec<AnnotationLayer>,
}

#[derive(Clone)]
pub(crate) struct AnnotationLayer {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) notes: Option<String>,
    pub(crate) color_hex: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) annotations: Vec<Annotation>,
}

#[derive(Clone)]
pub(crate) enum Annotation {
    Point(PointAnnotation),
    Polygon(PolygonAnnotation),
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AnnotationMetadataEntry {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Clone)]
pub(crate) struct PointAnnotation {
    pub(crate) id: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) x_level0: f64,
    pub(crate) y_level0: f64,
    pub(crate) metadata: Vec<AnnotationMetadataEntry>,
}

#[derive(Clone)]
pub(crate) struct PolygonVertex {
    pub(crate) x_level0: f64,
    pub(crate) y_level0: f64,
}

#[derive(Clone)]
pub(crate) struct PolygonAnnotation {
    pub(crate) id: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) vertices: Vec<PolygonVertex>,
    pub(crate) metadata: Vec<AnnotationMetadataEntry>,
}

#[derive(Serialize)]
pub(crate) struct SidebarTreeRow {
    pub(crate) row_id: String,
    pub(crate) parent_layer_id: String,
    pub(crate) label: String,
    pub(crate) annotation_count: i32,
    pub(crate) indent: i32,
    pub(crate) is_layer: bool,
    pub(crate) is_collapsed: bool,
    pub(crate) is_selected: bool,
    pub(crate) visible: bool,
    pub(crate) color_r: i32,
    pub(crate) color_g: i32,
    pub(crate) color_b: i32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct AnnotationExportMetadata {
    pub(crate) author: String,
    pub(crate) organization: String,
    pub(crate) project_name: String,
    pub(crate) license: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ExportFile {
    pub(crate) file_path: String,
    #[serde(alias = "fingerprint_hex")]
    pub(crate) file_sha256: String,
    #[serde(default)]
    pub(crate) metadata: AnnotationExportMetadata,
    pub(crate) annotation_layers: Vec<ExportAnnotationLayer>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ExportAnnotationLayer {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) notes: Option<String>,
    pub(crate) color_hex: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) annotations: Vec<ExportAnnotation>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ExportAnnotation {
    Point {
        id: String,
        created_at: i64,
        updated_at: i64,
        x_level0: f64,
        y_level0: f64,
        #[serde(default)]
        metadata: Vec<AnnotationMetadataEntry>,
    },
    Polygon {
        id: String,
        created_at: i64,
        updated_at: i64,
        vertices: Vec<ExportPolygonVertex>,
        #[serde(default)]
        metadata: Vec<AnnotationMetadataEntry>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ExportPolygonVertex {
    pub(crate) x_level0: f64,
    pub(crate) y_level0: f64,
}

pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn hex_color_to_rgb(color_hex: &str) -> (u8, u8, u8) {
    let hex = color_hex.trim_start_matches('#');
    if hex.len() != 6 {
        return (0xF2, 0xF4, 0xF8);
    }
    let red = u8::from_str_radix(&hex[0..2], 16).ok();
    let green = u8::from_str_radix(&hex[2..4], 16).ok();
    let blue = u8::from_str_radix(&hex[4..6], 16).ok();
    match (red, green, blue) {
        (Some(red), Some(green), Some(blue)) => (red, green, blue),
        _ => (0xF2, 0xF4, 0xF8),
    }
}

fn palette_seed() -> usize {
    let uuid = Uuid::new_v4();
    let mut seed = 0usize;
    for byte in uuid.as_bytes().iter().take(std::mem::size_of::<usize>()) {
        seed = (seed << 8) | *byte as usize;
    }
    seed
}

pub(crate) fn choose_annotation_layer_color(annotation_layers: &[AnnotationLayer]) -> String {
    let mut usage_counts: HashMap<&'static str, usize> = SET_COLOR_PALETTE
        .iter()
        .copied()
        .map(|color| (color, 0))
        .collect();
    let used_colors: HashSet<&str> = annotation_layers
        .iter()
        .map(|set| set.color_hex.as_str())
        .collect();
    for set in annotation_layers {
        if let Some(count) = usage_counts.get_mut(set.color_hex.as_str()) {
            *count += 1;
        }
    }

    let unused_colors: Vec<&str> = SET_COLOR_PALETTE
        .iter()
        .copied()
        .filter(|color| !used_colors.contains(color))
        .collect();
    let seed = palette_seed();
    if !unused_colors.is_empty() {
        return unused_colors[seed % unused_colors.len()].to_string();
    }

    let min_usage = usage_counts.values().copied().min().unwrap_or(0);
    let least_used: Vec<&str> = SET_COLOR_PALETTE
        .iter()
        .copied()
        .filter(|color| usage_counts.get(color).copied().unwrap_or(0) == min_usage)
        .collect();
    least_used[seed % least_used.len()].to_string()
}

pub(crate) fn annotation_label(annotation: &Annotation) -> String {
    match annotation {
        Annotation::Point(_) => "Point".to_string(),
        Annotation::Polygon(_) => "Polygon".to_string(),
    }
}

pub(crate) fn sort_annotation_layers(annotation_layers: &mut [AnnotationLayer]) {
    annotation_layers.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

pub(crate) fn unique_untitled_set_name(annotation_layers: &[AnnotationLayer]) -> String {
    let existing = annotation_layers
        .iter()
        .map(|set| set.name.as_str())
        .collect::<HashSet<_>>();
    if !existing.contains("Untitled") {
        return "Untitled".to_string();
    }

    let mut suffix = 1;
    loop {
        let candidate = format!("Untitled {suffix}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}
