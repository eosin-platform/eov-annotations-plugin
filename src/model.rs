//! The annotation data model and input validation.
//!
//! Coordinates are level-0 pixels of the slide, `x` to the right and `y`
//! down, origin at the top-left corner of the level-0 canvas (the convention
//! of OpenSlide and of the eov plugin API).

use eov_plugin_api::protocol::{Rgba, Shape};
use serde::{Deserialize, Serialize};

/// Database id of a slide record.
pub type SlideId = i64;

/// Longest layer, class or annotation name, in characters.
pub const MAX_NAME_CHARS: usize = 200;
/// Longest description or layer notes, in characters.
pub const MAX_TEXT_CHARS: usize = 10_000;
/// Most key/value properties per annotation.
pub const MAX_PROPERTIES: usize = 64;
pub const MAX_PROPERTY_KEY_CHARS: usize = 100;
pub const MAX_PROPERTY_VALUE_CHARS: usize = 4_000;
/// The host refuses objects with more vertices.
pub const MAX_VERTICES: usize = 100_000;

/// Review state of an annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Draft,
    Verified,
    Rejected,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::Draft, Status::Verified, Status::Rejected];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Verified => "verified",
            Status::Rejected => "rejected",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Draft => "Draft",
            Status::Verified => "Verified",
            Status::Rejected => "Rejected",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "draft" | "" => Some(Status::Draft),
            "verified" | "approved" | "accepted" => Some(Status::Verified),
            "rejected" => Some(Status::Rejected),
            _ => None,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Status::Draft => 0,
            Status::Verified => 1,
            Status::Rejected => 2,
        }
    }

    pub fn from_index(index: i64) -> Option<Self> {
        Status::ALL.get(usize::try_from(index).ok()?).copied()
    }
}

/// The kind of a geometry, for labels and icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryKind {
    Point,
    Polyline,
    Polygon,
    Rectangle,
    Ellipse,
    Text,
}

impl GeometryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GeometryKind::Point => "point",
            GeometryKind::Polyline => "polyline",
            GeometryKind::Polygon => "polygon",
            GeometryKind::Rectangle => "rectangle",
            GeometryKind::Ellipse => "ellipse",
            GeometryKind::Text => "text",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GeometryKind::Point => "Point",
            GeometryKind::Polyline => "Line",
            GeometryKind::Polygon => "Polygon",
            GeometryKind::Rectangle => "Rectangle",
            GeometryKind::Ellipse => "Ellipse",
            GeometryKind::Text => "Text",
        }
    }

    /// Whether the shape encloses an area (usable as a counting region).
    pub fn is_area(self) -> bool {
        matches!(
            self,
            GeometryKind::Polygon | GeometryKind::Rectangle | GeometryKind::Ellipse
        )
    }
}

/// The geometry of an annotation, in level-0 pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Geometry {
    Point {
        x: f64,
        y: f64,
    },
    Polyline {
        points: Vec<[f64; 2]>,
    },
    /// A simple exterior ring with simple, disjoint holes inside it (rings
    /// are implicitly closed).
    Polygon {
        exterior: Vec<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        holes: Vec<Vec<[f64; 2]>>,
    },
    Rectangle {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
    /// A text label anchored at its top-left corner; the text is the
    /// annotation's name.
    Text {
        x: f64,
        y: f64,
    },
}

/// An axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bounds {
    pub fn of_points<'a>(points: impl IntoIterator<Item = &'a [f64; 2]>) -> Bounds {
        let mut b = Bounds {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        for p in points {
            b.min_x = b.min_x.min(p[0]);
            b.min_y = b.min_y.min(p[1]);
            b.max_x = b.max_x.max(p[0]);
            b.max_y = b.max_y.max(p[1]);
        }
        if b.min_x > b.max_x {
            return Bounds::default();
        }
        b
    }

    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    pub fn intersects(&self, other: &Bounds) -> bool {
        self.min_x <= other.max_x
            && other.min_x <= self.max_x
            && self.min_y <= other.max_y
            && other.min_y <= self.max_y
    }

    pub fn center(&self) -> [f64; 2] {
        [
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        ]
    }
}

impl Geometry {
    pub fn kind(&self) -> GeometryKind {
        match self {
            Geometry::Point { .. } => GeometryKind::Point,
            Geometry::Polyline { .. } => GeometryKind::Polyline,
            Geometry::Polygon { .. } => GeometryKind::Polygon,
            Geometry::Rectangle { .. } => GeometryKind::Rectangle,
            Geometry::Ellipse { .. } => GeometryKind::Ellipse,
            Geometry::Text { .. } => GeometryKind::Text,
        }
    }

    pub fn bounds(&self) -> Bounds {
        match self {
            Geometry::Point { x, y } | Geometry::Text { x, y } => Bounds {
                min_x: *x,
                min_y: *y,
                max_x: *x,
                max_y: *y,
            },
            Geometry::Polyline { points } => Bounds::of_points(points),
            Geometry::Polygon { exterior, .. } => Bounds::of_points(exterior),
            Geometry::Rectangle {
                x,
                y,
                width,
                height,
            } => Bounds {
                min_x: *x,
                min_y: *y,
                max_x: x + width,
                max_y: y + height,
            },
            Geometry::Ellipse { cx, cy, rx, ry } => Bounds {
                min_x: cx - rx,
                min_y: cy - ry,
                max_x: cx + rx,
                max_y: cy + ry,
            },
        }
    }

    /// Number of stored vertices.
    pub fn vertex_count(&self) -> usize {
        match self {
            Geometry::Polyline { points } => points.len(),
            Geometry::Polygon { exterior, holes } => {
                exterior.len() + holes.iter().map(Vec::len).sum::<usize>()
            }
            _ => 1,
        }
    }

    /// Every coordinate is finite.
    pub fn is_finite(&self) -> bool {
        let finite = |p: &[f64; 2]| p[0].is_finite() && p[1].is_finite();
        match self {
            Geometry::Point { x, y } | Geometry::Text { x, y } => x.is_finite() && y.is_finite(),
            Geometry::Polyline { points } => points.iter().all(finite),
            Geometry::Polygon { exterior, holes } => {
                exterior.iter().all(finite) && holes.iter().all(|h| h.iter().all(finite))
            }
            Geometry::Rectangle {
                x,
                y,
                width,
                height,
            } => [x, y, width, height].iter().all(|v| v.is_finite()),
            Geometry::Ellipse { cx, cy, rx, ry } => [cx, cy, rx, ry].iter().all(|v| v.is_finite()),
        }
    }

    /// The host shape drawing this geometry (`text` for text labels).
    pub fn to_shape(&self, text: &str) -> Shape {
        match self {
            Geometry::Point { x, y } => Shape::Point { x: *x, y: *y },
            Geometry::Polyline { points } => Shape::Polyline {
                points: points.clone(),
            },
            Geometry::Polygon { exterior, holes } => Shape::Polygon {
                exterior: exterior.clone(),
                holes: holes.clone(),
            },
            Geometry::Rectangle {
                x,
                y,
                width,
                height,
            } => Shape::Rectangle {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            },
            Geometry::Ellipse { cx, cy, rx, ry } => Shape::Ellipse {
                cx: *cx,
                cy: *cy,
                rx: *rx,
                ry: *ry,
            },
            Geometry::Text { x, y } => Shape::Text {
                x: *x,
                y: *y,
                text: if text.trim().is_empty() {
                    "Text".to_string()
                } else {
                    text.to_string()
                },
            },
        }
    }

    /// Converts a host shape. Text shapes keep their text in the second
    /// value.
    pub fn from_shape(shape: &Shape) -> (Geometry, Option<String>) {
        match shape {
            Shape::Point { x, y } => (Geometry::Point { x: *x, y: *y }, None),
            Shape::Polyline { points } => (
                Geometry::Polyline {
                    points: points.clone(),
                },
                None,
            ),
            Shape::Polygon { exterior, holes } => (
                Geometry::Polygon {
                    exterior: exterior.clone(),
                    holes: holes.clone(),
                },
                None,
            ),
            Shape::Rectangle {
                x,
                y,
                width,
                height,
            } => (
                Geometry::Rectangle {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                },
                None,
            ),
            Shape::Ellipse { cx, cy, rx, ry } => (
                Geometry::Ellipse {
                    cx: *cx,
                    cy: *cy,
                    rx: *rx,
                    ry: *ry,
                },
                None,
            ),
            Shape::Text { x, y, text } => (Geometry::Text { x: *x, y: *y }, Some(text.clone())),
        }
    }

    /// The point used to frame or label the geometry.
    pub fn anchor(&self) -> [f64; 2] {
        match self {
            Geometry::Point { x, y } | Geometry::Text { x, y } => [*x, *y],
            Geometry::Ellipse { cx, cy, .. } => [*cx, *cy],
            other => other.bounds().center(),
        }
    }

    pub fn translated(&self, dx: f64, dy: f64) -> Geometry {
        let mv = |p: &[f64; 2]| [p[0] + dx, p[1] + dy];
        match self {
            Geometry::Point { x, y } => Geometry::Point {
                x: x + dx,
                y: y + dy,
            },
            Geometry::Text { x, y } => Geometry::Text {
                x: x + dx,
                y: y + dy,
            },
            Geometry::Polyline { points } => Geometry::Polyline {
                points: points.iter().map(mv).collect(),
            },
            Geometry::Polygon { exterior, holes } => Geometry::Polygon {
                exterior: exterior.iter().map(mv).collect(),
                holes: holes.iter().map(|h| h.iter().map(mv).collect()).collect(),
            },
            Geometry::Rectangle {
                x,
                y,
                width,
                height,
            } => Geometry::Rectangle {
                x: x + dx,
                y: y + dy,
                width: *width,
                height: *height,
            },
            Geometry::Ellipse { cx, cy, rx, ry } => Geometry::Ellipse {
                cx: cx + dx,
                cy: cy + dy,
                rx: *rx,
                ry: *ry,
            },
        }
    }
}

/// What a layer is for. Count points and HPF fields get their own layers so
/// they can be hidden or cleared together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LayerRole {
    #[default]
    Normal,
    Counts,
    Fields,
}

impl LayerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            LayerRole::Normal => "",
            LayerRole::Counts => "counts",
            LayerRole::Fields => "fields",
        }
    }

    pub fn parse(text: &str) -> Self {
        match text {
            "counts" => LayerRole::Counts,
            "fields" => LayerRole::Fields,
            _ => LayerRole::Normal,
        }
    }

    pub fn default_name(self) -> &'static str {
        match self {
            LayerRole::Normal => "Annotations",
            LayerRole::Counts => "Cell counts",
            LayerRole::Fields => "HPF fields",
        }
    }
}

/// An RGB colour (annotations are always drawn opaque; opacity is separate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    /// Parses `#RRGGBB` (or `#RRGGBBAA`, ignoring alpha, and `#RGB`).
    /// Never panics on arbitrary input (ANN-4).
    pub fn parse(text: &str) -> Option<Color> {
        let text = text.trim();
        let hex = text.strip_prefix('#')?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        match hex.len() {
            3 => {
                let nibble = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
                Some(Color(nibble(0)?, nibble(1)?, nibble(2)?))
            }
            6 | 8 => Some(Color(byte(0)?, byte(2)?, byte(4)?)),
            _ => None,
        }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    pub fn rgba(self, alpha: u8) -> Rgba {
        Rgba(self.0, self.1, self.2, alpha)
    }

    /// Packs as `0xRRGGBB`.
    pub fn to_rgb_u32(self) -> u32 {
        (u32::from(self.0) << 16) | (u32::from(self.1) << 8) | u32::from(self.2)
    }
}

/// The default palette for new layers and classes (Okabe–Ito based, which
/// stays distinguishable with the common colour-vision deficiencies, plus
/// a few saturated extras).
pub const PALETTE: [&str; 16] = [
    "#E69F00", "#56B4E9", "#009E73", "#F0E442", "#0072B2", "#D55E00", "#CC79A7", "#FFFFFF",
    "#FF355E", "#39FF14", "#9D4EDD", "#43AA8B", "#FF8A5B", "#B8F2E6", "#577590", "#F4A261",
];

/// The least used palette colour, preferring the order of [`PALETTE`].
pub fn pick_color<'a>(used: impl IntoIterator<Item = &'a Color>) -> Color {
    let mut counts = [0usize; PALETTE.len()];
    for color in used {
        if let Some(i) = PALETTE
            .iter()
            .position(|hex| Color::parse(hex).as_ref() == Some(color))
        {
            counts[i] += 1;
        }
    }
    let min = counts.iter().copied().min().unwrap_or(0);
    let index = counts.iter().position(|c| *c == min).unwrap_or(0);
    Color::parse(PALETTE[index]).unwrap_or(Color(0xFF, 0xFF, 0xFF))
}

/// A reusable class (ontology entry) shared by every slide.
#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    pub id: String,
    pub name: String,
    pub color: Color,
    /// Single-key hotkey that selects the class while an annotation tool is
    /// active.
    pub hotkey: Option<String>,
    pub visible: bool,
    /// 0–1, applied to the fill and outline of its annotations.
    pub opacity: f32,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A group of annotations on one slide.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub id: String,
    pub slide_id: SlideId,
    pub name: String,
    pub notes: String,
    /// Colour of unclassified annotations.
    pub color: Color,
    pub visible: bool,
    /// 0–1, applied to the whole layer.
    pub opacity: f32,
    /// Locked layers can't be edited or extended.
    pub locked: bool,
    pub role: LayerRole,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: String,
    pub layer_id: String,
    pub class_id: Option<String>,
    pub geometry: Geometry,
    /// Display name; the text of text annotations.
    pub name: String,
    pub description: String,
    pub status: Status,
    pub author: String,
    pub locked: bool,
    /// Free key/value metadata, in order.
    pub properties: Vec<(String, String)>,
    /// Unix milliseconds.
    pub created_at: i64,
    pub updated_at: i64,
}

impl Annotation {
    /// The label shown in lists: its name, else its kind.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.geometry.kind().label().to_string()
        } else {
            self.name.clone()
        }
    }
}

/// What the database knows about a slide.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlideRecord {
    pub id: SlideId,
    /// The host's slide identity (`FileInfo::identity`).
    pub identity: Option<String>,
    /// The pre-1.0 sampled fingerprint (migration key).
    pub legacy_fingerprint: Option<[u8; 32]>,
    /// Fallback key when neither is available.
    pub fallback_key: Option<String>,
    pub filename: String,
    pub width: u64,
    pub height: u64,
    pub mpp_x: Option<f64>,
    pub mpp_y: Option<f64>,
    pub vendor: Option<String>,
    pub created_at: i64,
    pub last_opened_at: i64,
}

/// Unix time in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A new random id.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Trims a name and checks its length. Control characters (including line
/// breaks) are replaced by spaces.
pub fn clean_name(text: &str, what: &str, allow_empty: bool) -> Result<String, String> {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() && !allow_empty {
        return Err(format!("The {what} cannot be empty."));
    }
    let count = cleaned.chars().count();
    if count > MAX_NAME_CHARS {
        return Err(format!(
            "The {what} is {count} characters long; the limit is {MAX_NAME_CHARS}."
        ));
    }
    Ok(cleaned)
}

/// Checks a multi-line text (description, notes).
pub fn clean_text(text: &str, what: &str) -> Result<String, String> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let count = cleaned.chars().count();
    if count > MAX_TEXT_CHARS {
        return Err(format!(
            "The {what} is {count} characters long; the limit is {MAX_TEXT_CHARS}."
        ));
    }
    Ok(cleaned)
}

/// Checks annotation properties.
pub fn clean_properties(properties: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
    if properties.len() > MAX_PROPERTIES {
        return Err(format!(
            "An annotation can have at most {MAX_PROPERTIES} properties."
        ));
    }
    properties
        .iter()
        .map(|(key, value)| {
            let key: String = key.chars().filter(|c| !c.is_control()).collect();
            let key = key.trim().to_string();
            let value: String = value
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .collect();
            if key.chars().count() > MAX_PROPERTY_KEY_CHARS {
                return Err(format!(
                    "Property names are limited to {MAX_PROPERTY_KEY_CHARS} characters."
                ));
            }
            if value.chars().count() > MAX_PROPERTY_VALUE_CHARS {
                return Err(format!(
                    "Property values are limited to {MAX_PROPERTY_VALUE_CHARS} characters."
                ));
            }
            Ok((key, value))
        })
        .collect()
}

/// Keys the host binds without modifiers (they can't be class hotkeys).
const HOST_PLAIN_KEYS: &[&str] = &[
    "f", "m", "n", "r", "d", "i", "+", "=", "-", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
];

/// Validates a class hotkey: empty for none, else one printable character
/// that neither eov nor this plugin's tools use.
pub fn clean_hotkey(text: &str, taken: &[&str]) -> Result<Option<String>, String> {
    let key = text.trim().to_lowercase();
    if key.is_empty() {
        return Ok(None);
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err("A hotkey is a single key (a letter or a symbol).".to_string());
    };
    if c.is_control() || c.is_whitespace() {
        return Err("A hotkey must be a visible character.".to_string());
    }
    if HOST_PLAIN_KEYS.contains(&key.as_str()) {
        return Err(format!("eov already uses the '{key}' key."));
    }
    if taken.contains(&key.as_str()) {
        return Err(format!(
            "'{key}' is already used by an annotation tool or class."
        ));
    }
    Ok(Some(key))
}

/// Opacity in 0–1 (non-finite values become 1).
pub fn clean_opacity(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0) as f32
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_without_panicking_on_any_input() {
        assert_eq!(Color::parse("#FF8000"), Some(Color(255, 128, 0)));
        assert_eq!(Color::parse("  #ff8000  "), Some(Color(255, 128, 0)));
        assert_eq!(Color::parse("#ff800080"), Some(Color(255, 128, 0)));
        assert_eq!(Color::parse("#f80"), Some(Color(255, 136, 0)));
        // ANN-4: byte-length checks used to slice inside multi-byte chars.
        for bad in [
            "##aébcd",
            "#aébcd",
            "#éééééé",
            "FF8000",
            "#12345",
            "",
            "#",
            "#GGGGGG",
        ] {
            assert_eq!(Color::parse(bad), None, "{bad}");
        }
        assert_eq!(Color(1, 2, 255).to_hex(), "#0102FF");
    }

    #[test]
    fn names_are_trimmed_and_limited() {
        assert_eq!(clean_name("  Tumour\n", "name", false).unwrap(), "Tumour");
        assert!(clean_name("   ", "name", false).is_err());
        assert_eq!(clean_name("", "name", true).unwrap(), "");
        assert!(clean_name(&"x".repeat(MAX_NAME_CHARS + 1), "name", false).is_err());
        // Counted in characters, not bytes.
        assert!(clean_name(&"é".repeat(MAX_NAME_CHARS), "name", false).is_ok());
    }

    #[test]
    fn hotkeys_avoid_host_and_tool_keys() {
        assert_eq!(clean_hotkey("Q", &[]).unwrap().as_deref(), Some("q"));
        assert_eq!(clean_hotkey("", &[]).unwrap(), None);
        assert!(clean_hotkey("f", &[]).is_err());
        assert!(clean_hotkey("5", &[]).is_err());
        assert!(clean_hotkey("p", &["p"]).is_err());
        assert!(clean_hotkey("ab", &[]).is_err());
        assert!(clean_hotkey(" ", &[]).is_ok());
    }

    #[test]
    fn geometry_round_trips_through_host_shapes() {
        let shapes = [
            Shape::Point { x: 1.0, y: 2.0 },
            Shape::Polyline {
                points: vec![[0.0, 0.0], [5.0, 5.0]],
            },
            Shape::Polygon {
                exterior: vec![[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]],
                holes: vec![],
            },
            Shape::Rectangle {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
            },
            Shape::Ellipse {
                cx: 1.0,
                cy: 2.0,
                rx: 3.0,
                ry: 4.0,
            },
        ];
        for shape in shapes {
            let (geometry, text) = Geometry::from_shape(&shape);
            assert!(text.is_none());
            assert_eq!(geometry.to_shape(""), shape);
        }
        let (text_geometry, text) = Geometry::from_shape(&Shape::Text {
            x: 1.0,
            y: 1.0,
            text: "Hi".into(),
        });
        assert_eq!(text.as_deref(), Some("Hi"));
        assert!(matches!(text_geometry.to_shape(""), Shape::Text { text, .. } if text == "Text"));
    }

    #[test]
    fn geometry_json_is_stable() {
        let g = Geometry::Polygon {
            exterior: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            holes: vec![],
        };
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"polygon","exterior":[[0.0,0.0],[1.0,0.0],[0.0,1.0]]}"#
        );
        assert_eq!(serde_json::from_str::<Geometry>(&json).unwrap(), g);
    }

    #[test]
    fn palette_prefers_unused_colors() {
        let first = pick_color([]);
        assert_eq!(first.to_hex(), PALETTE[0]);
        let second = pick_color([&first]);
        assert_eq!(second.to_hex(), PALETTE[1]);
    }

    #[test]
    fn status_parsing() {
        assert_eq!(Status::parse("Verified"), Some(Status::Verified));
        assert_eq!(Status::parse(""), Some(Status::Draft));
        assert_eq!(Status::parse("maybe"), None);
        assert_eq!(Status::from_index(2), Some(Status::Rejected));
        assert_eq!(Status::from_index(-1), None);
    }
}
