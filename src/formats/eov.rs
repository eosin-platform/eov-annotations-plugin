//! The eov annotation format.
//!
//! Format 2 (this version) is self-describing (ANN-17): it has a format
//! name and version, the coordinate convention, the slide's identity, size,
//! pixel spacing and bounds, and never the absolute path of the slide (the
//! file name only when the user enables it). Format 1 is what the pre-1.0
//! plugin exported (`file_path`, `file_sha256`, `annotation_layers`).

use super::{ImportAnnotation, ImportDoc, ImportLayer, SlideHint, iso8601, num, parse_time};
use crate::model::{Annotation, Class, Color, Geometry, Layer, LayerRole, SlideRecord, Status};
use eov_plugin_api::i18n::tr;
use eov_plugin_api::protocol::Rect;
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub const FORMAT: &str = "eov-annotations";
pub const ARCHIVE_FORMAT: &str = "eov-annotations-archive";
pub const FORMAT_VERSION: i64 = 2;

/// One slide to export.
pub struct ExportSlide {
    pub record: SlideRecord,
    pub quickhash1: Option<String>,
    pub bounds: Option<Rect>,
    pub layers: Vec<(Layer, Vec<Arc<Annotation>>)>,
}

#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    pub include_filename: bool,
    pub author: String,
    pub organization: String,
    pub project: String,
    pub license: String,
    pub generator: String,
    pub exported_at: i64,
}

pub fn coordinates() -> Value {
    json!({
        "space": "level0_pixels",
        "origin": "top_left_of_level0_canvas",
        "x_axis": "right",
        "y_axis": "down",
        "unit": "pixel",
        "bounds_offset_subtracted": false
    })
}

pub fn slide_json(slide: &ExportSlide, options: &ExportOptions) -> Value {
    let r = &slide.record;
    let mut s = Map::new();
    s.insert("identity".into(), json!(r.identity));
    s.insert("quickhash1".into(), json!(slide.quickhash1));
    s.insert("width".into(), json!(r.width));
    s.insert("height".into(), json!(r.height));
    s.insert("mpp_x".into(), json!(r.mpp_x));
    s.insert("mpp_y".into(), json!(r.mpp_y));
    s.insert("vendor".into(), json!(r.vendor));
    s.insert(
        "bounds".into(),
        slide.bounds.map_or(
            Value::Null,
            |b| json!({ "x": b.x, "y": b.y, "width": b.width, "height": b.height }),
        ),
    );
    if options.include_filename {
        s.insert("filename".into(), json!(r.filename));
    }
    Value::Object(s)
}

fn class_name<'a>(classes: &'a [Class], id: Option<&str>) -> Option<&'a str> {
    let id = id?;
    classes.iter().find(|c| c.id == id).map(|c| c.name.as_str())
}

/// Builds a format-2 document.
pub fn export(slide: &ExportSlide, classes: &[Class], options: &ExportOptions) -> Value {
    let used: std::collections::HashSet<&str> = slide
        .layers
        .iter()
        .flat_map(|(_, list)| list.iter().filter_map(|a| a.class_id.as_deref()))
        .collect();
    let class_list: Vec<Value> = classes
        .iter()
        .filter(|c| used.contains(c.id.as_str()))
        .map(|c| json!({ "name": c.name, "color": c.color.to_hex() }))
        .collect();
    let layers: Vec<Value> = slide
        .layers
        .iter()
        .map(|(layer, annotations)| {
            json!({
                "id": layer.id,
                "name": layer.name,
                "notes": layer.notes,
                "color": layer.color.to_hex(),
                "visible": layer.visible,
                "opacity": layer.opacity,
                "locked": layer.locked,
                "role": layer.role.as_str(),
                "created_at": iso8601(layer.created_at),
                "updated_at": iso8601(layer.updated_at),
                "annotations": annotations.iter().map(|a| json!({
                    "id": a.id,
                    "geometry": a.geometry,
                    "class": class_name(classes, a.class_id.as_deref()),
                    "name": a.name,
                    "description": a.description,
                    "status": a.status.as_str(),
                    "author": a.author,
                    "locked": a.locked,
                    "properties": a.properties,
                    "comments": super::comments_json(&a.comments),
                    "created_at": iso8601(a.created_at),
                    "updated_at": iso8601(a.updated_at),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "generator": options.generator,
        "exported_at": iso8601(options.exported_at),
        "coordinates": coordinates(),
        "slide": slide_json(slide, options),
        "metadata": {
            "author": options.author,
            "organization": options.organization,
            "project": options.project,
            "license": options.license,
        },
        "classes": class_list,
        "layers": layers,
    })
}

/// Every slide of the database in one document (backup, "export all").
pub fn archive(slides: &[ExportSlide], classes: &[Class], options: &ExportOptions) -> Value {
    json!({
        "format": ARCHIVE_FORMAT,
        "format_version": FORMAT_VERSION,
        "generator": options.generator,
        "exported_at": iso8601(options.exported_at),
        "classes": classes.iter().map(|c| json!({
            "name": c.name, "color": c.color.to_hex(), "hotkey": c.hotkey,
            "visible": c.visible, "opacity": c.opacity,
        })).collect::<Vec<_>>(),
        "slides": slides.iter().map(|s| export(s, classes, options)).collect::<Vec<_>>(),
    })
}

pub fn is_eov(value: &Value) -> bool {
    match value.get("format").and_then(Value::as_str) {
        Some(FORMAT) | Some(ARCHIVE_FORMAT) => true,
        _ => value.get("annotation_layers").is_some_and(Value::is_array),
    }
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn properties(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        // [[key, value], …]
        Value::Array(rows) => rows
            .iter()
            .filter_map(|row| match row {
                Value::Array(kv) => Some((text(kv.first()), text(kv.get(1)))),
                // [{key, value}, …] (format 1)
                Value::Object(o) => Some((text(o.get("key")), text(o.get("value")))),
                _ => None,
            })
            .collect(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string()),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn parse(value: &Value) -> Result<ImportDoc, String> {
    match value.get("format").and_then(Value::as_str) {
        Some(ARCHIVE_FORMAT) => parse_archive(value),
        Some(FORMAT) => {
            let version = value
                .get("format_version")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if version > FORMAT_VERSION {
                return Err(tr!(
                    "This file uses eov annotation format {version}; this plugin reads up to {supported}. Update the plugin.",
                    version = version,
                    supported = FORMAT_VERSION
                ));
            }
            parse_v2(value)
        }
        _ => parse_v1(value),
    }
}

fn parse_v2(value: &Value) -> Result<ImportDoc, String> {
    let mut doc = ImportDoc {
        format: "eov JSON",
        ..Default::default()
    };
    if let Some(slide) = value.get("slide") {
        doc.slide = SlideHint {
            identity: slide
                .get("identity")
                .and_then(Value::as_str)
                .map(str::to_string),
            quickhash1: slide
                .get("quickhash1")
                .and_then(Value::as_str)
                .map(str::to_string),
            width: slide.get("width").and_then(Value::as_u64),
            height: slide.get("height").and_then(Value::as_u64),
            mpp_x: slide.get("mpp_x").and_then(num),
            mpp_y: slide.get("mpp_y").and_then(num),
            legacy_sha256: None,
            filename: slide
                .get("filename")
                .and_then(Value::as_str)
                .map(str::to_string),
            dicom: None,
        };
    }
    for class in value
        .get("classes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = text(class.get("name"));
        let color = class
            .get("color")
            .and_then(Value::as_str)
            .and_then(Color::parse);
        doc.note_class(&name, color);
    }
    let Some(layers) = value.get("layers").and_then(Value::as_array) else {
        return Err(tr!("The file has no \"layers\"."));
    };
    for layer in layers {
        let mut out = ImportLayer {
            name: text(layer.get("name")),
            color: layer
                .get("color")
                .and_then(Value::as_str)
                .and_then(Color::parse),
            notes: text(layer.get("notes")),
            visible: layer.get("visible").and_then(Value::as_bool),
            role: LayerRole::parse(&text(layer.get("role"))),
            annotations: Vec::new(),
        };
        if layer.get("color").is_some() && out.color.is_none() {
            doc.warnings.push(tr!(
                "Layer \"{}\" has an unreadable colour; a palette colour is used.",
                out.name
            ));
        }
        for a in layer
            .get("annotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(geometry) = a
                .get("geometry")
                .and_then(|g| serde_json::from_value::<Geometry>(g.clone()).ok())
            else {
                // Translators: a reason why an annotation of an imported file was skipped.
                doc.skip(tr!("unreadable geometry"));
                continue;
            };
            let mut ann = ImportAnnotation::new(geometry);
            ann.class = a.get("class").and_then(Value::as_str).map(str::to_string);
            if let Some(class) = &ann.class {
                doc.note_class(&class.clone(), None);
            }
            ann.name = text(a.get("name"));
            ann.description = text(a.get("description"));
            ann.status = a
                .get("status")
                .and_then(Value::as_str)
                .and_then(Status::parse)
                .unwrap_or_default();
            ann.author = text(a.get("author"));
            ann.locked = a.get("locked").and_then(Value::as_bool).unwrap_or(false);
            ann.properties = properties(a.get("properties"));
            ann.comments = super::parse_comments(a.get("comments"));
            ann.created_at = a.get("created_at").and_then(parse_time);
            ann.updated_at = a.get("updated_at").and_then(parse_time);
            out.annotations.push(ann);
        }
        doc.layers.push(out);
    }
    Ok(doc)
}

/// The export format of the pre-1.0 plugin.
fn parse_v1(value: &Value) -> Result<ImportDoc, String> {
    let mut doc = ImportDoc {
        format: "eov JSON (pre-1.0)",
        ..Default::default()
    };
    doc.slide.legacy_sha256 = value
        .get("file_sha256")
        .or_else(|| value.get("fingerprint_hex"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(layers) = value.get("annotation_layers").and_then(Value::as_array) else {
        return Err(tr!("The file has no \"annotation_layers\"."));
    };
    for layer in layers {
        let mut out = ImportLayer {
            name: text(layer.get("name")),
            color: layer
                .get("color_hex")
                .and_then(Value::as_str)
                .and_then(Color::parse),
            notes: text(layer.get("notes")),
            visible: None,
            role: LayerRole::Normal,
            annotations: Vec::new(),
        };
        for a in layer
            .get("annotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let geometry = match a.get("type").and_then(Value::as_str) {
                Some("point") => match (
                    a.get("x_level0").and_then(num),
                    a.get("y_level0").and_then(num),
                ) {
                    (Some(x), Some(y)) => Geometry::Point { x, y },
                    _ => {
                        doc.skip(tr!("point without coordinates"));
                        continue;
                    }
                },
                Some("polygon") => {
                    let vertices: Option<Vec<[f64; 2]>> = a
                        .get("vertices")
                        .and_then(Value::as_array)
                        .map(|vs| {
                            vs.iter()
                                .map(|v| {
                                    Some([
                                        v.get("x_level0").and_then(num)?,
                                        v.get("y_level0").and_then(num)?,
                                    ])
                                })
                                .collect()
                        })
                        .unwrap_or(None);
                    match vertices {
                        Some(exterior) => Geometry::Polygon {
                            exterior,
                            holes: Vec::new(),
                        },
                        None => {
                            doc.skip(tr!("polygon with unreadable vertices"));
                            continue;
                        }
                    }
                }
                _ => {
                    doc.skip(tr!("unknown annotation type"));
                    continue;
                }
            };
            let mut ann = ImportAnnotation::new(geometry);
            ann.properties = properties(a.get("metadata"));
            ann.created_at = a.get("created_at").and_then(parse_time);
            ann.updated_at = a.get("updated_at").and_then(parse_time);
            out.annotations.push(ann);
        }
        doc.layers.push(out);
    }
    Ok(doc)
}

fn parse_archive(value: &Value) -> Result<ImportDoc, String> {
    let mut doc = ImportDoc {
        format: "eov archive",
        ..Default::default()
    };
    for class in value
        .get("classes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = text(class.get("name"));
        let color = class
            .get("color")
            .and_then(Value::as_str)
            .and_then(Color::parse);
        doc.note_class(&name, color);
    }
    let slides = value
        .get("slides")
        .and_then(Value::as_array)
        .ok_or_else(|| tr!("The archive has no \"slides\"."))?;
    for slide in slides {
        match parse_v2(slide) {
            Ok(entry) => doc.entries.push(entry),
            Err(e) => doc
                .warnings
                .push(tr!("An archive entry was skipped: {}", e)),
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LayerRole;

    fn sample() -> (ExportSlide, Vec<Class>) {
        let class = Class {
            id: "c".into(),
            name: "Tumour".into(),
            color: Color(200, 10, 10),
            hotkey: None,
            visible: true,
            opacity: 1.0,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        let layer = Layer {
            id: "L".into(),
            slide_id: 1,
            name: "Regions".into(),
            notes: "n".into(),
            color: Color(1, 2, 3),
            visible: true,
            opacity: 0.5,
            locked: false,
            role: LayerRole::Normal,
            sort_order: 0,
            created_at: 1_000,
            updated_at: 2_000,
        };
        let a = Annotation {
            id: "a".into(),
            layer_id: "L".into(),
            class_id: Some("c".into()),
            geometry: Geometry::Ellipse {
                cx: 10.0,
                cy: 20.0,
                rx: 5.0,
                ry: 6.0,
            },
            name: "Focus".into(),
            description: "desc".into(),
            status: Status::Verified,
            author: "Dr. A".into(),
            locked: true,
            properties: vec![("grade".into(), "3".into())],
            comments: vec![],
            created_at: 1_792_056_600_000,
            updated_at: 1_792_056_600_000,
        };
        let slide = ExportSlide {
            record: SlideRecord {
                id: 1,
                identity: Some("eov-slide-1:abc".into()),
                filename: "/secret/MRN-12345/case.svs".into(),
                width: 1000,
                height: 2000,
                mpp_x: Some(0.25),
                mpp_y: Some(0.25),
                ..Default::default()
            },
            quickhash1: Some("qh".into()),
            bounds: None,
            layers: vec![(layer, vec![Arc::new(a)])],
        };
        (slide, vec![class])
    }

    #[test]
    fn exports_are_self_describing_and_leak_no_paths() {
        let (slide, classes) = sample();
        let doc = export(&slide, &classes, &ExportOptions::default());
        let text = doc.to_string();
        assert_eq!(doc["format"], FORMAT);
        assert_eq!(doc["format_version"], 2);
        assert_eq!(doc["coordinates"]["y_axis"], "down");
        assert_eq!(doc["slide"]["width"], 1000);
        assert_eq!(doc["slide"]["mpp_x"], 0.25);
        assert!(
            !text.contains("secret"),
            "no paths or file names by default"
        );
        let with_name = export(
            &slide,
            &classes,
            &ExportOptions {
                include_filename: true,
                ..Default::default()
            },
        );
        assert!(with_name["slide"]["filename"].as_str().is_some());
    }

    #[test]
    fn format_2_round_trips() {
        let (slide, classes) = sample();
        let doc = export(&slide, &classes, &ExportOptions::default());
        let parsed = parse(&doc).unwrap();
        assert_eq!(parsed.slide.identity.as_deref(), Some("eov-slide-1:abc"));
        assert_eq!(parsed.layers.len(), 1);
        let a = &parsed.layers[0].annotations[0];
        assert_eq!(
            a.geometry,
            Geometry::Ellipse {
                cx: 10.0,
                cy: 20.0,
                rx: 5.0,
                ry: 6.0
            }
        );
        assert_eq!(a.class.as_deref(), Some("Tumour"));
        assert_eq!(a.status, Status::Verified);
        assert!(a.locked);
        assert_eq!(a.properties, vec![("grade".to_string(), "3".to_string())]);
        assert_eq!(a.created_at, Some(1_792_056_600_000));
        assert_eq!(parsed.classes[0].color, Some(Color(200, 10, 10)));
    }

    #[test]
    fn pre_1_0_exports_are_read() {
        let v1 = json!({
            "file_path": "/data/case.svs",
            "file_sha256": "ab".repeat(32),
            "metadata": {"author": "x"},
            "annotation_layers": [{
                "id": "L", "name": "Mitoses", "notes": null, "color_hex": "#FF0000",
                "created_at": 1, "updated_at": 2,
                "annotations": [
                    {"type": "point", "id": "p", "created_at": 1, "updated_at": 1,
                     "x_level0": 5.0, "y_level0": 6.0,
                     "metadata": [{"key": "k", "value": "v"}]},
                    {"type": "polygon", "id": "g", "created_at": 1, "updated_at": 1,
                     "vertices": [{"x_level0": 0, "y_level0": 0}, {"x_level0": 10, "y_level0": 0},
                                  {"x_level0": 10, "y_level0": 10}]},
                    {"type": "polygon", "id": "bad", "vertices": [{"x_level0": "x"}]},
                    {"type": "bitmask", "id": "b"}
                ]
            }]
        });
        let doc = parse(&v1).unwrap();
        assert_eq!(
            doc.slide.legacy_sha256.as_deref(),
            Some("ab".repeat(32).as_str())
        );
        assert_eq!(doc.layers[0].annotations.len(), 2);
        assert_eq!(doc.layers[0].annotations[0].properties[0].0, "k");
        assert_eq!(doc.layers[0].color, Some(Color(255, 0, 0)));
        assert_eq!(doc.skipped.values().sum::<usize>(), 2);
    }

    #[test]
    fn newer_formats_are_refused_with_a_message() {
        let future = json!({"format": FORMAT, "format_version": 99, "layers": []});
        assert!(parse(&future).unwrap_err().contains("Update the plugin"));
    }

    #[test]
    fn archives_hold_one_entry_per_slide() {
        let (slide, classes) = sample();
        let doc = archive(&[slide], &classes, &ExportOptions::default());
        let parsed = parse(&doc).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].layers.len(), 1);
    }
}
