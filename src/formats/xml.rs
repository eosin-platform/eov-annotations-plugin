//! ASAP and Aperio ImageScope XML annotations (import).
//!
//! Both use level-0 pixel coordinates with y down.
//!
//! - ASAP: `<ASAP_Annotations><Annotations><Annotation Name Type
//!   PartOfGroup Color><Coordinates><Coordinate Order X Y/>…`, groups in
//!   `<AnnotationGroups>`. Groups become classes.
//! - ImageScope: `<Annotations MicronsPerPixel><Annotation Id Name
//!   LineColor><Regions><Region Type NegativeROA Text><Vertices><Vertex X
//!   Y/>…`. Each annotation becomes a layer; `LineColor` is a Windows
//!   `COLORREF` (0x00BBGGRR); negative regions become holes of the region
//!   that contains them.

use super::{ImportAnnotation, ImportDoc, ImportLayer};
use crate::geometry::point_in_ring;
use crate::model::{Color, Geometry};
use roxmltree::{Document, Node};

pub fn parse(text: &str, stem: &str) -> Result<ImportDoc, String> {
    let document = Document::parse(text).map_err(|e| format!("The XML is not well-formed: {e}"))?;
    let root = document.root_element();
    match root.tag_name().name() {
        "ASAP_Annotations" => Ok(parse_asap(root, stem)),
        "Annotations" => Ok(parse_imagescope(root, stem)),
        other => Err(format!(
            "Unsupported XML annotations (root element <{other}>); ASAP and Aperio ImageScope files are supported."
        )),
    }
}

fn attr_f64(node: Node<'_, '_>, name: &str) -> Option<f64> {
    node.attribute(name)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |n| n.is_element() && n.tag_name().name() == name)
}

fn parse_asap(root: Node<'_, '_>, stem: &str) -> ImportDoc {
    let mut doc = ImportDoc {
        format: "ASAP XML",
        ..Default::default()
    };
    for groups in children(root, "AnnotationGroups") {
        for group in children(groups, "Group") {
            if let Some(name) = group.attribute("Name") {
                doc.note_class(name, group.attribute("Color").and_then(Color::parse));
            }
        }
    }
    let mut layer = ImportLayer {
        name: stem.to_string(),
        ..Default::default()
    };
    for annotations in children(root, "Annotations") {
        for a in children(annotations, "Annotation") {
            let mut coordinates: Vec<(f64, [f64; 2])> = Vec::new();
            let mut bad = false;
            for list in children(a, "Coordinates") {
                for (i, c) in children(list, "Coordinate").enumerate() {
                    match (attr_f64(c, "X"), attr_f64(c, "Y")) {
                        (Some(x), Some(y)) => {
                            let order = attr_f64(c, "Order").unwrap_or(i as f64);
                            coordinates.push((order, [x, y]));
                        }
                        _ => bad = true,
                    }
                }
            }
            if bad || coordinates.is_empty() {
                doc.skip("annotation with unreadable coordinates");
                continue;
            }
            coordinates.sort_by(|a, b| a.0.total_cmp(&b.0));
            let points: Vec<[f64; 2]> = coordinates.into_iter().map(|(_, p)| p).collect();
            let kind = a.attribute("Type").unwrap_or("Polygon");
            let shapes: Vec<Geometry> = match kind {
                "Dot" => vec![Geometry::Point {
                    x: points[0][0],
                    y: points[0][1],
                }],
                "PointSet" => points
                    .iter()
                    .map(|p| Geometry::Point { x: p[0], y: p[1] })
                    .collect(),
                "Measurement" | "Line" => vec![Geometry::Polyline { points }],
                "Rectangle" if points.len() == 4 => {
                    let b = crate::model::Bounds::of_points(&points);
                    let axis_aligned = points.iter().all(|p| {
                        (p[0] == b.min_x || p[0] == b.max_x) && (p[1] == b.min_y || p[1] == b.max_y)
                    });
                    if axis_aligned {
                        vec![Geometry::Rectangle {
                            x: b.min_x,
                            y: b.min_y,
                            width: b.width(),
                            height: b.height(),
                        }]
                    } else {
                        vec![Geometry::Polygon {
                            exterior: points,
                            holes: vec![],
                        }]
                    }
                }
                // Splines are imported as their control polygon.
                "Polygon" | "Spline" | "Rectangle" => vec![Geometry::Polygon {
                    exterior: points,
                    holes: vec![],
                }],
                _ => {
                    doc.skip("unsupported ASAP annotation type");
                    continue;
                }
            };
            let group = a
                .attribute("PartOfGroup")
                .filter(|g| !g.is_empty() && *g != "None")
                .map(str::to_string);
            if let Some(group) = &group {
                doc.note_class(group, None);
            }
            for geometry in shapes {
                let mut ann = ImportAnnotation::new(geometry);
                ann.class = group.clone();
                ann.name = a.attribute("Name").unwrap_or_default().to_string();
                layer.annotations.push(ann);
            }
        }
    }
    doc.layers.push(layer);
    doc
}

/// A Windows COLORREF (0x00BBGGRR).
fn colorref(value: Option<&str>) -> Option<Color> {
    let v: i64 = value?.trim().parse().ok()?;
    let v = v as u32;
    Some(Color(v as u8, (v >> 8) as u8, (v >> 16) as u8))
}

fn parse_imagescope(root: Node<'_, '_>, stem: &str) -> ImportDoc {
    let mut doc = ImportDoc {
        format: "Aperio ImageScope XML",
        ..Default::default()
    };
    if let Some(mpp) = attr_f64(root, "MicronsPerPixel").filter(|m| *m > 0.0) {
        doc.slide.mpp_x = Some(mpp);
        doc.slide.mpp_y = Some(mpp);
    }
    for (index, a) in children(root, "Annotation").enumerate() {
        let name = a
            .attribute("Name")
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{stem} {}",
                    a.attribute("Id").unwrap_or(&(index + 1).to_string())
                )
            });
        let mut layer = ImportLayer {
            name,
            color: colorref(a.attribute("LineColor")),
            visible: a.attribute("Visible").map(|v| v != "0"),
            ..Default::default()
        };
        let mut description = String::new();
        for attributes in children(a, "Attributes") {
            for attribute in children(attributes, "Attribute") {
                if attribute.attribute("Name") == Some("Description") {
                    description = attribute.attribute("Value").unwrap_or_default().to_string();
                }
            }
        }
        layer.notes = description;
        let mut positives: Vec<ImportAnnotation> = Vec::new();
        let mut negatives: Vec<Vec<[f64; 2]>> = Vec::new();
        for regions in children(a, "Regions") {
            for region in children(regions, "Region") {
                let mut points = Vec::new();
                let mut bad = false;
                for vertices in children(region, "Vertices") {
                    for v in children(vertices, "Vertex") {
                        match (attr_f64(v, "X"), attr_f64(v, "Y")) {
                            (Some(x), Some(y)) => points.push([x, y]),
                            _ => bad = true,
                        }
                    }
                }
                if bad || points.is_empty() {
                    doc.skip("region with unreadable vertices");
                    continue;
                }
                let negative = region.attribute("NegativeROA") == Some("1");
                let kind = region.attribute("Type").unwrap_or("0");
                let geometry = match kind {
                    "1" => {
                        let b = crate::model::Bounds::of_points(&points);
                        Geometry::Rectangle {
                            x: b.min_x,
                            y: b.min_y,
                            width: b.width(),
                            height: b.height(),
                        }
                    }
                    "2" => {
                        let b = crate::model::Bounds::of_points(&points);
                        Geometry::Ellipse {
                            cx: (b.min_x + b.max_x) / 2.0,
                            cy: (b.min_y + b.max_y) / 2.0,
                            rx: b.width() / 2.0,
                            ry: b.height() / 2.0,
                        }
                    }
                    "3" | "4" => Geometry::Polyline { points },
                    "5" => Geometry::Point {
                        x: points[0][0],
                        y: points[0][1],
                    },
                    _ if points.len() == 1 => Geometry::Point {
                        x: points[0][0],
                        y: points[0][1],
                    },
                    _ => {
                        if negative {
                            negatives.push(points);
                            continue;
                        }
                        Geometry::Polygon {
                            exterior: points,
                            holes: vec![],
                        }
                    }
                };
                let mut ann = ImportAnnotation::new(geometry);
                ann.name = region.attribute("Text").unwrap_or_default().to_string();
                positives.push(ann);
            }
        }
        for hole in negatives {
            let host = positives.iter_mut().find(|p| match &p.geometry {
                Geometry::Polygon { exterior, .. } => {
                    hole.first().is_some_and(|q| point_in_ring(*q, exterior))
                }
                _ => false,
            });
            match host {
                Some(ImportAnnotation {
                    geometry: Geometry::Polygon { holes, .. },
                    ..
                }) => holes.push(hole),
                _ => doc.skip("negative region outside every region"),
            }
        }
        layer.annotations = positives;
        doc.layers.push(layer);
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASAP: &str = r##"<?xml version="1.0"?>
<ASAP_Annotations>
  <Annotations>
    <Annotation Name="Annotation 0" Type="Polygon" PartOfGroup="tumor" Color="#F4FA58">
      <Coordinates>
        <Coordinate Order="2" X="100" Y="200" />
        <Coordinate Order="0" X="0" Y="0" />
        <Coordinate Order="1" X="100" Y="0" />
      </Coordinates>
    </Annotation>
    <Annotation Name="Dot 1" Type="Dot" PartOfGroup="None" Color="#F4FA58">
      <Coordinates><Coordinate Order="0" X="5.5" Y="6.5" /></Coordinates>
    </Annotation>
    <Annotation Name="Box" Type="Rectangle" PartOfGroup="tumor">
      <Coordinates>
        <Coordinate Order="0" X="0" Y="0" /><Coordinate Order="1" X="10" Y="0" />
        <Coordinate Order="2" X="10" Y="5" /><Coordinate Order="3" X="0" Y="5" />
      </Coordinates>
    </Annotation>
    <Annotation Name="Bad" Type="Polygon"><Coordinates><Coordinate X="x" Y="1"/></Coordinates></Annotation>
  </Annotations>
  <AnnotationGroups>
    <Group Name="tumor" PartOfGroup="None" Color="#64FE2E"><Attributes /></Group>
  </AnnotationGroups>
</ASAP_Annotations>"##;

    const IMAGESCOPE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Annotations MicronsPerPixel="0.252000">
  <Annotation Id="1" Name="" LineColor="255" Visible="1">
    <Attributes><Attribute Name="Description" Id="0" Value="Tumour areas"/></Attributes>
    <Regions>
      <Region Id="1" Type="0" Text="Main" NegativeROA="0">
        <Vertices><Vertex X="0" Y="0" Z="0"/><Vertex X="100" Y="0"/><Vertex X="100" Y="100"/><Vertex X="0" Y="100"/></Vertices>
      </Region>
      <Region Id="2" Type="0" NegativeROA="1">
        <Vertices><Vertex X="40" Y="40"/><Vertex X="60" Y="40"/><Vertex X="60" Y="60"/><Vertex X="40" Y="60"/></Vertices>
      </Region>
      <Region Id="3" Type="2">
        <Vertices><Vertex X="200" Y="200"/><Vertex X="220" Y="240"/></Vertices>
      </Region>
      <Region Id="4" Type="4">
        <Vertices><Vertex X="0" Y="0"/><Vertex X="30" Y="40"/></Vertices>
      </Region>
    </Regions>
  </Annotation>
  <Annotation Id="2" Name="Lymphocytes" LineColor="65280">
    <Regions><Region Id="1" Type="0"><Vertices><Vertex X="1" Y="1"/></Vertices></Region></Regions>
  </Annotation>
</Annotations>"#;

    #[test]
    fn asap_files_are_read() {
        let doc = parse(ASAP, "slide").unwrap();
        assert_eq!(doc.format, "ASAP XML");
        let layer = &doc.layers[0];
        assert_eq!(layer.annotations.len(), 3);
        assert_eq!(
            layer.annotations[0].geometry,
            Geometry::Polygon {
                exterior: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 200.0]],
                holes: vec![]
            },
            "coordinates follow their Order"
        );
        assert_eq!(layer.annotations[0].class.as_deref(), Some("tumor"));
        assert_eq!(layer.annotations[1].class, None);
        assert_eq!(
            layer.annotations[2].geometry,
            Geometry::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0
            }
        );
        assert_eq!(doc.classes[0].color, Some(Color(0x64, 0xFE, 0x2E)));
        assert_eq!(doc.skipped.values().sum::<usize>(), 1);
    }

    #[test]
    fn imagescope_files_are_read() {
        let doc = parse(IMAGESCOPE, "slide").unwrap();
        assert_eq!(doc.slide.mpp_x, Some(0.252));
        assert_eq!(doc.layers.len(), 2);
        let first = &doc.layers[0];
        assert_eq!(first.color, Some(Color(255, 0, 0)), "COLORREF 255 is red");
        assert_eq!(first.notes, "Tumour areas");
        assert_eq!(first.annotations.len(), 3);
        let Geometry::Polygon { holes, .. } = &first.annotations[0].geometry else {
            panic!()
        };
        assert_eq!(holes.len(), 1, "the negative region became a hole");
        assert_eq!(first.annotations[0].name, "Main");
        assert!(
            matches!(first.annotations[1].geometry, Geometry::Ellipse { rx, ry, .. } if rx == 10.0 && ry == 20.0)
        );
        assert!(matches!(
            first.annotations[2].geometry,
            Geometry::Polyline { .. }
        ));
        assert_eq!(doc.layers[1].name, "Lymphocytes");
        assert_eq!(doc.layers[1].color, Some(Color(0, 255, 0)));
        assert!(matches!(
            doc.layers[1].annotations[0].geometry,
            Geometry::Point { .. }
        ));
    }

    #[test]
    fn malformed_xml_is_an_error_not_a_panic() {
        assert!(parse("<Annotations><Annotation>", "x").is_err());
        assert!(parse("<Other/>", "x").unwrap_err().contains("Unsupported"));
    }
}
