//! DICOM Microscopy Bulk Simple Annotations (PS3.3 A.88 and C.37.1; SOP
//! class 1.2.840.10008.5.1.4.1.1.91.1, modality ANN), import and export.
//! The file format itself is read and written by [`part10`].
//!
//! # What maps to what
//!
//! - **Groups.** A DICOM annotation group has one graphic type and one
//!   property type, so eov writes one group per layer, class and kind of
//!   shape: the annotations of one class in one layer drawn the same way
//!   (the unclassified annotations of a layer form groups of their own).
//!   The group label is the class name, or the layer name for unclassified
//!   annotations (cut to the 64 characters of LO); the description names
//!   the layer and says what DICOM cannot show of the group; the
//!   Recommended Display CIELab Value is the class (or layer) colour. The
//!   generation type is MANUAL and the annotations apply to all optical
//!   paths.
//! - **Codes.** eov classes are names, not codes, so every group gets the
//!   generic property category and type SCT 91723000 "Anatomical
//!   Structure" (CID 7150), which says nothing wrong about what was drawn;
//!   the class is the group label.
//! - **Shapes.** Points and the anchors of text labels are POINTs; lines,
//!   and arrows from tail to tip, POLYLINEs; polygons POLYGONs (their outer
//!   ring, the first vertex not repeated); rectangles RECTANGLEs (the four
//!   corners clockwise from the top left); ellipses ELLIPSEs (the ends of
//!   the major axis, then those of the minor axis). On import, rectangles
//!   and ellipses that are not axis-aligned become polygons.
//! - **Coordinates.** Point Coordinates Data (OF, 32-bit floats) holds
//!   (column, row) pairs in the total pixel matrix of the image (Pixel
//!   Origin Interpretation VOLUME), with DICOM's image-relative convention
//!   (PS3.3 C.10.5.1.2): (0, 0) is the top-left corner of the top-left
//!   pixel, whose centre is (0.5, 0.5), and (Columns, Rows) is the
//!   bottom-right corner of the image. That is eov's level-0 convention
//!   (pixel *i* covers *i* to *i* + 1), so values are written and read
//!   unchanged; they are rounded to 32-bit floats (better than 0.01 px on
//!   slides of up to 100 000 px). For lines and polygons, Long Primitive
//!   Point Index List (OL) holds the 1-based index of each annotation's
//!   first coordinate *value* (1, 1 + 2·n₁, …), as highdicom and
//!   dicom-microscopy-viewer read it; a list that counts points instead is
//!   recognised when the values cannot be meant.
//! - **References.** When the slide is a DICOM image, OpenSlide publishes
//!   its level-0 instance's attributes as `dicom.*` properties: the export
//!   then joins the slide's study (its Study Instance UID, patient and
//!   study attributes copied, as one study requires), shares its frame of
//!   reference and refers to the image in the Referenced Image Sequence
//!   and the Referenced Series Sequence. Otherwise the object starts a
//!   study of its own with empty patient attributes (nothing identifying
//!   is invented) and refers to no image, which other viewers need to
//!   attach it to a slide (the Referenced Image Sequence that 2D
//!   annotations require is missing).
//! - **What DICOM cannot hold.** Holes, the text of text labels,
//!   arrowheads, layers, annotation names, descriptions and review
//!   statuses have no place in the IOD. They are kept in private attributes
//!   (creator "EOV ANNOTATIONS", group 0077: the slide at (0077,xx01), each
//!   group's layer, class, kind and annotations at (0077,xx02), as eov
//!   JSON), which other programs ignore and eov reads back, so a round trip
//!   through eov keeps them. Authors, comments, properties and times are
//!   not exported.
//!
//! Imports read groups of 2D annotations (POINT, POLYLINE, POLYGON,
//! RECTANGLE, ELLIPSE, with 32- or 64-bit coordinates) from any producer:
//! each group becomes a layer named after its label (groups with the same
//! label share one), its label the class of its annotations, and its
//! measurements annotation properties. 3D annotations (millimetres in the
//! slide's frame of reference) and coordinates relative to single frames
//! are refused with a reason.

pub mod part10;

use super::eov::{ExportOptions, ExportSlide, coordinates, slide_json};
use super::{ImportAnnotation, ImportDoc, ImportLayer};
use crate::model::{
    Annotation, Bounds, Class, Color, Geometry, GeometryKind, Layer, LayerRole, Status,
};
use eov_plugin_api::i18n::{isolate, tr};
use eov_plugin_api::protocol::PropertyMap;
use part10::{DataSet, Tag, Vr};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ops::Range;

/// Microscopy Bulk Simple Annotations Storage.
pub const SOP_CLASS_UID: &str = "1.2.840.10008.5.1.4.1.1.91.1";
/// VL Whole Slide Microscopy Image Storage.
const WSI_SOP_CLASS_UID: &str = "1.2.840.10008.5.1.4.1.1.77.1.6";
/// The structured report storage classes.
const SR_SOP_CLASS_PREFIX: &str = "1.2.840.10008.5.1.4.1.1.88.";
/// The implementation that wrote a file (eov's, derived from a UUID).
const IMPLEMENTATION_CLASS_UID: &str = "2.25.87798927157457428775379251802367988426";
const IMPLEMENTATION_VERSION_NAME: &str = concat!("EOV_ANN_", env!("CARGO_PKG_VERSION"));

/// Most annotations read from one file (the rest are skipped with a
/// reason), which bounds the memory an import takes.
pub const MAX_ANNOTATIONS: usize = 2_000_000;
/// Most measurements of a group turned into annotation properties.
const MAX_MEASUREMENTS: usize = 8;

/// eov's private block: the creator element (0077,00xx) holds this text,
/// and the block's elements are (0077,xx01) and (0077,xx02).
const PRIVATE_TAG_GROUP: u16 = 0x0077;
const PRIVATE_CREATOR: &str = "EOV ANNOTATIONS";
const PRIVATE_BLOCK: u16 = 0x10;
/// The slide (eov JSON), in the data set.
const PRIVATE_SLIDE: u16 = 0x01;
/// The layer, class, kind and annotations of a group, in its item.
const PRIVATE_GROUP: u16 = 0x02;

/// The tags of the attributes read and written.
mod tag {
    use super::part10::Tag;

    pub const SPECIFIC_CHARACTER_SET: Tag = Tag(0x0008, 0x0005);
    pub const INSTANCE_CREATION_DATE: Tag = Tag(0x0008, 0x0012);
    pub const INSTANCE_CREATION_TIME: Tag = Tag(0x0008, 0x0013);
    pub const SOP_CLASS_UID: Tag = Tag(0x0008, 0x0016);
    pub const SOP_INSTANCE_UID: Tag = Tag(0x0008, 0x0018);
    pub const STUDY_DATE: Tag = Tag(0x0008, 0x0020);
    pub const CONTENT_DATE: Tag = Tag(0x0008, 0x0023);
    pub const STUDY_TIME: Tag = Tag(0x0008, 0x0030);
    pub const CONTENT_TIME: Tag = Tag(0x0008, 0x0033);
    pub const ACCESSION_NUMBER: Tag = Tag(0x0008, 0x0050);
    pub const MODALITY: Tag = Tag(0x0008, 0x0060);
    pub const MANUFACTURER: Tag = Tag(0x0008, 0x0070);
    pub const INSTITUTION_NAME: Tag = Tag(0x0008, 0x0080);
    pub const REFERRING_PHYSICIAN_NAME: Tag = Tag(0x0008, 0x0090);
    pub const CODE_VALUE: Tag = Tag(0x0008, 0x0100);
    pub const CODING_SCHEME_DESIGNATOR: Tag = Tag(0x0008, 0x0102);
    pub const CODE_MEANING: Tag = Tag(0x0008, 0x0104);
    pub const TIMEZONE_OFFSET_FROM_UTC: Tag = Tag(0x0008, 0x0201);
    pub const STUDY_DESCRIPTION: Tag = Tag(0x0008, 0x1030);
    pub const SERIES_DESCRIPTION: Tag = Tag(0x0008, 0x103E);
    pub const MANUFACTURER_MODEL_NAME: Tag = Tag(0x0008, 0x1090);
    pub const REFERENCED_SERIES_SEQUENCE: Tag = Tag(0x0008, 0x1115);
    pub const REFERENCED_IMAGE_SEQUENCE: Tag = Tag(0x0008, 0x1140);
    pub const REFERENCED_INSTANCE_SEQUENCE: Tag = Tag(0x0008, 0x114A);
    pub const REFERENCED_SOP_CLASS_UID: Tag = Tag(0x0008, 0x1150);
    pub const REFERENCED_SOP_INSTANCE_UID: Tag = Tag(0x0008, 0x1155);
    pub const PATIENT_NAME: Tag = Tag(0x0010, 0x0010);
    pub const PATIENT_ID: Tag = Tag(0x0010, 0x0020);
    pub const PATIENT_BIRTH_DATE: Tag = Tag(0x0010, 0x0030);
    pub const PATIENT_SEX: Tag = Tag(0x0010, 0x0040);
    pub const DEVICE_SERIAL_NUMBER: Tag = Tag(0x0018, 0x1000);
    pub const SOFTWARE_VERSIONS: Tag = Tag(0x0018, 0x1020);
    pub const STUDY_INSTANCE_UID: Tag = Tag(0x0020, 0x000D);
    pub const SERIES_INSTANCE_UID: Tag = Tag(0x0020, 0x000E);
    pub const STUDY_ID: Tag = Tag(0x0020, 0x0010);
    pub const SERIES_NUMBER: Tag = Tag(0x0020, 0x0011);
    pub const INSTANCE_NUMBER: Tag = Tag(0x0020, 0x0013);
    pub const FRAME_OF_REFERENCE_UID: Tag = Tag(0x0020, 0x0052);
    pub const POSITION_REFERENCE_INDICATOR: Tag = Tag(0x0020, 0x1040);
    pub const MEASUREMENT_UNITS_CODE_SEQUENCE: Tag = Tag(0x0040, 0x08EA);
    pub const CONCEPT_NAME_CODE_SEQUENCE: Tag = Tag(0x0040, 0xA043);
    pub const ANNOTATION_GROUP_NUMBER: Tag = Tag(0x0040, 0xA180);
    pub const PIXEL_ORIGIN_INTERPRETATION: Tag = Tag(0x0048, 0x0301);
    pub const RECOMMENDED_DISPLAY_CIELAB_VALUE: Tag = Tag(0x0062, 0x000D);
    pub const POINT_COORDINATES_DATA: Tag = Tag(0x0066, 0x0016);
    pub const DOUBLE_POINT_COORDINATES_DATA: Tag = Tag(0x0066, 0x0022);
    pub const LONG_PRIMITIVE_POINT_INDEX_LIST: Tag = Tag(0x0066, 0x0040);
    pub const MEASUREMENTS_SEQUENCE: Tag = Tag(0x0066, 0x0121);
    pub const FLOATING_POINT_VALUES: Tag = Tag(0x0066, 0x0125);
    pub const MEASUREMENT_VALUES_SEQUENCE: Tag = Tag(0x0066, 0x0132);
    pub const ANNOTATION_COORDINATE_TYPE: Tag = Tag(0x006A, 0x0001);
    pub const ANNOTATION_GROUP_SEQUENCE: Tag = Tag(0x006A, 0x0002);
    pub const ANNOTATION_GROUP_UID: Tag = Tag(0x006A, 0x0003);
    pub const ANNOTATION_GROUP_LABEL: Tag = Tag(0x006A, 0x0005);
    pub const ANNOTATION_GROUP_DESCRIPTION: Tag = Tag(0x006A, 0x0006);
    pub const ANNOTATION_GROUP_GENERATION_TYPE: Tag = Tag(0x006A, 0x0007);
    pub const ANNOTATION_PROPERTY_CATEGORY_CODE_SEQUENCE: Tag = Tag(0x006A, 0x0009);
    pub const ANNOTATION_PROPERTY_TYPE_CODE_SEQUENCE: Tag = Tag(0x006A, 0x000A);
    pub const NUMBER_OF_ANNOTATIONS: Tag = Tag(0x006A, 0x000C);
    pub const ANNOTATION_APPLIES_TO_ALL_OPTICAL_PATHS: Tag = Tag(0x006A, 0x000D);
    pub const ANNOTATION_INDEX_LIST: Tag = Tag(0x006A, 0x0011);
    pub const GRAPHIC_TYPE: Tag = Tag(0x0070, 0x0023);
    pub const CONTENT_LABEL: Tag = Tag(0x0070, 0x0080);
    pub const CONTENT_DESCRIPTION: Tag = Tag(0x0070, 0x0081);
    pub const CONTENT_CREATOR_NAME: Tag = Tag(0x0070, 0x0084);
}

/// A coded concept.
struct Code {
    value: &'static str,
    scheme: &'static str,
    meaning: &'static str,
}

/// The property category and type of every exported group.
const ANATOMICAL_STRUCTURE: Code = Code {
    value: "91723000",
    scheme: "SCT",
    meaning: "Anatomical Structure",
};

fn code_item(code: &Code) -> DataSet {
    let mut item = DataSet::new();
    item.put_text(tag::CODE_VALUE, Vr::SH, code.value);
    item.put_text(tag::CODING_SCHEME_DESIGNATOR, Vr::SH, code.scheme);
    item.put_text(tag::CODE_MEANING, Vr::LO, code.meaning);
    item
}

/// Whether `text` is a UID: digits and dots, at most 64 characters, no
/// empty component.
pub fn is_uid(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .split('.')
            .all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()))
}

/// A new UID: `2.25.` and a random UUID as a decimal integer (PS3.5 B.2).
pub fn new_uid() -> String {
    format!("2.25.{}", uuid::Uuid::new_v4().as_u128())
}

/// A text value for a short-string VR: no control characters, no `\`
/// (the value separator), at most `max` characters.
fn short_text(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| match c {
            '\\' => '/',
            c if c.is_control() => ' ',
            c => c,
        })
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A date (DA, `YYYYMMDD`) or `""`.
fn date_value(text: &str) -> String {
    let text = text.trim();
    if text.len() == 8 && text.bytes().all(|b| b.is_ascii_digit()) {
        text.to_string()
    } else {
        String::new()
    }
}

/// A time (TM, `HHMMSS.FFFFFF` or a prefix of it) or `""`.
fn time_value(text: &str) -> String {
    let text = text.trim();
    let (clock, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (2..=6).contains(&clock.len())
        && clock.len() % 2 == 0
        && digits(clock)
        && fraction.len() <= 6
        && digits(fraction)
    {
        text.to_string()
    } else {
        String::new()
    }
}

/// Unix milliseconds as a DICOM date and time (UTC).
fn date_time(ms: i64) -> (String, String) {
    let secs = ms.div_euclid(1000);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = super::civil_from_days(secs.div_euclid(86_400));
    (
        format!("{y:04}{m:02}{d:02}"),
        format!("{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60),
    )
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// D50, the white of the ICC profile connection space DICOM's CIELab
/// values use.
const D50: [f64; 3] = [0.964_22, 1.0, 0.825_21];
const LAB_EPSILON: f64 = 216.0 / 24_389.0;
const LAB_KAPPA: f64 = 24_389.0 / 27.0;

/// An sRGB colour as DICOM CIELab PCS values (PS3.3 C.10.7.1.1): L* 0–100
/// and a*, b* −128–127 scaled to 0–65535, with D50 white (sRGB adapted to
/// it with the Bradford transform).
pub fn rgb_to_lab(color: Color) -> [u16; 3] {
    let linear = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(color.0), linear(color.1), linear(color.2));
    let xyz = [
        0.436_074_7 * r + 0.385_064_9 * g + 0.143_080_4 * b,
        0.222_504_5 * r + 0.716_878_6 * g + 0.060_616_9 * b,
        0.013_932_2 * r + 0.097_104_5 * g + 0.714_173_3 * b,
    ];
    let f = |t: f64| {
        if t > LAB_EPSILON {
            t.cbrt()
        } else {
            (LAB_KAPPA * t + 16.0) / 116.0
        }
    };
    let [fx, fy, fz] = [f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2])];
    let (l, a, b) = (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
    let scale = |v: f64| v.round().clamp(0.0, 65_535.0) as u16;
    [
        scale(l * 65_535.0 / 100.0),
        scale((a + 128.0) * 65_535.0 / 255.0),
        scale((b + 128.0) * 65_535.0 / 255.0),
    ]
}

/// DICOM CIELab PCS values as the nearest sRGB colour.
pub fn lab_to_rgb(lab: [u16; 3]) -> Color {
    let l = f64::from(lab[0]) * 100.0 / 65_535.0;
    let a = f64::from(lab[1]) * 255.0 / 65_535.0 - 128.0;
    let b = f64::from(lab[2]) * 255.0 / 65_535.0 - 128.0;
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inverse = |f: f64| {
        let cube = f * f * f;
        if cube > LAB_EPSILON {
            cube
        } else {
            (116.0 * f - 16.0) / LAB_KAPPA
        }
    };
    let y = if l > LAB_KAPPA * LAB_EPSILON {
        fy * fy * fy
    } else {
        l / LAB_KAPPA
    };
    let (x, y, z) = (inverse(fx) * D50[0], y * D50[1], inverse(fz) * D50[2]);
    let r = 3.133_856_1 * x - 1.616_866_7 * y - 0.490_614_6 * z;
    let g = -0.978_768_4 * x + 1.916_141_5 * y + 0.033_454_0 * z;
    let b = 0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z;
    let encode = |c: f64| {
        let c = c.clamp(0.0, 1.0);
        let v = if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    Color(encode(r), encode(g), encode(b))
}

// ---------------------------------------------------------------------------
// eov's private attributes
// ---------------------------------------------------------------------------

/// What DICOM cannot hold of a group, kept for eov (and read back).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct PrivateGroup {
    #[serde(default)]
    layer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer_color: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    layer_notes: String,
    /// What the layer is for (`counts`, `fields`; empty for others).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    layer_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer_visible: Option<bool>,
    #[serde(default)]
    class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    class_color: Option<String>,
    /// The eov shape kind (`point`, `text`, `polyline`, `arrow`, …).
    #[serde(default)]
    kind: String,
    /// Only the annotations that have something to keep.
    #[serde(default)]
    annotations: Vec<PrivateAnnotation>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct PrivateAnnotation {
    /// Position in the group, from 0.
    index: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    holes: Vec<Vec<[f64; 2]>>,
}

fn put_private(dataset: &mut DataSet, element: u16, json: &str) {
    dataset.put_text(
        Tag(PRIVATE_TAG_GROUP, PRIVATE_BLOCK),
        Vr::LO,
        PRIVATE_CREATOR,
    );
    dataset.put_text(
        Tag(PRIVATE_TAG_GROUP, (PRIVATE_BLOCK << 8) | element),
        Vr::UT,
        json,
    );
}

/// An element of eov's private block, wherever its creator reserved it.
fn private_text(dataset: &DataSet, element: u16) -> Option<String> {
    let block = (0x10..=0xFF)
        .find(|b| dataset.text(Tag(PRIVATE_TAG_GROUP, *b)).as_deref() == Some(PRIVATE_CREATOR))?;
    dataset.text(Tag(PRIVATE_TAG_GROUP, (block << 8) | element))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// The DICOM image an export refers to: the open slide, when it is one
/// (OpenSlide's `dicom.*` properties of its level-0 instance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Source {
    pub study_instance_uid: String,
    pub series_instance_uid: String,
    pub sop_class_uid: String,
    pub sop_instance_uid: String,
    pub frame_of_reference_uid: Option<String>,
    pub position_reference_indicator: String,
    // Copied so that the annotations join the slide's study.
    pub patient_name: String,
    pub patient_id: String,
    pub patient_birth_date: String,
    pub patient_sex: String,
    pub study_date: String,
    pub study_time: String,
    pub accession_number: String,
    pub referring_physician_name: String,
    pub study_id: String,
    pub study_description: String,
}

impl Source {
    /// The DICOM identity of a slide, from its properties; `None` unless
    /// it is a DICOM image whose study, series and instance are known.
    pub fn of_slide(props: &PropertyMap) -> Option<Source> {
        let value = |key: &str| {
            props
                .get(&format!("dicom.{key}"))
                .map(|v| v.trim_matches(['\0', ' ']).to_string())
                .unwrap_or_default()
        };
        let uid = |key: &str| Some(value(key)).filter(|v| is_uid(v));
        let sex = value("PatientSex").to_ascii_uppercase();
        Some(Source {
            study_instance_uid: uid("StudyInstanceUID")?,
            series_instance_uid: uid("SeriesInstanceUID")?,
            sop_class_uid: uid("SOPClassUID")?,
            sop_instance_uid: uid("SOPInstanceUID")?,
            frame_of_reference_uid: uid("FrameOfReferenceUID"),
            position_reference_indicator: short_text(&value("PositionReferenceIndicator"), 64),
            patient_name: short_text(&value("PatientName"), 64 * 3),
            patient_id: short_text(&value("PatientID"), 64),
            patient_birth_date: date_value(&value("PatientBirthDate")),
            patient_sex: if matches!(sex.as_str(), "M" | "F" | "O") {
                sex
            } else {
                String::new()
            },
            study_date: date_value(&value("StudyDate")),
            study_time: time_value(&value("StudyTime")),
            accession_number: short_text(&value("AccessionNumber"), 16),
            referring_physician_name: short_text(&value("ReferringPhysicianName"), 64 * 3),
            study_id: short_text(&value("StudyID"), 16),
            study_description: short_text(&value("StudyDescription"), 64),
        })
    }
}

/// An export and what the user should know about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub bytes: Vec<u8>,
    /// Annotations written.
    pub count: usize,
    /// What other DICOM readers will not see, and what the file refers to.
    pub notes: Vec<String>,
}

/// The annotations of one layer, class and kind of shape.
struct Group<'a> {
    layer: &'a Layer,
    class: Option<&'a Class>,
    kind: GeometryKind,
    annotations: Vec<&'a Annotation>,
}

fn groups<'a>(slide: &'a ExportSlide, classes: &'a [Class]) -> Vec<Group<'a>> {
    let mut out: Vec<Group<'a>> = Vec::new();
    for (layer, annotations) in &slide.layers {
        let first = out.len();
        for a in annotations {
            let class = a
                .class_id
                .as_deref()
                .and_then(|id| classes.iter().find(|c| c.id == id));
            let kind = a.geometry.kind();
            let same = |g: &&mut Group<'a>| {
                g.kind == kind && g.class.map(|c| &c.id) == class.map(|c| &c.id)
            };
            match out[first..].iter_mut().find(same) {
                Some(group) => group.annotations.push(a),
                None => out.push(Group {
                    layer,
                    class,
                    kind,
                    annotations: vec![a],
                }),
            }
        }
    }
    out
}

fn graphic_type(kind: GeometryKind) -> &'static str {
    match kind {
        GeometryKind::Point | GeometryKind::Text => "POINT",
        GeometryKind::Polyline | GeometryKind::Arrow => "POLYLINE",
        GeometryKind::Polygon => "POLYGON",
        GeometryKind::Rectangle => "RECTANGLE",
        GeometryKind::Ellipse => "ELLIPSE",
    }
}

/// The points DICOM keeps of a shape, or `None` when it has too few or
/// coordinates that do not fit 32-bit floats.
fn points(geometry: &Geometry) -> Option<Vec<[f64; 2]>> {
    let points = match geometry {
        Geometry::Point { x, y } | Geometry::Text { x, y } => vec![[*x, *y]],
        Geometry::Polyline { points } | Geometry::Arrow { points } => points.clone(),
        Geometry::Polygon { exterior, .. } => {
            let mut ring = exterior.clone();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            ring
        }
        Geometry::Rectangle {
            x,
            y,
            width,
            height,
        } => vec![
            [*x, *y],
            [x + width, *y],
            [x + width, y + height],
            [*x, y + height],
        ],
        // The ends of the major axis, then of the minor axis.
        Geometry::Ellipse { cx, cy, rx, ry } if rx >= ry => vec![
            [cx - rx, *cy],
            [cx + rx, *cy],
            [*cx, cy - ry],
            [*cx, cy + ry],
        ],
        Geometry::Ellipse { cx, cy, rx, ry } => vec![
            [*cx, cy - ry],
            [*cx, cy + ry],
            [cx - rx, *cy],
            [cx + rx, *cy],
        ],
    };
    let needed = match geometry.kind() {
        GeometryKind::Polyline | GeometryKind::Arrow => 2,
        GeometryKind::Polygon => 3,
        _ => 1,
    };
    let fits = |v: f64| v.is_finite() && (v as f32).is_finite();
    (points.len() >= needed && points.iter().all(|p| fits(p[0]) && fits(p[1]))).then_some(points)
}

/// What an export could not put into DICOM's own attributes.
#[derive(Default)]
struct Losses {
    holes: usize,
    holed_regions: usize,
    texts: usize,
    arrows: usize,
    named: usize,
    skipped: usize,
}

fn group_item(number: usize, group: &Group<'_>, losses: &mut Losses) -> Result<DataSet, String> {
    let mut values: Vec<f32> = Vec::new();
    let mut starts: Vec<u32> = Vec::new();
    let mut kept = Vec::new();
    let mut holed = 0usize;
    for a in &group.annotations {
        let Some(points) = points(&a.geometry) else {
            losses.skipped += 1;
            continue;
        };
        let index = kept.len();
        let start = u32::try_from(values.len() + 1)
            .ok()
            .filter(|s| *s < u32::MAX)
            .ok_or_else(|| tr!("There are too many coordinates for one DICOM annotation group."))?;
        starts.push(start);
        values.extend(points.iter().flat_map(|p| [p[0] as f32, p[1] as f32]));
        let mut entry = PrivateAnnotation {
            index,
            name: a.name.clone(),
            description: a.description.clone(),
            status: if a.status == Status::Draft {
                String::new()
            } else {
                a.status.as_str().to_string()
            },
            holes: Vec::new(),
        };
        if let Geometry::Polygon { holes, .. } = &a.geometry
            && !holes.is_empty()
        {
            losses.holes += holes.len();
            holed += 1;
            entry.holes = holes.clone();
        }
        let described =
            !entry.name.is_empty() || !entry.description.is_empty() || !entry.status.is_empty();
        losses.named += usize::from(described);
        kept.push((described || !entry.holes.is_empty()).then_some(entry));
    }
    let count = kept.len();
    losses.holed_regions += holed;
    match group.kind {
        GeometryKind::Text => losses.texts += count,
        GeometryKind::Arrow => losses.arrows += count,
        _ => {}
    }
    let label = group
        .class
        .map_or(group.layer.name.as_str(), |c| c.name.as_str());
    let label = Some(short_text(label, 64))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "Annotations".to_string());
    let mut description = format!("eov layer \"{}\"", group.layer.name);
    match group.kind {
        GeometryKind::Text => description.push_str(
            ": the anchor points of text labels, whose text DICOM bulk annotations cannot hold.",
        ),
        GeometryKind::Arrow => description.push_str(
            ": arrows, as lines from tail to tip (DICOM bulk annotations have no arrowheads).",
        ),
        GeometryKind::Polygon if holed > 0 => description.push_str(&format!(
            ": {holed} of these regions {} holes, which DICOM bulk annotations cannot hold; they are drawn without them.",
            if holed == 1 { "has" } else { "have" }
        )),
        _ => description.push('.'),
    }
    let color = group.class.map_or(group.layer.color, |c| c.color);
    let private = PrivateGroup {
        layer: group.layer.name.clone(),
        layer_color: Some(group.layer.color.to_hex()),
        layer_notes: group.layer.notes.clone(),
        layer_role: group.layer.role.as_str().to_string(),
        layer_visible: (!group.layer.visible).then_some(false),
        class: group.class.map(|c| c.name.clone()),
        class_color: group.class.map(|c| c.color.to_hex()),
        kind: group.kind.as_str().to_string(),
        annotations: kept.into_iter().flatten().collect(),
    };
    let private = serde_json::to_string(&private).map_err(|e| e.to_string())?;
    let mut item = DataSet::new();
    item.put_u16s(
        tag::ANNOTATION_GROUP_NUMBER,
        &[u16::try_from(number).map_err(|_| tr!("There are too many annotation groups."))?],
    );
    item.put_u16s(tag::RECOMMENDED_DISPLAY_CIELAB_VALUE, &rgb_to_lab(color));
    item.put_f32s(tag::POINT_COORDINATES_DATA, Vr::OF, &values);
    if matches!(graphic_type(group.kind), "POLYLINE" | "POLYGON") {
        item.put_u32s(tag::LONG_PRIMITIVE_POINT_INDEX_LIST, Vr::OL, &starts);
    }
    item.put_text(tag::ANNOTATION_GROUP_UID, Vr::UI, &new_uid());
    item.put_text(tag::ANNOTATION_GROUP_LABEL, Vr::LO, &label);
    item.put_text(tag::ANNOTATION_GROUP_DESCRIPTION, Vr::UT, &description);
    item.put_text(tag::ANNOTATION_GROUP_GENERATION_TYPE, Vr::CS, "MANUAL");
    item.put_items(
        tag::ANNOTATION_PROPERTY_CATEGORY_CODE_SEQUENCE,
        vec![code_item(&ANATOMICAL_STRUCTURE)],
    );
    item.put_items(
        tag::ANNOTATION_PROPERTY_TYPE_CODE_SEQUENCE,
        vec![code_item(&ANATOMICAL_STRUCTURE)],
    );
    item.put_u32(
        tag::NUMBER_OF_ANNOTATIONS,
        u32::try_from(count).map_err(|_| tr!("There are too many annotations in one group."))?,
    );
    item.put_text(tag::ANNOTATION_APPLIES_TO_ALL_OPTICAL_PATHS, Vr::CS, "YES");
    item.put_text(tag::GRAPHIC_TYPE, Vr::CS, graphic_type(group.kind));
    put_private(&mut item, PRIVATE_GROUP, &private);
    Ok(item)
}

fn reference_item(source: &Source) -> DataSet {
    let mut item = DataSet::new();
    item.put_text(tag::REFERENCED_SOP_CLASS_UID, Vr::UI, &source.sop_class_uid);
    item.put_text(
        tag::REFERENCED_SOP_INSTANCE_UID,
        Vr::UI,
        &source.sop_instance_uid,
    );
    item
}

/// A Microscopy Bulk Simple Annotations object of a slide (see the module
/// documentation for what goes where). `source` is the slide's DICOM
/// identity when it is a DICOM image.
pub fn export(
    slide: &ExportSlide,
    classes: &[Class],
    options: &ExportOptions,
    source: Option<&Source>,
) -> Result<Export, String> {
    let mut losses = Losses::default();
    let mut items = Vec::new();
    for group in groups(slide, classes) {
        let item = group_item(items.len() + 1, &group, &mut losses)?;
        if item.uint(tag::NUMBER_OF_ANNOTATIONS) != Some(0) {
            items.push(item);
        }
    }
    let count: usize = items
        .iter()
        .filter_map(|i| i.uint(tag::NUMBER_OF_ANNOTATIONS))
        .sum::<u64>() as usize;
    if items.is_empty() {
        return Err(tr!("The slide has no annotations to export."));
    }
    let (date, time) = date_time(options.exported_at);
    let sop_instance_uid = new_uid();
    let study_instance_uid = source.map_or_else(new_uid, |s| s.study_instance_uid.clone());
    // The slide's patient and study attributes, or empty ones.
    let copied = source.cloned().unwrap_or_default();
    let mut ds = DataSet::new();
    // SOP Common.
    ds.put_text(tag::SPECIFIC_CHARACTER_SET, Vr::CS, "ISO_IR 192");
    ds.put_text(tag::INSTANCE_CREATION_DATE, Vr::DA, &date);
    ds.put_text(tag::INSTANCE_CREATION_TIME, Vr::TM, &time);
    ds.put_text(tag::SOP_CLASS_UID, Vr::UI, SOP_CLASS_UID);
    ds.put_text(tag::SOP_INSTANCE_UID, Vr::UI, &sop_instance_uid);
    ds.put_text(tag::TIMEZONE_OFFSET_FROM_UTC, Vr::SH, "+0000");
    // Patient (type 2: empty unless the slide says).
    ds.put_text(tag::PATIENT_NAME, Vr::PN, &copied.patient_name);
    ds.put_text(tag::PATIENT_ID, Vr::LO, &copied.patient_id);
    ds.put_text(tag::PATIENT_BIRTH_DATE, Vr::DA, &copied.patient_birth_date);
    ds.put_text(tag::PATIENT_SEX, Vr::CS, &copied.patient_sex);
    // General Study: the slide's, or a new one made now.
    ds.put_text(tag::STUDY_INSTANCE_UID, Vr::UI, &study_instance_uid);
    match source {
        Some(s) => {
            ds.put_text(tag::STUDY_DATE, Vr::DA, &s.study_date);
            ds.put_text(tag::STUDY_TIME, Vr::TM, &s.study_time);
            if !s.study_description.is_empty() {
                ds.put_text(tag::STUDY_DESCRIPTION, Vr::LO, &s.study_description);
            }
        }
        None => {
            ds.put_text(tag::STUDY_DATE, Vr::DA, &date);
            ds.put_text(tag::STUDY_TIME, Vr::TM, &time);
        }
    }
    ds.put_text(
        tag::REFERRING_PHYSICIAN_NAME,
        Vr::PN,
        &copied.referring_physician_name,
    );
    ds.put_text(tag::STUDY_ID, Vr::SH, &copied.study_id);
    ds.put_text(tag::ACCESSION_NUMBER, Vr::SH, &copied.accession_number);
    // General Series and Microscopy Bulk Simple Annotations Series.
    ds.put_text(tag::MODALITY, Vr::CS, "ANN");
    ds.put_text(tag::SERIES_INSTANCE_UID, Vr::UI, &new_uid());
    ds.put_text(tag::SERIES_NUMBER, Vr::IS, "1");
    ds.put_text(tag::SERIES_DESCRIPTION, Vr::LO, "eov annotations");
    // Frame of Reference: the slide's.
    if let Some(frame) = source.and_then(|s| s.frame_of_reference_uid.as_deref()) {
        ds.put_text(tag::FRAME_OF_REFERENCE_UID, Vr::UI, frame);
        ds.put_text(
            tag::POSITION_REFERENCE_INDICATOR,
            Vr::LO,
            &copied.position_reference_indicator,
        );
    }
    // General and Enhanced General Equipment.
    ds.put_text(tag::MANUFACTURER, Vr::LO, "eov");
    ds.put_text(
        tag::MANUFACTURER_MODEL_NAME,
        Vr::LO,
        "eov annotations plugin",
    );
    ds.put_text(tag::DEVICE_SERIAL_NUMBER, Vr::LO, "1");
    ds.put_text(tag::SOFTWARE_VERSIONS, Vr::LO, env!("CARGO_PKG_VERSION"));
    let organization = short_text(&options.organization, 64);
    if !organization.is_empty() {
        ds.put_text(tag::INSTITUTION_NAME, Vr::LO, &organization);
    }
    // Microscopy Bulk Simple Annotations.
    ds.put_text(tag::INSTANCE_NUMBER, Vr::IS, "1");
    ds.put_text(tag::CONTENT_LABEL, Vr::CS, "EOV_ANNOTATIONS");
    let project = short_text(&options.project, 64);
    ds.put_text(
        tag::CONTENT_DESCRIPTION,
        Vr::LO,
        if project.is_empty() {
            "Annotations made with eov"
        } else {
            &project
        },
    );
    ds.put_text(
        tag::CONTENT_CREATOR_NAME,
        Vr::PN,
        &short_text(&options.author, 64),
    );
    ds.put_text(tag::CONTENT_DATE, Vr::DA, &date);
    ds.put_text(tag::CONTENT_TIME, Vr::TM, &time);
    ds.put_text(tag::ANNOTATION_COORDINATE_TYPE, Vr::CS, "2D");
    ds.put_text(tag::PIXEL_ORIGIN_INTERPRETATION, Vr::CS, "VOLUME");
    ds.put_items(tag::ANNOTATION_GROUP_SEQUENCE, items);
    // The image (and its series, Common Instance Reference) annotated.
    if let Some(source) = source {
        ds.put_items(tag::REFERENCED_IMAGE_SEQUENCE, vec![reference_item(source)]);
        let mut series = DataSet::new();
        series.put_items(
            tag::REFERENCED_INSTANCE_SEQUENCE,
            vec![reference_item(source)],
        );
        series.put_text(
            tag::SERIES_INSTANCE_UID,
            Vr::UI,
            &source.series_instance_uid,
        );
        ds.put_items(tag::REFERENCED_SERIES_SEQUENCE, vec![series]);
    }
    let private = json!({
        "format": "eov-annotations-dicom",
        "format_version": 1,
        "generator": options.generator,
        "coordinates": coordinates(),
        "slide": slide_json(slide, options),
    });
    put_private(&mut ds, PRIVATE_SLIDE, &private.to_string());
    let bytes = part10::write(
        &part10::FileMeta {
            sop_class_uid: SOP_CLASS_UID,
            sop_instance_uid: &sop_instance_uid,
            implementation_class_uid: IMPLEMENTATION_CLASS_UID,
            implementation_version_name: IMPLEMENTATION_VERSION_NAME,
        },
        &ds,
    )?;
    Ok(Export {
        bytes,
        count,
        notes: notes(&losses, source),
    })
}

/// The export note: what the file refers to and what other viewers will
/// not see (in the language of the interface: eov shows it).
fn notes(losses: &Losses, source: Option<&Source>) -> Vec<String> {
    let mut notes = vec![match source {
        Some(s) => tr!(
            "It refers to this slide's DICOM image (series {}) and joins its study; the patient and study attributes are the slide's.",
            isolate(&s.series_instance_uid)
        ),
        None => tr!(
            "This slide is not a DICOM image, so the file refers to no image: it is a study of its own with empty patient attributes, and DICOM viewers cannot attach it to a slide by themselves."
        ),
    }];
    // What DICOM bulk annotations cannot hold, as items of one list.
    let mut lost = Vec::new();
    if losses.holes > 0 {
        lost.push(tr!(
            // Translators: an item of the list in "DICOM bulk annotations cannot
            // hold {}: …"; {regions} is "in 1 region" or "in 2 regions"; "drawn
            // without it/them": other viewers draw these regions without their holes.
            "{n} hole {regions} (drawn without it)"
                | "{n} holes {regions} (drawn without them)" % losses.holes,
            regions = tr!(
                // Translators: in "{n} holes {regions} (drawn without them)".
                "in {n} region" | "in {n} regions" % losses.holed_regions
            )
        ));
    }
    if losses.texts > 0 {
        lost.push(tr!(
            // Translators: an item of the list in "DICOM bulk annotations cannot hold {}: …".
            "the text of {n} text label (written as points)"
                | "the text of {n} text labels (written as points)" % losses.texts
        ));
    }
    if losses.arrows > 0 {
        lost.push(tr!(
            // Translators: an item of the list in "DICOM bulk annotations cannot hold {}: …".
            "the heads of {n} arrow (written as lines)"
                | "the heads of {n} arrows (written as lines)" % losses.arrows
        ));
    }
    if losses.named > 0 {
        lost.push(tr!(
            // Translators: an item of the list in "DICOM bulk annotations cannot hold {}: …".
            "the names, descriptions and statuses of {n} annotation"
                | "the names, descriptions and statuses of {n} annotations" % losses.named
        ));
    }
    if !lost.is_empty() {
        notes.push(tr!(
            // Translators: {} lists what is lost, such as "the heads of 2 arrows (written
            // as lines) and the text of 1 text label (written as points)".
            "DICOM bulk annotations cannot hold {}: other viewers do not see them, but eov reads them back from its private attributes.",
            join_and(&lost)
        ));
    }
    notes.push(tr!(
        "Each layer, class and kind of shape is a group labelled with its class (or its layer). Authors, comments, properties and times are not exported (eov JSON keeps everything)."
    ));
    if losses.skipped > 0 {
        notes.push(tr!(
            "{n} annotation with too few points or unusable coordinates was left out."
                | "{n} annotations with too few points or unusable coordinates were left out."
                    % losses.skipped
        ));
    }
    notes
}

/// Items as a list: "a, b and c".
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => tr!(
            // Translators: the end of a list, "a, b and c": {first} is "a, b".
            "{first} and {last}",
            first = rest.join(&tr!(
                // Translators: between the items of a list ("a, b and c").
                ", "
            )),
            last = last
        ),
    }
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// The DICOM objects a file says it annotates.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reference {
    pub study: Option<String>,
    pub series: Vec<String>,
    pub instances: Vec<String>,
}

/// Whether a file made for the DICOM objects in `reference` belongs to the
/// open slide, by its `dicom.*` properties.
pub fn match_slide(reference: &Reference, props: &PropertyMap) -> (String, bool) {
    let get = |key: &str| {
        props
            .get(key)
            .map(|v| v.trim_matches(['\0', ' ']).to_string())
            .filter(|v| !v.is_empty())
    };
    let (series, instance) = (get("dicom.SeriesInstanceUID"), get("dicom.SOPInstanceUID"));
    if series.is_none() && instance.is_none() {
        return (
            tr!(
                "Made for a DICOM slide; this one is not a DICOM image, so whether the annotations belong to it cannot be checked. Coordinates are taken as level-0 pixels of this slide."
            ),
            true,
        );
    }
    if instance
        .as_ref()
        .is_some_and(|i| reference.instances.contains(i))
    {
        return (tr!("Made for this slide (its DICOM image)."), false);
    }
    if series
        .as_ref()
        .is_some_and(|s| reference.series.contains(s))
    {
        return if reference.instances.is_empty() {
            (tr!("Made for this slide (its DICOM series)."), false)
        } else {
            (
                tr!(
                    "Made for another image of this slide's DICOM series (another resolution level?), whose pixels may not be level-0 pixels."
                ),
                true,
            )
        };
    }
    (
        tr!("Made for a different DICOM slide (it refers to other images)."),
        true,
    )
}

/// Why a DICOM file that is not a bulk annotation object is not imported.
fn not_annotations(sop_class: &str) -> String {
    if sop_class == WSI_SOP_CLASS_UID {
        tr!(
            "This DICOM file is a slide image (VL Whole Slide Microscopy), not annotations: open it as a slide. Annotations are read from Microscopy Bulk Simple Annotations objects (modality ANN)."
        )
    } else if sop_class.starts_with(SR_SOP_CLASS_PREFIX) {
        tr!(
            "This DICOM file is a structured report (SR). eov reads DICOM annotations from Microscopy Bulk Simple Annotations objects (modality ANN), not from structured reports."
        )
    } else if sop_class.is_empty() {
        tr!("This DICOM file does not say what it holds (it has no SOP Class UID).")
    } else {
        tr!(
            "This DICOM file is not an annotation object (SOP class {}); eov reads Microscopy Bulk Simple Annotations (modality ANN).",
            isolate(sop_class)
        )
    }
}

/// What a file says about its slide: eov's private slide description and
/// the DICOM objects it refers to.
fn slide_hint(ds: &DataSet, doc: &mut ImportDoc) {
    if let Some(text) = private_text(ds, PRIVATE_SLIDE) {
        match serde_json::from_str::<Value>(&text) {
            Ok(root) => {
                let slide = &root["slide"];
                let text = |key: &str| slide.get(key).and_then(Value::as_str).map(str::to_string);
                doc.slide.identity = text("identity");
                doc.slide.quickhash1 = text("quickhash1");
                doc.slide.filename = text("filename");
                doc.slide.width = slide.get("width").and_then(Value::as_u64);
                doc.slide.height = slide.get("height").and_then(Value::as_u64);
                doc.slide.mpp_x = slide.get("mpp_x").and_then(super::num);
                doc.slide.mpp_y = slide.get("mpp_y").and_then(super::num);
            }
            Err(_) => doc.warnings.push(tr!(
                "eov's private description of the slide is unreadable and was ignored."
            )),
        }
    }
    let uid = |item: &DataSet, tag: Tag| item.text(tag).filter(|u| is_uid(u));
    let mut reference = Reference {
        study: uid(ds, tag::STUDY_INSTANCE_UID),
        ..Default::default()
    };
    for series in ds.items(tag::REFERENCED_SERIES_SEQUENCE) {
        reference
            .series
            .extend(uid(series, tag::SERIES_INSTANCE_UID));
        for instance in series.items(tag::REFERENCED_INSTANCE_SEQUENCE) {
            reference
                .instances
                .extend(uid(instance, tag::REFERENCED_SOP_INSTANCE_UID));
        }
    }
    for image in ds.items(tag::REFERENCED_IMAGE_SEQUENCE) {
        reference
            .instances
            .extend(uid(image, tag::REFERENCED_SOP_INSTANCE_UID));
    }
    if !reference.series.is_empty() || !reference.instances.is_empty() {
        doc.slide.dicom = Some(reference);
    }
}

/// Reads a DICOM file of bulk annotations.
pub fn parse(bytes: &[u8], stem: &str) -> Result<ImportDoc, String> {
    let file = part10::read(bytes).map_err(|e| {
        // Translators: {} says what is wrong with the file, in English.
        tr!("The DICOM file cannot be read: {}.", e)
    })?;
    let ds = &file.dataset;
    let sop_class = ds
        .text(tag::SOP_CLASS_UID)
        .filter(|c| !c.is_empty())
        .or_else(|| file.meta.text(part10::MEDIA_STORAGE_SOP_CLASS_UID))
        .unwrap_or_default();
    if sop_class != SOP_CLASS_UID {
        return Err(not_annotations(&sop_class));
    }
    match ds
        .text(tag::ANNOTATION_COORDINATE_TYPE)
        .unwrap_or_default()
        .as_str()
    {
        "2D" => {}
        "3D" => {
            return Err(tr!(
                "This file holds 3D annotations: their coordinates are millimetres in the slide's frame of reference (DICOM SCOORD3D), and eov places annotations by level-0 pixels. Only 2D (pixel) annotations can be imported."
            ));
        }
        "" => {
            return Err(tr!(
                "The file does not say whether its coordinates are 2D or 3D (Annotation Coordinate Type is missing)."
            ));
        }
        other => {
            return Err(tr!(
                "The file's coordinates are of an unknown type (\"{}\"); 2D annotations can be imported.",
                isolate(other)
            ));
        }
    }
    let mut doc = ImportDoc {
        format: "DICOM bulk annotations",
        ..Default::default()
    };
    match ds
        .text(tag::PIXEL_ORIGIN_INTERPRETATION)
        .unwrap_or_default()
        .as_str()
    {
        "" | "VOLUME" => {}
        "FRAME" => {
            return Err(tr!(
                "The annotations' pixel coordinates are relative to single frames (tiles) of the image (Pixel Origin Interpretation FRAME), which eov cannot place; coordinates relative to the whole image (VOLUME) can be imported."
            ));
        }
        other => doc.warnings.push(tr!(
            "Unknown pixel origin interpretation \"{}\"; coordinates are taken relative to the whole image.",
            isolate(other)
        )),
    }
    let groups = ds.items(tag::ANNOTATION_GROUP_SEQUENCE);
    if groups.is_empty() {
        return Err(tr!("The DICOM file has no annotation groups."));
    }
    slide_hint(ds, &mut doc);
    let mut total = 0usize;
    let mut rotated = 0usize;
    for (index, group) in groups.iter().enumerate() {
        read_group(group, index, stem, &mut doc, &mut total, &mut rotated);
    }
    if rotated > 0 {
        doc.warnings.push(tr!(
            "{n} rectangle or ellipse not aligned with the image axes became a polygon."
                | "{n} rectangles and ellipses not aligned with the image axes became polygons."
                    % rotated
        ));
    }
    Ok(doc)
}

/// The coordinate values of a group, 32-bit or 64-bit.
fn coordinate_values(item: &DataSet) -> Result<Option<Vec<f64>>, String> {
    if let Some(values) = item.f32s(tag::POINT_COORDINATES_DATA)? {
        return Ok(Some(values.into_iter().map(f64::from).collect()));
    }
    item.f64s(tag::DOUBLE_POINT_COORDINATES_DATA)
}

/// The coordinate values of each line or polygon of a group, from its Long
/// Primitive Point Index List: the 1-based index of each annotation's
/// first coordinate value. A list that counts points instead is read as
/// such (with a warning) when it cannot be meant as values. The error is
/// why the group is skipped.
fn primitive_ranges(
    item: &DataSet,
    values: usize,
    label: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<Range<usize>>, String> {
    let Some(element) = item.get(tag::LONG_PRIMITIVE_POINT_INDEX_LIST) else {
        return if item.uint(tag::NUMBER_OF_ANNOTATIONS) == Some(1) {
            Ok(std::iter::once(0..values).collect())
        } else {
            Err(tr!(
                "annotation group with lines or polygons without a point index list"
            ))
        };
    };
    let unreadable = |_| tr!("annotation group with an unreadable point index list");
    let starts: Vec<u64> = if matches!(element.vr, Vr::OV | Vr::UV) {
        item.u64s(tag::LONG_PRIMITIVE_POINT_INDEX_LIST)
            .map_err(unreadable)?
            .unwrap_or_default()
    } else {
        item.u32s(tag::LONG_PRIMITIVE_POINT_INDEX_LIST)
            .map_err(unreadable)?
            .unwrap_or_default()
            .into_iter()
            .map(u64::from)
            .collect()
    };
    if starts.is_empty() {
        return Err(tr!(
            "annotation group with lines or polygons with an empty point index list"
        ));
    }
    let fits = |limit: u64, step: u64| {
        starts.windows(2).all(|w| w[0] < w[1])
            && starts
                .iter()
                .all(|&s| s >= 1 && s <= limit && (s - 1) % step == 0)
    };
    let n = values as u64;
    let scale = if fits(n, 2) {
        1
    } else if fits(n / 2, 1) {
        warnings.push(tr!(
            "The point index list of \"{}\" counts points rather than coordinate values; it was read that way.",
            isolate(label)
        ));
        2
    } else {
        return Err(tr!(
            "annotation group with a point index list that does not fit its coordinates"
        ));
    };
    let offsets: Vec<usize> = starts.iter().map(|&s| (s as usize - 1) * scale).collect();
    Ok(offsets
        .iter()
        .enumerate()
        .map(|(i, &start)| start..offsets.get(i + 1).copied().unwrap_or(values))
        .collect())
}

/// An ellipse from the ends of its major and minor axes; one that is not
/// axis-aligned becomes a polygon (`true`).
fn ellipse(p: &[[f64; 2]]) -> (Geometry, bool) {
    let (a, b, c, d) = (p[0], p[1], p[2], p[3]);
    let (cx, cy) = ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
    let (ux, uy) = ((b[0] - a[0]) / 2.0, (b[1] - a[1]) / 2.0);
    let major = ux.hypot(uy);
    let minor = (d[0] - c[0]).hypot(d[1] - c[1]) / 2.0;
    let tolerance = 1e-6 * major.max(minor).max(1.0);
    if uy.abs() <= tolerance {
        (
            Geometry::Ellipse {
                cx,
                cy,
                rx: major,
                ry: minor,
            },
            false,
        )
    } else if ux.abs() <= tolerance {
        (
            Geometry::Ellipse {
                cx,
                cy,
                rx: minor,
                ry: major,
            },
            false,
        )
    } else {
        let (sin, cos) = uy.atan2(ux).sin_cos();
        let exterior = (0..64)
            .map(|i| {
                let t = f64::from(i) / 64.0 * std::f64::consts::TAU;
                let (x, y) = (major * t.cos(), minor * t.sin());
                [cx + x * cos - y * sin, cy + x * sin + y * cos]
            })
            .collect();
        (
            Geometry::Polygon {
                exterior,
                holes: Vec::new(),
            },
            true,
        )
    }
}

/// A rectangle from its four corners; one that is not axis-aligned stays a
/// polygon (`true`).
fn rectangle(points: Vec<[f64; 2]>) -> (Geometry, bool) {
    let b = Bounds::of_points(&points);
    let tolerance = 1e-6 * b.width().max(b.height()).max(1.0);
    let near = |v: f64, t: f64| (v - t).abs() <= tolerance;
    let aligned = points.iter().all(|p| {
        (near(p[0], b.min_x) || near(p[0], b.max_x)) && (near(p[1], b.min_y) || near(p[1], b.max_y))
    });
    if aligned {
        (
            Geometry::Rectangle {
                x: b.min_x,
                y: b.min_y,
                width: b.width(),
                height: b.height(),
            },
            false,
        )
    } else {
        (
            Geometry::Polygon {
                exterior: points,
                holes: Vec::new(),
            },
            true,
        )
    }
}

/// A group's measurements as annotation properties (`[name] [unit]` →
/// value), one list per annotation (empty when it has none).
fn measurements(item: &DataSet, count: usize, doc: &mut ImportDoc) -> Vec<Vec<(String, String)>> {
    let list = item.items(tag::MEASUREMENTS_SEQUENCE);
    if list.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut out = vec![Vec::new(); count];
    for m in list.iter().take(MAX_MEASUREMENTS) {
        let meaning = |seq: Tag, field: Tag| {
            m.items(seq)
                .first()
                .and_then(|c| c.text(field))
                .map(|t| short_text(&t, 64))
                .unwrap_or_default()
        };
        let name = Some(meaning(tag::CONCEPT_NAME_CODE_SEQUENCE, tag::CODE_MEANING))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Measurement".to_string());
        let unit = meaning(tag::MEASUREMENT_UNITS_CODE_SEQUENCE, tag::CODE_VALUE);
        let key = if unit.is_empty() || unit == "1" {
            name
        } else {
            format!("{name} [{unit}]")
        };
        let Some(values_item) = m.items(tag::MEASUREMENT_VALUES_SEQUENCE).first() else {
            // Translators: a reason why a measurement of an imported file was skipped.
            doc.skip(tr!("measurement without values"));
            continue;
        };
        let (Ok(Some(values)), Ok(indices)) = (
            values_item.f32s(tag::FLOATING_POINT_VALUES),
            values_item.u32s(tag::ANNOTATION_INDEX_LIST),
        ) else {
            doc.skip(tr!("measurement with unreadable values"));
            continue;
        };
        let pairs: Vec<(usize, f32)> = match indices {
            // 1-based annotation numbers of the values.
            Some(indices) if indices.len() == values.len() => indices
                .iter()
                .map(|i| (*i as usize).wrapping_sub(1))
                .zip(values)
                .collect(),
            None if values.len() == count => values.into_iter().enumerate().collect(),
            _ => {
                doc.skip(tr!("measurement that does not match its annotations"));
                continue;
            }
        };
        for (index, value) in pairs {
            if let Some(slot) = out.get_mut(index)
                && value.is_finite()
            {
                slot.push((key.clone(), format!("{value}")));
            }
        }
    }
    out
}

fn read_group(
    item: &DataSet,
    index: usize,
    stem: &str,
    doc: &mut ImportDoc,
    total: &mut usize,
    rotated: &mut usize,
) {
    let number = item
        .uint(tag::ANNOTATION_GROUP_NUMBER)
        .unwrap_or(index as u64 + 1);
    let label = item
        .text(tag::ANNOTATION_GROUP_LABEL)
        .map(|l| l.trim().to_string())
        .unwrap_or_default();
    let property_type = item
        .items(tag::ANNOTATION_PROPERTY_TYPE_CODE_SEQUENCE)
        .first()
        .and_then(|c| c.text(tag::CODE_MEANING))
        .map(|m| m.trim().to_string())
        .unwrap_or_default();
    // What the group is: its label, else what its property type says.
    let class = [&label, &property_type]
        .into_iter()
        .find(|t| !t.is_empty())
        .cloned();
    let name = class.clone().unwrap_or_else(|| format!("{stem} {number}"));
    let values = match coordinate_values(item) {
        Ok(Some(values)) => values,
        Ok(None) => {
            // Translators: a reason why a group of an imported file was skipped.
            doc.skip(tr!("annotation group without coordinates"));
            return;
        }
        Err(_) => {
            doc.skip(tr!("annotation group with unreadable coordinates"));
            return;
        }
    };
    if values.len() % 2 != 0 {
        doc.skip(tr!(
            "annotation group whose coordinates are not (column, row) pairs"
        ));
        return;
    }
    let graphic = item
        .text(tag::GRAPHIC_TYPE)
        .unwrap_or_default()
        .to_ascii_uppercase();
    let fixed = |points: usize| {
        let step = 2 * points;
        (0..values.len() / step)
            .map(|i| i * step..(i + 1) * step)
            .collect::<Vec<_>>()
    };
    let ranges = match graphic.as_str() {
        "POINT" => fixed(1),
        "RECTANGLE" | "ELLIPSE" if values.len() % 8 == 0 => fixed(4),
        "RECTANGLE" | "ELLIPSE" => {
            doc.skip(tr!("rectangles or ellipses without four points each"));
            return;
        }
        "POLYLINE" | "POLYGON" => {
            match primitive_ranges(item, values.len(), &name, &mut doc.warnings) {
                Ok(ranges) => ranges,
                Err(reason) => {
                    doc.skip(reason);
                    return;
                }
            }
        }
        "" => {
            doc.skip(tr!("annotation group without a graphic type"));
            return;
        }
        other => {
            doc.skip(tr!("annotation group of graphic type {}", isolate(other)));
            return;
        }
    };
    if let Some(declared) = item.uint(tag::NUMBER_OF_ANNOTATIONS)
        && declared != ranges.len() as u64
    {
        doc.warnings.push(tr!(
            "\"{name}\" says it has {n} annotation, but its coordinates hold {held}; the coordinates were read."
                | "\"{name}\" says it has {n} annotations, but its coordinates hold {held}; the coordinates were read."
                % declared,
            name = isolate(&name),
            held = ranges.len()
        ));
    }
    let private = match private_text(item, PRIVATE_GROUP) {
        Some(text) => match serde_json::from_str::<PrivateGroup>(&text) {
            Ok(p) if !p.layer.trim().is_empty() => Some(p),
            _ => {
                doc.warnings.push(tr!(
                    "eov's private attributes of \"{}\" are unreadable; the group was read from its DICOM attributes alone.",
                    isolate(&name)
                ));
                None
            }
        },
        None => None,
    };
    let lab = item
        .u16s(tag::RECOMMENDED_DISPLAY_CIELAB_VALUE)
        .filter(|v| v.len() == 3)
        .map(|v| lab_to_rgb([v[0], v[1], v[2]]));
    let description = item
        .text(tag::ANNOTATION_GROUP_DESCRIPTION)
        .unwrap_or_default();
    let (new_layer, class, class_color, kind) = match &private {
        Some(p) => (
            ImportLayer {
                name: p.layer.clone(),
                color: p.layer_color.as_deref().and_then(Color::parse),
                notes: p.layer_notes.clone(),
                visible: p.layer_visible,
                role: LayerRole::parse(&p.layer_role),
                annotations: Vec::new(),
            },
            p.class.clone().filter(|c| !c.trim().is_empty()),
            p.class_color.as_deref().and_then(Color::parse).or(lab),
            p.kind.as_str(),
        ),
        None => (
            ImportLayer {
                name,
                color: lab,
                notes: description,
                ..Default::default()
            },
            class,
            lab,
            "",
        ),
    };
    if let Some(class) = &class {
        doc.note_class(class, class_color);
    }
    let layer = match doc.layers.iter().position(|l| l.name == new_layer.name) {
        Some(i) => i,
        None => {
            doc.layers.push(new_layer);
            doc.layers.len() - 1
        }
    };
    let extras: BTreeMap<usize, PrivateAnnotation> = private
        .as_ref()
        .map(|p| {
            let mut map = BTreeMap::new();
            for a in &p.annotations {
                map.entry(a.index).or_insert_with(|| a.clone());
            }
            map
        })
        .unwrap_or_default();
    let mut properties = measurements(item, ranges.len(), doc);
    for (i, range) in ranges.iter().enumerate() {
        if *total >= MAX_ANNOTATIONS {
            *doc.skipped
                .entry(tr!(
                    "annotations beyond the limit of {} per file",
                    MAX_ANNOTATIONS
                ))
                .or_default() += ranges.len() - i;
            return;
        }
        let coordinates = &values[range.clone()];
        if !coordinates.iter().all(|v| v.is_finite()) {
            doc.skip(tr!("annotation with non-finite coordinates"));
            continue;
        }
        let mut points: Vec<[f64; 2]> = coordinates.as_chunks::<2>().0.to_vec();
        let extra = extras.get(&i);
        let geometry = match graphic.as_str() {
            "POINT" => {
                let [x, y] = points[0];
                if kind == "text" {
                    Geometry::Text { x, y }
                } else {
                    Geometry::Point { x, y }
                }
            }
            "POLYLINE" if kind == "arrow" => Geometry::Arrow { points },
            "POLYLINE" => Geometry::Polyline { points },
            "POLYGON" => {
                if points.len() > 1 && points.first() == points.last() {
                    points.pop();
                }
                Geometry::Polygon {
                    exterior: points,
                    holes: extra.map(|e| e.holes.clone()).unwrap_or_default(),
                }
            }
            "RECTANGLE" => {
                let (geometry, turned) = rectangle(points);
                *rotated += usize::from(turned);
                geometry
            }
            _ => {
                let (geometry, turned) = ellipse(&points);
                *rotated += usize::from(turned);
                geometry
            }
        };
        let mut a = ImportAnnotation::new(geometry);
        a.class = class.clone();
        if let Some(extra) = extra {
            a.name = extra.name.clone();
            a.description = extra.description.clone();
            a.status = Status::parse(&extra.status).unwrap_or_default();
        }
        if let Some(list) = properties.get_mut(i) {
            a.properties = std::mem::take(list);
        }
        doc.layers[layer].annotations.push(a);
        *total += 1;
    }
}

#[cfg(test)]
mod tests;
