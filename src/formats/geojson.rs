//! GeoJSON, as QuPath reads and writes it.
//!
//! - Coordinates are level-0 pixels, y down (QuPath's image space).
//! - `properties.classification` is `{ "name", "color": [r, g, b] }`
//!   (QuPath ≥ 0.4; `colorRGB` and `names` of older versions are read).
//! - `properties.name`, `isLocked`, `metadata` and `measurements` are read.
//! - Rectangles and ellipses are exported as polygons (QuPath has no
//!   ellipse geometry in GeoJSON); `properties.eov` keeps the exact shape,
//!   layer, status and provenance so a round trip through eov is lossless.

use super::eov::{ExportOptions, ExportSlide, coordinates, slide_json};
use super::{ImportAnnotation, ImportDoc, ImportLayer, coord, num, parse_time};
use crate::geometry::ellipse_ring;
use crate::model::{Annotation, Class, Color, Geometry, Layer, Status};
use serde_json::{Map, Value, json};

const GEOMETRY_TYPES: &[&str] = &[
    "Point",
    "MultiPoint",
    "LineString",
    "MultiLineString",
    "Polygon",
    "MultiPolygon",
    "GeometryCollection",
];

pub fn is_geojson(value: &Value) -> bool {
    match value {
        Value::Object(o) => o.get("type").and_then(Value::as_str).is_some_and(|t| {
            t == "FeatureCollection" || t == "Feature" || GEOMETRY_TYPES.contains(&t)
        }),
        Value::Array(items) => {
            items
                .first()
                .and_then(|f| f.get("type"))
                .and_then(Value::as_str)
                == Some("Feature")
        }
        _ => false,
    }
}

fn ring(value: &Value) -> Option<Vec<[f64; 2]>> {
    let mut points: Vec<[f64; 2]> = value.as_array()?.iter().map(coord).collect::<Option<_>>()?;
    // GeoJSON rings repeat the first point at the end.
    if points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    Some(points)
}

fn polygon(value: &Value) -> Option<Geometry> {
    let rings: Vec<Vec<[f64; 2]>> = value.as_array()?.iter().map(ring).collect::<Option<_>>()?;
    let mut rings = rings.into_iter();
    let exterior = rings.next()?;
    Some(Geometry::Polygon {
        exterior,
        holes: rings.collect(),
    })
}

/// Geometries of a GeoJSON geometry object (multi-geometries split).
fn geometries(value: &Value, out: &mut Vec<Geometry>) -> Result<(), &'static str> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or("geometry without a type")?;
    let coords = value.get("coordinates");
    match kind {
        "Point" => {
            let [x, y] = coords.and_then(coord).ok_or("unreadable point")?;
            out.push(Geometry::Point { x, y });
        }
        "MultiPoint" => {
            for p in coords
                .and_then(Value::as_array)
                .ok_or("unreadable points")?
            {
                let [x, y] = coord(p).ok_or("unreadable point")?;
                out.push(Geometry::Point { x, y });
            }
        }
        "LineString" => {
            let points = coords
                .and_then(Value::as_array)
                .ok_or("unreadable line")?
                .iter()
                .map(coord)
                .collect::<Option<Vec<_>>>()
                .ok_or("unreadable line")?;
            out.push(Geometry::Polyline { points });
        }
        "MultiLineString" => {
            for line in coords.and_then(Value::as_array).ok_or("unreadable lines")? {
                let points = line
                    .as_array()
                    .ok_or("unreadable line")?
                    .iter()
                    .map(coord)
                    .collect::<Option<Vec<_>>>()
                    .ok_or("unreadable line")?;
                out.push(Geometry::Polyline { points });
            }
        }
        "Polygon" => out.push(coords.and_then(polygon).ok_or("unreadable polygon")?),
        "MultiPolygon" => {
            for p in coords
                .and_then(Value::as_array)
                .ok_or("unreadable polygons")?
            {
                out.push(polygon(p).ok_or("unreadable polygon")?);
            }
        }
        "GeometryCollection" => {
            for g in value
                .get("geometries")
                .and_then(Value::as_array)
                .ok_or("unreadable geometry collection")?
            {
                geometries(g, out)?;
            }
        }
        _ => return Err("unsupported geometry type"),
    }
    Ok(())
}

/// QuPath colours: `[r, g, b]` or a packed (A)RGB integer.
fn color(value: Option<&Value>) -> Option<Color> {
    match value? {
        Value::Array(c) if c.len() >= 3 => {
            let channel = |v: &Value| num(v).map(|f| f.clamp(0.0, 255.0) as u8);
            Some(Color(channel(&c[0])?, channel(&c[1])?, channel(&c[2])?))
        }
        Value::Number(n) => {
            let v = n.as_i64()? as u32;
            Some(Color((v >> 16) as u8, (v >> 8) as u8, v as u8))
        }
        Value::String(s) => Color::parse(s),
        _ => None,
    }
}

fn classification(props: &Map<String, Value>) -> (Option<String>, Option<Color>) {
    let Some(c) = props.get("classification") else {
        return (None, None);
    };
    match c {
        Value::String(name) => (Some(name.clone()), None),
        Value::Object(o) => {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    o.get("names").and_then(Value::as_array).map(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(": ")
                    })
                })
                .filter(|n| !n.trim().is_empty());
            (
                name,
                color(o.get("color")).or_else(|| color(o.get("colorRGB"))),
            )
        }
        _ => (None, None),
    }
}

fn string_map(value: Option<&Value>, out: &mut Vec<(String, String)>) {
    match value {
        Some(Value::Object(map)) => {
            for (k, v) in map {
                let v = v
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string());
                out.push((k.clone(), v));
            }
        }
        // QuPath < 0.4: [{"name": …, "value": …}]
        Some(Value::Array(rows)) => {
            for row in rows {
                if let (Some(name), Some(value)) =
                    (row.get("name").and_then(Value::as_str), row.get("value"))
                {
                    out.push((name.to_string(), value.to_string()));
                }
            }
        }
        _ => {}
    }
}

pub fn parse(value: &Value, stem: &str) -> Result<ImportDoc, String> {
    let mut doc = ImportDoc {
        format: "GeoJSON",
        ..Default::default()
    };
    let features: Vec<Value> = match value {
        Value::Array(items) => items.clone(),
        Value::Object(o) => match o.get("type").and_then(Value::as_str) {
            Some("FeatureCollection") => o
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .ok_or("The FeatureCollection has no features.")?,
            Some("Feature") => vec![value.clone()],
            Some(_) => vec![json!({ "type": "Feature", "geometry": value, "properties": {} })],
            None => return Err("Not GeoJSON.".to_string()),
        },
        _ => return Err("Not GeoJSON.".to_string()),
    };
    if let Some(eov) = value.get("eov").and_then(|e| e.get("slide")) {
        doc.slide.identity = eov
            .get("identity")
            .and_then(Value::as_str)
            .map(str::to_string);
        doc.slide.width = eov.get("width").and_then(Value::as_u64);
        doc.slide.height = eov.get("height").and_then(Value::as_u64);
        doc.slide.mpp_x = eov.get("mpp_x").and_then(num);
        doc.slide.mpp_y = eov.get("mpp_y").and_then(num);
    }
    let mut layer_index: Vec<(String, usize)> = Vec::new();
    for feature in &features {
        if feature.get("type").and_then(Value::as_str) != Some("Feature") {
            doc.skip("not a GeoJSON feature");
            continue;
        }
        let empty = Map::new();
        let props = feature
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let eov = props.get("eov").and_then(Value::as_object);
        let mut shapes = Vec::new();
        let exact = eov
            .and_then(|e| e.get("geometry"))
            .and_then(|g| serde_json::from_value::<Geometry>(g.clone()).ok());
        match exact {
            Some(g) => shapes.push(g),
            None => match feature.get("geometry") {
                Some(Value::Null) | None => {
                    doc.skip("feature without geometry");
                    continue;
                }
                Some(g) => {
                    if let Err(reason) = geometries(g, &mut shapes) {
                        doc.skip(reason);
                        continue;
                    }
                }
            },
        }
        let (class, class_color) = classification(props);
        if let Some(class) = &class {
            doc.note_class(class, class_color);
        }
        let object_type = props
            .get("objectType")
            .and_then(Value::as_str)
            .unwrap_or("annotation");
        let layer_name = eov
            .and_then(|e| e.get("layer"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| match object_type {
                "annotation" => stem.to_string(),
                other => format!("{stem} ({other}s)"),
            });
        let index = match layer_index.iter().find(|(n, _)| *n == layer_name) {
            Some((_, i)) => *i,
            None => {
                doc.layers.push(ImportLayer {
                    name: layer_name.clone(),
                    color: eov
                        .and_then(|e| e.get("layer_color"))
                        .and_then(Value::as_str)
                        .and_then(Color::parse),
                    ..Default::default()
                });
                layer_index.push((layer_name, doc.layers.len() - 1));
                doc.layers.len() - 1
            }
        };
        let mut properties = Vec::new();
        string_map(props.get("metadata"), &mut properties);
        string_map(props.get("measurements"), &mut properties);
        if properties.len() > crate::model::MAX_PROPERTIES {
            properties.truncate(crate::model::MAX_PROPERTIES);
            doc.skip("properties beyond the limit of 64 per annotation");
        }
        let text = |key: &str| {
            eov.and_then(|e| e.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default()
        };
        for geometry in shapes {
            let mut a = ImportAnnotation::new(geometry);
            a.class = class.clone();
            a.name = props
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            a.locked = props
                .get("isLocked")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            a.properties = properties.clone();
            a.description = text("description");
            a.author = text("author");
            a.status = Status::parse(&text("status")).unwrap_or_default();
            a.created_at = eov.and_then(|e| e.get("created_at")).and_then(parse_time);
            a.updated_at = eov.and_then(|e| e.get("updated_at")).and_then(parse_time);
            doc.layers[index].annotations.push(a);
        }
    }
    if doc.layers.is_empty() && doc.skipped.is_empty() {
        return Err("The GeoJSON file contains no features.".to_string());
    }
    Ok(doc)
}

fn closed(ring: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = ring.to_vec();
    if let Some(first) = ring.first() {
        out.push(*first);
    }
    out
}

fn geometry_json(g: &Geometry) -> Value {
    match g {
        Geometry::Point { x, y } | Geometry::Text { x, y } => {
            json!({"type": "Point", "coordinates": [x, y]})
        }
        Geometry::Polyline { points } => json!({"type": "LineString", "coordinates": points}),
        Geometry::Polygon { exterior, holes } => {
            let mut rings = vec![closed(exterior)];
            rings.extend(holes.iter().map(|h| closed(h)));
            json!({"type": "Polygon", "coordinates": rings})
        }
        Geometry::Rectangle {
            x,
            y,
            width,
            height,
        } => json!({"type": "Polygon", "coordinates": [closed(&[
            [*x, *y], [x + width, *y], [x + width, y + height], [*x, y + height]
        ])]}),
        Geometry::Ellipse { cx, cy, rx, ry } => {
            json!({"type": "Polygon", "coordinates": [closed(&ellipse_ring(*cx, *cy, *rx, *ry, 128))]})
        }
    }
}

fn feature(a: &Annotation, layer: &Layer, classes: &[Class]) -> Value {
    let class = a
        .class_id
        .as_deref()
        .and_then(|id| classes.iter().find(|c| c.id == id));
    let mut props = Map::new();
    props.insert("objectType".into(), json!("annotation"));
    if !a.name.trim().is_empty() {
        props.insert("name".into(), json!(a.name));
    }
    if let Some(class) = class {
        props.insert(
            "classification".into(),
            json!({"name": class.name, "color": [class.color.0, class.color.1, class.color.2]}),
        );
    }
    props.insert("isLocked".into(), json!(a.locked));
    if !a.properties.is_empty() {
        let metadata: Map<String, Value> = a
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        props.insert("metadata".into(), Value::Object(metadata));
    }
    props.insert(
        "eov".into(),
        json!({
            "kind": a.geometry.kind().as_str(),
            "geometry": a.geometry,
            "layer": layer.name,
            "layer_color": layer.color.to_hex(),
            "status": a.status.as_str(),
            "description": a.description,
            "author": a.author,
            "created_at": super::iso8601(a.created_at),
            "updated_at": super::iso8601(a.updated_at),
        }),
    );
    json!({
        "type": "Feature",
        "id": a.id,
        "geometry": geometry_json(&a.geometry),
        "properties": props,
    })
}

pub fn export(slide: &ExportSlide, classes: &[Class], options: &ExportOptions) -> Value {
    let features: Vec<Value> = slide
        .layers
        .iter()
        .flat_map(|(layer, annotations)| {
            annotations.iter().map(move |a| feature(a, layer, classes))
        })
        .collect();
    json!({
        "type": "FeatureCollection",
        "features": features,
        "eov": {
            "format_version": super::eov::FORMAT_VERSION,
            "generator": options.generator,
            "coordinates": coordinates(),
            "slide": slide_json(slide, options),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QuPath 0.5 export (pretty-printed, trimmed).
    const QUPATH: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {
          "type": "Feature",
          "id": "a7b3c0de-0000-4000-8000-000000000001",
          "geometry": {
            "type": "Polygon",
            "coordinates": [[[100, 100], [400, 100], [400, 300], [100, 300], [100, 100]],
                            [[200, 150], [300, 150], [300, 250], [200, 250], [200, 150]]]
          },
          "properties": {
            "objectType": "annotation",
            "name": "Hotspot",
            "classification": {"name": "Tumor", "color": [200, 0, 0]},
            "isLocked": true,
            "measurements": {"Area µm^2": 1234.5}
          }
        },
        {
          "type": "Feature",
          "id": "a7b3c0de-0000-4000-8000-000000000002",
          "geometry": {"type": "MultiPoint", "coordinates": [[10, 10], [20, 20], [30, 30]]},
          "properties": {"objectType": "annotation", "classification": {"names": ["Positive"], "colorRGB": -16711936}}
        },
        {
          "type": "Feature",
          "geometry": {"type": "Polygon", "coordinates": [[[0, 0], [5, 0], [0, 5], [0, 0]]]},
          "properties": {"objectType": "detection"}
        },
        {"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [[["a", 0]]]}, "properties": {}},
        {"type": "Feature", "geometry": null, "properties": {}}
      ]
    }"#;

    #[test]
    fn qupath_exports_are_read() {
        let value: Value = serde_json::from_str(QUPATH).unwrap();
        assert!(is_geojson(&value));
        let doc = parse(&value, "case").unwrap();
        assert_eq!(
            doc.layers.len(),
            2,
            "annotations and detections get separate layers"
        );
        let main = &doc.layers[0];
        assert_eq!(main.name, "case");
        assert_eq!(main.annotations.len(), 4);
        let hotspot = &main.annotations[0];
        assert_eq!(hotspot.name, "Hotspot");
        assert_eq!(hotspot.class.as_deref(), Some("Tumor"));
        assert!(hotspot.locked);
        let Geometry::Polygon { exterior, holes } = &hotspot.geometry else {
            panic!()
        };
        assert_eq!(exterior.len(), 4, "the closing vertex is dropped");
        assert_eq!(holes.len(), 1);
        assert_eq!(hotspot.properties[0].0, "Area µm^2");
        assert_eq!(doc.layers[1].name, "case (detections)");
        assert_eq!(doc.classes.len(), 2);
        assert_eq!(doc.classes[1].color, Some(Color(0, 255, 0)));
        assert_eq!(doc.skipped.values().sum::<usize>(), 2);
    }

    #[test]
    fn bare_geometries_and_feature_arrays_are_accepted() {
        let point = json!({"type": "Point", "coordinates": [1, 2]});
        assert_eq!(parse(&point, "x").unwrap().annotation_count(), 1);
        let array = json!([{"type": "Feature", "geometry": point, "properties": null}]);
        assert!(is_geojson(&array));
        assert_eq!(parse(&array, "x").unwrap().annotation_count(), 1);
        assert!(parse(&json!({"type": "FeatureCollection", "features": []}), "x").is_err());
    }

    #[test]
    fn exports_round_trip_exactly() {
        use crate::model::LayerRole;
        use std::sync::Arc;
        let layer = Layer {
            id: "L".into(),
            slide_id: 1,
            name: "Regions".into(),
            notes: String::new(),
            color: Color(1, 2, 3),
            visible: true,
            opacity: 1.0,
            locked: false,
            role: LayerRole::Normal,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        let ellipse = Annotation {
            id: "e".into(),
            layer_id: "L".into(),
            class_id: None,
            geometry: Geometry::Ellipse {
                cx: 50.0,
                cy: 50.0,
                rx: 10.0,
                ry: 20.0,
            },
            name: String::new(),
            description: "d".into(),
            status: Status::Rejected,
            author: "A".into(),
            locked: false,
            properties: vec![("k".into(), "v".into())],
            created_at: 1_792_056_600_000,
            updated_at: 1_792_056_600_000,
        };
        let slide = ExportSlide {
            record: Default::default(),
            quickhash1: None,
            bounds: None,
            layers: vec![(layer, vec![Arc::new(ellipse.clone())])],
        };
        let out = export(&slide, &[], &ExportOptions::default());
        // Readers that ignore the extension get a closed polygon.
        assert_eq!(out["features"][0]["geometry"]["type"], "Polygon");
        let ring = out["features"][0]["geometry"]["coordinates"][0]
            .as_array()
            .unwrap();
        assert_eq!(ring.first(), ring.last());
        let doc = parse(&out, "x").unwrap();
        let a = &doc.layers[0].annotations[0];
        assert_eq!(doc.layers[0].name, "Regions");
        assert_eq!(a.geometry, ellipse.geometry);
        assert_eq!(a.status, Status::Rejected);
        assert_eq!(a.description, "d");
        assert_eq!(a.properties, ellipse.properties);
    }
}
