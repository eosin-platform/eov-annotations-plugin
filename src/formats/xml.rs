//! ASAP and Aperio ImageScope XML annotations (import and export).
//!
//! Both use level-0 pixel coordinates with y down.
//!
//! Exports add a few attributes of their own (`EovKind`, `EovLayer`,
//! `EovId`, `EovHoleOf` in ASAP; `Class`, `Description` and `EovKind`
//! region attributes in ImageScope), which those programs ignore, so that
//! what they cannot express (layers, holes in ASAP, ellipses, arrows and
//! text) comes back intact when the file is imported into eov again.
//!
//! - ASAP: `<ASAP_Annotations><Annotations><Annotation Name Type
//!   PartOfGroup Color><Coordinates><Coordinate Order X Y/>…`, groups in
//!   `<AnnotationGroups>`. Groups become classes.
//! - ImageScope: `<Annotations MicronsPerPixel><Annotation Id Name
//!   LineColor><Regions><Region Type NegativeROA Text><Vertices><Vertex X
//!   Y/>…`. Each annotation becomes a layer; `LineColor` is a Windows
//!   `COLORREF` (0x00BBGGRR); negative regions become holes of the region
//!   that contains them.

use super::eov::ExportSlide;
use super::{ImportAnnotation, ImportDoc, ImportLayer};
use crate::geometry::point_in_ring;
use crate::model::{Class, Color, Geometry};
use roxmltree::{Document, Node};

pub fn parse(text: &str, stem: &str) -> Result<ImportDoc, String> {
    let document = Document::parse(text).map_err(|e| format!("The XML is not well-formed: {e}"))?;
    let root = document.root_element();
    if super::ndpa::is_ndpa(root) {
        return Ok(super::ndpa::parse(root, stem));
    }
    match root.tag_name().name() {
        "ASAP_Annotations" => Ok(parse_asap(root, stem)),
        "Annotations" => Ok(parse_imagescope(root, stem)),
        other => Err(format!(
            "Unsupported XML annotations (root element <{other}>); ASAP, Aperio ImageScope and Hamamatsu NDPA files are supported."
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
    // Layers by name (`EovLayer`, from eov's own exports); the rest go
    // into one layer named after the file.
    let mut layers: Vec<ImportLayer> = vec![ImportLayer {
        name: stem.to_string(),
        ..Default::default()
    }];
    // Holes exported as separate polygons: (parent id, ring).
    let mut holes: Vec<(String, Vec<[f64; 2]>)> = Vec::new();
    // Where each `EovId` landed: (layer index, annotation index).
    let mut ids: std::collections::HashMap<String, (usize, usize)> = Default::default();
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
            if let Some(parent) = a.attribute("EovHoleOf") {
                holes.push((parent.to_string(), points));
                continue;
            }
            let kind = a.attribute("Type").unwrap_or("Polygon");
            let shapes: Vec<Geometry> = match (a.attribute("EovKind"), kind) {
                (Some("text"), _) => vec![Geometry::Text {
                    x: points[0][0],
                    y: points[0][1],
                }],
                (Some("arrow"), _) if points.len() >= 2 => vec![Geometry::Arrow { points }],
                (Some("polyline"), _) if points.len() >= 2 => vec![Geometry::Polyline { points }],
                (Some("ellipse"), _) => {
                    let b = crate::model::Bounds::of_points(&points);
                    vec![Geometry::Ellipse {
                        cx: (b.min_x + b.max_x) / 2.0,
                        cy: (b.min_y + b.max_y) / 2.0,
                        rx: b.width() / 2.0,
                        ry: b.height() / 2.0,
                    }]
                }
                (_, kind) => match kind {
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
                            (p[0] == b.min_x || p[0] == b.max_x)
                                && (p[1] == b.min_y || p[1] == b.max_y)
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
                },
            };
            let group = a
                .attribute("PartOfGroup")
                .filter(|g| !g.is_empty() && *g != "None")
                .map(str::to_string);
            if let Some(group) = &group {
                doc.note_class(group, None);
            }
            let layer = match a.attribute("EovLayer").filter(|l| !l.trim().is_empty()) {
                Some(name) => match layers.iter().position(|l| l.name == name) {
                    Some(index) => index,
                    None => {
                        layers.push(ImportLayer {
                            name: name.to_string(),
                            ..Default::default()
                        });
                        layers.len() - 1
                    }
                },
                None => 0,
            };
            for geometry in shapes {
                let mut ann = ImportAnnotation::new(geometry);
                ann.class = group.clone();
                ann.name = a.attribute("Name").unwrap_or_default().to_string();
                if let Some(id) = a.attribute("EovId") {
                    ids.insert(id.to_string(), (layer, layers[layer].annotations.len()));
                }
                layers[layer].annotations.push(ann);
            }
        }
    }
    for (parent, ring) in holes {
        match ids.get(&parent).copied() {
            Some((layer, index)) => match &mut layers[layer].annotations[index].geometry {
                Geometry::Polygon { holes, .. } => holes.push(ring),
                _ => doc.skip("hole of a shape that is not a polygon"),
            },
            None => doc.skip("hole of a missing polygon"),
        }
    }
    doc.layers = layers
        .into_iter()
        .enumerate()
        .filter(|(i, l)| *i > 0 || !l.annotations.is_empty())
        .map(|(_, l)| l)
        .collect();
    if doc.layers.is_empty() {
        doc.layers.push(ImportLayer {
            name: stem.to_string(),
            ..Default::default()
        });
    }
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
                    // Arrows run from the first vertex (tail) to the last
                    // (tip); rulers are lines.
                    "3" => Geometry::Arrow { points },
                    "4" => Geometry::Polyline { points },
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
                for attributes in children(region, "Attributes") {
                    for attribute in children(attributes, "Attribute") {
                        let value = attribute.attribute("Value").unwrap_or_default();
                        match attribute.attribute("Name") {
                            Some("Class") if !value.trim().is_empty() => {
                                doc.note_class(value, None);
                                ann.class = Some(value.trim().to_string());
                            }
                            Some("Description") => ann.description = value.to_string(),
                            Some("EovKind") if value == "text" => {
                                if let Geometry::Point { x, y } = ann.geometry {
                                    ann.geometry = Geometry::Text { x, y };
                                }
                            }
                            _ => {}
                        }
                    }
                }
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

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// An XML attribute value.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A coordinate, written so that it reads back exactly.
fn num(v: f64) -> String {
    format!("{v}")
}

fn class_of<'a>(classes: &'a [Class], id: Option<&str>) -> Option<&'a Class> {
    let id = id?;
    classes.iter().find(|c| c.id == id)
}

/// Polygon vertices of an ellipse (64; the extremes are exact, so the
/// bounding box gives the ellipse back).
fn ellipse_points(cx: f64, cy: f64, rx: f64, ry: f64) -> Vec<[f64; 2]> {
    crate::geometry::ellipse_ring(cx, cy, rx, ry, 64)
}

fn rectangle_points(x: f64, y: f64, width: f64, height: f64) -> Vec<[f64; 2]> {
    vec![
        [x, y],
        [x + width, y],
        [x + width, y + height],
        [x, y + height],
    ]
}

/// A geometry as one of the formats writes it.
struct Shaped<'a, K> {
    /// The format's shape name or number.
    kind: K,
    /// The `EovKind` that restores what the format cannot express.
    eov_kind: Option<&'static str>,
    points: Vec<[f64; 2]>,
    holes: &'a [Vec<[f64; 2]>],
}

impl<'a, K> Shaped<'a, K> {
    fn new(kind: K, points: Vec<[f64; 2]>) -> Self {
        Self {
            kind,
            eov_kind: None,
            points,
            holes: &[],
        }
    }

    fn marked(mut self, eov_kind: &'static str) -> Self {
        self.eov_kind = Some(eov_kind);
        self
    }
}

fn asap_shape(geometry: &Geometry) -> Shaped<'_, &'static str> {
    match geometry {
        Geometry::Point { x, y } => Shaped::new("Dot", vec![[*x, *y]]),
        Geometry::Text { x, y } => Shaped::new("Dot", vec![[*x, *y]]).marked("text"),
        Geometry::Polyline { points } => {
            Shaped::new("Measurement", points.clone()).marked("polyline")
        }
        Geometry::Arrow { points } => Shaped::new("Measurement", points.clone()).marked("arrow"),
        Geometry::Polygon { exterior, holes } => Shaped {
            holes,
            ..Shaped::new("Polygon", exterior.clone())
        },
        Geometry::Rectangle {
            x,
            y,
            width,
            height,
        } => Shaped::new("Rectangle", rectangle_points(*x, *y, *width, *height)),
        Geometry::Ellipse { cx, cy, rx, ry } => {
            Shaped::new("Polygon", ellipse_points(*cx, *cy, *rx, *ry)).marked("ellipse")
        }
    }
}

/// ImageScope region types: 0 polygon, 1 rectangle, 2 ellipse, 3 arrow,
/// 4 ruler, 5 point.
fn imagescope_shape(geometry: &Geometry) -> Shaped<'_, u8> {
    match geometry {
        Geometry::Polygon { exterior, holes } => Shaped {
            holes,
            ..Shaped::new(0, exterior.clone())
        },
        Geometry::Rectangle {
            x,
            y,
            width,
            height,
        } => Shaped::new(1, rectangle_points(*x, *y, *width, *height)),
        Geometry::Ellipse { cx, cy, rx, ry } => {
            Shaped::new(2, vec![[cx - rx, cy - ry], [cx + rx, cy + ry]])
        }
        Geometry::Arrow { points } => Shaped::new(3, points.clone()),
        Geometry::Polyline { points } => Shaped::new(4, points.clone()),
        Geometry::Point { x, y } => Shaped::new(5, vec![[*x, *y]]),
        Geometry::Text { x, y } => Shaped::new(5, vec![[*x, *y]]).marked("text"),
    }
}

/// ASAP XML of a slide. Classes become groups. ASAP has no layers, holes,
/// ellipses, arrows or text: those are written as the nearest ASAP shape
/// with `Eov*` attributes that eov reads back.
pub fn export_asap(slide: &ExportSlide, classes: &[Class]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ASAP_Annotations>\n\t<Annotations>\n",
    );
    let mut used: Vec<&Class> = Vec::new();
    for (layer, annotations) in &slide.layers {
        for a in annotations {
            let class = class_of(classes, a.class_id.as_deref());
            if let Some(class) = class
                && !used.iter().any(|c| c.id == class.id)
            {
                used.push(class);
            }
            let color = class.map_or(layer.color, |c| c.color).to_hex();
            let group = class.map_or("None".to_string(), |c| c.name.clone());
            let Shaped {
                kind,
                eov_kind,
                points,
                holes,
            } = asap_shape(&a.geometry);
            let mut write = |name: &str, kind: &str, extra: &str, points: &[[f64; 2]]| {
                out.push_str(&format!(
                    "\t\t<Annotation Name=\"{}\" Type=\"{kind}\" PartOfGroup=\"{}\" Color=\"{color}\" EovLayer=\"{}\"{extra}>\n\t\t\t<Coordinates>\n",
                    esc(name),
                    esc(&group),
                    esc(&layer.name),
                ));
                for (i, p) in points.iter().enumerate() {
                    out.push_str(&format!(
                        "\t\t\t\t<Coordinate Order=\"{i}\" X=\"{}\" Y=\"{}\" />\n",
                        num(p[0]),
                        num(p[1])
                    ));
                }
                out.push_str("\t\t\t</Coordinates>\n\t\t</Annotation>\n");
            };
            let mut extra = format!(" EovId=\"{}\"", esc(&a.id));
            if let Some(eov_kind) = eov_kind {
                extra.push_str(&format!(" EovKind=\"{eov_kind}\""));
            }
            write(&a.name, kind, &extra, &points);
            for hole in holes {
                let name = format!("{} (hole)", a.display_name());
                write(
                    &name,
                    "Polygon",
                    &format!(" EovHoleOf=\"{}\"", esc(&a.id)),
                    hole,
                );
            }
        }
    }
    out.push_str("\t</Annotations>\n\t<AnnotationGroups>\n");
    for class in used {
        out.push_str(&format!(
            "\t\t<Group Name=\"{}\" PartOfGroup=\"None\" Color=\"{}\">\n\t\t\t<Attributes />\n\t\t</Group>\n",
            esc(&class.name),
            class.color.to_hex()
        ));
    }
    out.push_str("\t</AnnotationGroups>\n</ASAP_Annotations>\n");
    out
}

/// A Windows COLORREF (0x00BBGGRR).
fn to_colorref(color: Color) -> u32 {
    (color.2 as u32) << 16 | (color.1 as u32) << 8 | color.0 as u32
}

/// Aperio ImageScope XML of a slide: one `Annotation` per layer, one region
/// per annotation (holes as negative regions after it). The class, the
/// description and text labels are kept in region attributes.
pub fn export_imagescope(slide: &ExportSlide, classes: &[Class]) -> String {
    let mpp = slide.record.mpp_x.or(slide.record.mpp_y).unwrap_or(0.0);
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Annotations MicronsPerPixel=\"{}\">\n",
        num(mpp)
    );
    let mut region_id = 0usize;
    for (index, (layer, annotations)) in slide.layers.iter().enumerate() {
        out.push_str(&format!(
            "  <Annotation Id=\"{}\" Name=\"{}\" ReadOnly=\"0\" NameReadOnly=\"0\" LineColorReadOnly=\"0\" Incremental=\"0\" Type=\"4\" LineColor=\"{}\" Visible=\"{}\" Selected=\"0\" MarkupImagePath=\"\" MacroName=\"\">\n",
            index + 1,
            esc(&layer.name),
            to_colorref(layer.color),
            if layer.visible { 1 } else { 0 }
        ));
        out.push_str(&format!(
            "    <Attributes>\n      <Attribute Name=\"Description\" Id=\"0\" Value=\"{}\"/>\n    </Attributes>\n    <Regions>\n",
            esc(&layer.notes)
        ));
        for a in annotations {
            let Shaped {
                kind,
                eov_kind,
                points,
                holes,
            } = imagescope_shape(&a.geometry);
            let mut attributes = Vec::new();
            if let Some(class) = class_of(classes, a.class_id.as_deref()) {
                attributes.push(("Class", class.name.clone()));
            }
            if !a.description.is_empty() {
                attributes.push(("Description", a.description.clone()));
            }
            if let Some(eov_kind) = eov_kind {
                attributes.push(("EovKind", eov_kind.to_string()));
            }
            let mut region = |kind: u8,
                              text: &str,
                              negative: bool,
                              attributes: &[(&str, String)],
                              points: &[[f64; 2]]| {
                region_id += 1;
                out.push_str(&format!(
                    "      <Region Id=\"{region_id}\" Type=\"{kind}\" Zoom=\"1\" Selected=\"0\" ImageLocation=\"\" ImageFocus=\"-1\" Text=\"{}\" NegativeROA=\"{}\" InputRegionId=\"0\" Analyze=\"1\" DisplayId=\"{region_id}\">\n        <Attributes>\n",
                    esc(text),
                    if negative { 1 } else { 0 }
                ));
                for (i, (name, value)) in attributes.iter().enumerate() {
                    out.push_str(&format!(
                        "          <Attribute Name=\"{name}\" Id=\"{i}\" Value=\"{}\"/>\n",
                        esc(value)
                    ));
                }
                out.push_str("        </Attributes>\n        <Vertices>\n");
                for p in points {
                    out.push_str(&format!(
                        "          <Vertex X=\"{}\" Y=\"{}\" Z=\"0\"/>\n",
                        num(p[0]),
                        num(p[1])
                    ));
                }
                out.push_str("        </Vertices>\n      </Region>\n");
            };
            region(kind, &a.name, false, &attributes, &points);
            for hole in holes {
                region(0, "", true, &[], hole);
            }
        }
        out.push_str("    </Regions>\n    <Plots/>\n  </Annotation>\n");
    }
    out.push_str("</Annotations>\n");
    out
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

    // -- export round trips ---------------------------------------------------

    use crate::model::{Annotation, Layer, LayerRole, SlideRecord, Status};
    use std::sync::Arc;

    fn class(id: &str, name: &str, color: Color) -> Class {
        Class {
            id: id.into(),
            name: name.into(),
            color,
            hotkey: None,
            visible: true,
            opacity: 1.0,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn layer(id: &str, name: &str) -> Layer {
        Layer {
            id: id.into(),
            slide_id: 1,
            name: name.into(),
            notes: format!("notes of {name}"),
            color: Color(0x12, 0x34, 0x56),
            visible: true,
            opacity: 1.0,
            locked: false,
            role: LayerRole::Normal,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn annotation(
        id: &str,
        layer: &str,
        class: Option<&str>,
        name: &str,
        geometry: Geometry,
    ) -> Arc<Annotation> {
        Arc::new(Annotation {
            id: id.into(),
            layer_id: layer.into(),
            class_id: class.map(str::to_string),
            geometry,
            name: name.into(),
            description: if class.is_some() {
                format!("about {name}")
            } else {
                String::new()
            },
            status: Status::Draft,
            author: String::new(),
            locked: false,
            properties: vec![],
            comments: vec![],
            created_at: 0,
            updated_at: 0,
        })
    }

    fn sample() -> (ExportSlide, Vec<Class>) {
        let classes = vec![
            class("c1", "Tumour & stroma <1>", Color(0xD5, 0x5E, 0x00)),
            class("c2", "Stroma", Color(0x56, 0xB4, 0xE9)),
        ];
        let regions = vec![
            annotation(
                "p",
                "A",
                Some("c1"),
                "Region \"one\"",
                Geometry::Polygon {
                    exterior: vec![[0.5, 0.25], [1000.0, 0.0], [1000.0, 800.0], [0.0, 800.0]],
                    holes: vec![vec![
                        [100.0, 100.0],
                        [200.0, 100.0],
                        [200.0, 200.0],
                        [100.0, 200.0],
                    ]],
                },
            ),
            annotation(
                "r",
                "A",
                Some("c2"),
                "Box",
                Geometry::Rectangle {
                    x: 10.1,
                    y: 20.2,
                    width: 30.3,
                    height: 40.4,
                },
            ),
            annotation(
                "e",
                "A",
                None,
                "Focus",
                Geometry::Ellipse {
                    cx: 500.0,
                    cy: 400.0,
                    rx: 120.5,
                    ry: 60.25,
                },
            ),
        ];
        let marks = vec![
            annotation(
                "pt",
                "B",
                Some("c1"),
                "Mitosis 1",
                Geometry::Point { x: 12.5, y: 13.75 },
            ),
            annotation(
                "t",
                "B",
                None,
                "Margin",
                Geometry::Text { x: 50.0, y: 60.0 },
            ),
            annotation(
                "l",
                "B",
                None,
                "Path",
                Geometry::Polyline {
                    points: vec![[0.0, 0.0], [10.0, 5.0], [20.0, 0.0]],
                },
            ),
            annotation(
                "a",
                "B",
                Some("c2"),
                "Here",
                Geometry::Arrow {
                    points: vec![[300.0, 300.0], [350.0, 280.0]],
                },
            ),
        ];
        let slide = ExportSlide {
            record: SlideRecord {
                id: 1,
                width: 2000,
                height: 1000,
                mpp_x: Some(0.25),
                mpp_y: Some(0.25),
                ..Default::default()
            },
            quickhash1: None,
            bounds: None,
            layers: vec![
                (layer("A", "Regions"), regions),
                (layer("B", "Marks"), marks),
            ],
        };
        (slide, classes)
    }

    fn close(a: &Geometry, b: &Geometry) -> bool {
        let near = |x: f64, y: f64| (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0);
        let pts = |p: &[[f64; 2]], q: &[[f64; 2]]| {
            p.len() == q.len()
                && p.iter()
                    .zip(q)
                    .all(|(a, b)| near(a[0], b[0]) && near(a[1], b[1]))
        };
        match (a, b) {
            (Geometry::Point { x, y }, Geometry::Point { x: u, y: v })
            | (Geometry::Text { x, y }, Geometry::Text { x: u, y: v }) => {
                near(*x, *u) && near(*y, *v)
            }
            (Geometry::Polyline { points: p }, Geometry::Polyline { points: q })
            | (Geometry::Arrow { points: p }, Geometry::Arrow { points: q }) => pts(p, q),
            (
                Geometry::Polygon {
                    exterior: e,
                    holes: h,
                },
                Geometry::Polygon {
                    exterior: f,
                    holes: k,
                },
            ) => pts(e, f) && h.len() == k.len() && h.iter().zip(k).all(|(h, k)| pts(h, k)),
            (
                Geometry::Rectangle {
                    x,
                    y,
                    width,
                    height,
                },
                Geometry::Rectangle {
                    x: u,
                    y: v,
                    width: w,
                    height: z,
                },
            ) => near(*x, *u) && near(*y, *v) && near(*width, *w) && near(*height, *z),
            (
                Geometry::Ellipse { cx, cy, rx, ry },
                Geometry::Ellipse {
                    cx: u,
                    cy: v,
                    rx: w,
                    ry: z,
                },
            ) => near(*cx, *u) && near(*cy, *v) && near(*rx, *w) && near(*ry, *z),
            _ => false,
        }
    }

    /// Every exported annotation comes back with its shape, class, name and
    /// layer.
    fn assert_round_trip(
        doc: &ImportDoc,
        slide: &ExportSlide,
        classes: &[Class],
        descriptions: bool,
    ) {
        assert!(doc.skipped.is_empty(), "{:?}", doc.skipped);
        for (layer, annotations) in &slide.layers {
            let imported = doc
                .layers
                .iter()
                .find(|l| l.name == layer.name)
                .unwrap_or_else(|| panic!("layer {} is missing: {:?}", layer.name, doc.layers));
            assert_eq!(
                imported.annotations.len(),
                annotations.len(),
                "{}",
                layer.name
            );
            for a in annotations {
                let b = imported
                    .annotations
                    .iter()
                    .find(|b| b.name == a.name)
                    .unwrap_or_else(|| panic!("{} is missing", a.name));
                assert!(
                    close(&a.geometry, &b.geometry),
                    "{:?} != {:?}",
                    a.geometry,
                    b.geometry
                );
                let class = a
                    .class_id
                    .as_deref()
                    .and_then(|id| classes.iter().find(|c| c.id == id))
                    .map(|c| c.name.clone());
                assert_eq!(b.class, class, "{}", a.name);
                if descriptions {
                    assert_eq!(b.description, a.description, "{}", a.name);
                }
            }
        }
    }

    #[test]
    fn asap_exports_read_back_with_shapes_classes_names_and_layers() {
        let (slide, classes) = sample();
        let xml = export_asap(&slide, &classes);
        assert!(roxmltree::Document::parse(&xml).is_ok(), "{xml}");
        let doc = parse(&xml, "case").unwrap();
        assert_eq!(doc.format, "ASAP XML");
        assert_round_trip(&doc, &slide, &classes, false);
        // Groups carry the class colours.
        let tumour = doc
            .classes
            .iter()
            .find(|c| c.name == "Tumour & stroma <1>")
            .unwrap();
        assert_eq!(tumour.color, Some(Color(0xD5, 0x5E, 0x00)));
        // What ASAP itself sees: a polygon for the ellipse, a measurement for
        // the arrow, and the hole as a polygon of its own.
        assert!(xml.contains("Type=\"Measurement\""));
        assert!(xml.contains("EovHoleOf=\"p\""));
    }

    #[test]
    fn imagescope_exports_read_back_with_shapes_classes_names_and_layers() {
        let (slide, classes) = sample();
        let xml = export_imagescope(&slide, &classes);
        assert!(roxmltree::Document::parse(&xml).is_ok(), "{xml}");
        let doc = parse(&xml, "case").unwrap();
        assert_eq!(doc.format, "Aperio ImageScope XML");
        assert_eq!(doc.slide.mpp_x, Some(0.25));
        assert_round_trip(&doc, &slide, &classes, true);
        let regions = doc.layers.iter().find(|l| l.name == "Regions").unwrap();
        assert_eq!(regions.color, Some(Color(0x12, 0x34, 0x56)));
        assert_eq!(regions.notes, "notes of Regions");
        assert!(xml.contains("NegativeROA=\"1\""));
    }
}
