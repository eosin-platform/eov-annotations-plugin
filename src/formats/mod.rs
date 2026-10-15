//! Annotation interchange formats.
//!
//! | Format | Import | Export |
//! |---|---|---|
//! | eov JSON (format 2, and the format of the pre-1.0 plugin) | ✓ | ✓ |
//! | eov archive (every slide in the database) | ✓ (current slide) | ✓ |
//! | GeoJSON (QuPath-compatible) | ✓ | ✓ |
//! | ASAP XML | ✓ | ✓ |
//! | Aperio ImageScope XML | ✓ | ✓ |
//! | Hamamatsu NDPA | ✓ (placed with the slide's NDPI offsets) | |
//! | CSV measurements | | ✓ |
//!
//! All coordinates are level-0 pixels of the slide, origin at the top-left
//! corner of the level-0 canvas, x to the right, y down — the convention of
//! OpenSlide, QuPath, ASAP and ImageScope. The OpenSlide bounds offset
//! (`openslide.bounds-x/y`) is **not** subtracted; it is recorded in eov
//! exports so other tools can apply it.
//!
//! Importers never panic on malformed input: every unusable element is
//! skipped and counted with a reason.

pub mod csv;
pub mod eov;
pub mod geojson;
pub mod ndpa;
pub mod xml;

use crate::model::{Color, Comment, Geometry, Status};
use std::collections::BTreeMap;

/// The coordinates of a parsed document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Units {
    /// Level-0 pixels (every format but NDPA).
    #[default]
    Pixels,
    /// Nanometres from the centre of the slide (NDPA), until placed on a
    /// slide with [`ndpa::place`].
    NdpaNanometres,
}

/// What was read from a file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportDoc {
    pub format: &'static str,
    pub units: Units,
    pub layers: Vec<ImportLayer>,
    /// Classes named in the file, with their colours when given.
    pub classes: Vec<ImportClass>,
    pub slide: SlideHint,
    pub warnings: Vec<String>,
    /// Reason → count of skipped elements.
    pub skipped: BTreeMap<String, usize>,
    /// The slides of an archive (each a complete document).
    pub entries: Vec<ImportDoc>,
}

impl ImportDoc {
    pub fn skip(&mut self, reason: impl Into<String>) {
        *self.skipped.entry(reason.into()).or_default() += 1;
    }

    pub fn annotation_count(&self) -> usize {
        self.layers.iter().map(|l| l.annotations.len()).sum()
    }

    /// Adds a class (first colour wins).
    pub fn note_class(&mut self, name: &str, color: Option<Color>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match self
            .classes
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(name))
        {
            Some(existing) => {
                if existing.color.is_none() {
                    existing.color = color;
                }
            }
            None => self.classes.push(ImportClass {
                name: name.to_string(),
                color,
            }),
        }
    }

    pub fn skipped_summary(&self) -> Option<String> {
        if self.skipped.is_empty() {
            return None;
        }
        Some(
            self.skipped
                .iter()
                .map(|(reason, n)| format!("{n} × {reason}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportLayer {
    pub name: String,
    pub color: Option<Color>,
    pub notes: String,
    pub visible: Option<bool>,
    pub annotations: Vec<ImportAnnotation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportAnnotation {
    pub geometry: Geometry,
    /// Class name.
    pub class: Option<String>,
    pub name: String,
    pub description: String,
    pub status: Status,
    pub author: String,
    pub locked: bool,
    pub properties: Vec<(String, String)>,
    pub comments: Vec<Comment>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

impl ImportAnnotation {
    pub fn new(geometry: Geometry) -> Self {
        Self {
            geometry,
            class: None,
            name: String::new(),
            description: String::new(),
            status: Status::Draft,
            author: String::new(),
            locked: false,
            properties: Vec::new(),
            comments: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }
}

/// Reads exported comments (`[{"author", "text", "created_at"}]`);
/// unusable entries are left out.
pub fn parse_comments(value: Option<&serde_json::Value>) -> Vec<Comment> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|c| {
                    let text = c.get("text")?.as_str()?.to_string();
                    Some(Comment {
                        id: c
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(crate::model::new_id),
                        author: c
                            .get("author")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        text,
                        created_at: c.get("created_at").and_then(parse_time).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Comments as exports write them.
pub fn comments_json(comments: &[Comment]) -> serde_json::Value {
    serde_json::Value::Array(
        comments
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "author": c.author,
                    "text": c.text,
                    "created_at": iso8601(c.created_at),
                })
            })
            .collect(),
    )
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportClass {
    pub name: String,
    pub color: Option<Color>,
}

/// What a file says about the slide it was made for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlideHint {
    pub identity: Option<String>,
    pub quickhash1: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub mpp_x: Option<f64>,
    pub mpp_y: Option<f64>,
    /// Full-file SHA-256 (exports of the pre-1.0 plugin).
    pub legacy_sha256: Option<String>,
    pub filename: Option<String>,
}

/// Reads any supported format, recognised by content.
pub fn parse(bytes: &[u8], file_name: &str) -> Result<ImportDoc, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        "The file is not UTF-8 text, so it is not a supported annotation file.".to_string()
    })?;
    let text = text.trim_start_matches('\u{feff}');
    let first = text.trim_start().chars().next();
    let stem = std::path::Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported".to_string());
    match first {
        Some('{') | Some('[') => {
            let value: serde_json::Value = serde_json::from_str(text)
                .map_err(|e| format!("The file is not valid JSON: {e}"))?;
            if eov::is_eov(&value) {
                eov::parse(&value)
            } else if geojson::is_geojson(&value) {
                geojson::parse(&value, &stem)
            } else {
                Err("This JSON file is neither GeoJSON nor an eov annotation export.".to_string())
            }
        }
        Some('<') => xml::parse(text, &stem),
        _ => Err(
            "Unsupported file: expected GeoJSON, eov JSON, ASAP XML, ImageScope XML or NDPA."
                .to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// Formats Unix milliseconds as RFC 3339 UTC (`2026-10-15T09:30:00Z`).
pub fn iso8601(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Parses RFC 3339 timestamps (with `Z` or an offset) or Unix numbers
/// (seconds below 10¹¹, otherwise milliseconds).
pub fn parse_time(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => {
            let v = n.as_f64()?;
            if !v.is_finite() || v < 0.0 {
                return None;
            }
            Some(if v < 1e11 {
                (v * 1000.0) as i64
            } else {
                v as i64
            })
        }
        serde_json::Value::String(s) => parse_rfc3339(s),
        _ => None,
    }
}

fn parse_rfc3339(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = text.split_once(['T', ' '])?;
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (clock, offset_secs) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0)
    } else if let Some(pos) = time.rfind(['+', '-']) {
        let (clock, offset) = time.split_at(pos);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let mut hm = offset[1..].split(':');
        let h: i64 = hm.next()?.parse().ok()?;
        let mi: i64 = hm.next().unwrap_or("0").parse().ok()?;
        (clock, sign * (h * 3600 + mi * 60))
    } else {
        (time, 0)
    };
    let mut hms = clock.split(':');
    let h: i64 = hms.next()?.parse().ok()?;
    let mi: i64 = hms.next()?.parse().ok()?;
    let s: f64 = hms.next().unwrap_or("0").parse().ok()?;
    let days = days_from_civil(y, m, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + s.floor() as i64 - offset_secs;
    Some(secs * 1000 + ((s.fract()) * 1000.0) as i64)
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A JSON number as a finite f64.
pub(crate) fn num(value: &serde_json::Value) -> Option<f64> {
    let v = match value {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    v.is_finite().then_some(v)
}

/// A `[x, y]` JSON coordinate.
pub(crate) fn coord(value: &serde_json::Value) -> Option<[f64; 2]> {
    let array = value.as_array()?;
    Some([num(array.first()?)?, num(array.get(1)?)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        let t = 1_792_056_600_000; // 2026-10-15T09:30:00Z
        assert_eq!(iso8601(t), "2026-10-15T09:30:00Z");
        assert_eq!(
            parse_time(&serde_json::json!("2026-10-15T09:30:00Z")),
            Some(t)
        );
        assert_eq!(
            parse_time(&serde_json::json!("2026-10-15T11:30:00+02:00")),
            Some(t)
        );
        assert_eq!(parse_time(&serde_json::json!(1_792_056_600)), Some(t));
        assert_eq!(parse_time(&serde_json::json!(t)), Some(t));
        assert_eq!(parse_time(&serde_json::json!("yesterday")), None);
        assert_eq!(parse_time(&serde_json::json!("2026-13-01T00:00:00Z")), None);
    }

    #[test]
    fn unknown_content_is_rejected_with_a_message() {
        assert!(
            parse(b"hello", "x.txt")
                .unwrap_err()
                .contains("Unsupported")
        );
        assert!(
            parse(&[0xff, 0xfe, 0x00], "x.bin")
                .unwrap_err()
                .contains("UTF-8")
        );
        assert!(
            parse(b"{\"a\": 1}", "x.json")
                .unwrap_err()
                .contains("neither")
        );
        assert!(
            parse(b"{oops", "x.json")
                .unwrap_err()
                .contains("not valid JSON")
        );
    }
}
