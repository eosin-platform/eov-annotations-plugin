//! Hamamatsu NDP.view annotations (`.ndpa`, import).
//!
//! ```xml
//! <annotations>
//!   <ndpviewstate id="1">
//!     <title>Tumour</title><details>…</details>
//!     <coordformat>nanometers</coordformat>
//!     <annotation type="freehand" color="#ff0000"><closed>1</closed>
//!       <pointlist><point><x>…</x><y>…</y></point>…</pointlist></annotation>
//!   </ndpviewstate>
//! </annotations>
//! ```
//!
//! Coordinates are nanometres on the glass, measured from the centre of the
//! slide rather than from a corner of the image. They are placed on a slide
//! with its NDPI properties: `hamamatsu.XOffsetFromSlideCentre` and
//! `hamamatsu.YOffsetFromSlideCentre` (where the image centre lies, in nm)
//! and the pixel size:
//!
//! `x_px = (x_nm − XOffset) / (mpp_x · 1000) + width / 2` (and likewise y).
//!
//! Supported annotation types: `freehand` (closed: polygon, rectangle when
//! its `specialtype` says so; open: line), `circle`, `pin`, `linearrow`
//! (an arrow from its first point to its second) and `line`/`ruler`.

use super::{ImportAnnotation, ImportDoc, ImportLayer, Units};
use crate::model::{Bounds, Color, Geometry};
use eov_plugin_api::protocol::{FileInfo, PropertyMap};
use roxmltree::Node;

/// Where a slide's NDPA coordinates go: its centre offset (nm), pixel size
/// (µm per level-0 pixel) and size (level-0 pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub offset_x_nm: f64,
    pub offset_y_nm: f64,
    pub mpp_x: f64,
    pub mpp_y: f64,
    pub width: f64,
    pub height: f64,
}

fn property(props: &PropertyMap, key: &str) -> Option<f64> {
    props
        .get(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

impl Frame {
    /// The frame of an open slide, or why NDPA coordinates cannot be placed
    /// on it.
    pub fn of_slide(props: &PropertyMap, info: &FileInfo) -> Result<Frame, String> {
        let vendor = props
            .get("openslide.vendor")
            .map(String::as_str)
            .unwrap_or("");
        if vendor != "hamamatsu" {
            return Err(format!(
                "NDPA coordinates are measured from the centre of a Hamamatsu slide (NDPI), but this slide{} is \
                 not one, so they cannot be placed on it.",
                if vendor.is_empty() {
                    String::new()
                } else {
                    format!(" ({vendor})")
                }
            ));
        }
        let (Some(offset_x_nm), Some(offset_y_nm)) = (
            property(props, "hamamatsu.XOffsetFromSlideCentre"),
            property(props, "hamamatsu.YOffsetFromSlideCentre"),
        ) else {
            return Err(
                "This NDPI does not give its offsets from the slide centre (hamamatsu.XOffsetFromSlideCentre \
                 and YOffsetFromSlideCentre), so the NDPA coordinates cannot be placed on it."
                    .to_string(),
            );
        };
        let mpp_x = property(props, "openslide.mpp-x").or(info.mpp_x);
        let mpp_y = property(props, "openslide.mpp-y").or(info.mpp_y);
        let (Some(mpp_x), Some(mpp_y)) = (mpp_x.filter(|v| *v > 0.0), mpp_y.filter(|v| *v > 0.0))
        else {
            return Err(
                "This slide has no pixel size, so the NDPA's nanometre coordinates cannot be converted to pixels."
                    .to_string(),
            );
        };
        if info.width == 0 || info.height == 0 {
            return Err("The slide's size is unknown.".to_string());
        }
        Ok(Frame {
            offset_x_nm,
            offset_y_nm,
            mpp_x,
            mpp_y,
            width: info.width as f64,
            height: info.height as f64,
        })
    }

    pub fn pixel(&self, p: [f64; 2]) -> [f64; 2] {
        [
            (p[0] - self.offset_x_nm) / (self.mpp_x * 1000.0) + self.width / 2.0,
            (p[1] - self.offset_y_nm) / (self.mpp_y * 1000.0) + self.height / 2.0,
        ]
    }

    fn geometry(&self, g: &Geometry) -> Geometry {
        let points = |list: &[[f64; 2]]| list.iter().map(|p| self.pixel(*p)).collect::<Vec<_>>();
        match g {
            Geometry::Point { x, y } => {
                let [x, y] = self.pixel([*x, *y]);
                Geometry::Point { x, y }
            }
            Geometry::Text { x, y } => {
                let [x, y] = self.pixel([*x, *y]);
                Geometry::Text { x, y }
            }
            Geometry::Polyline { points: p } => Geometry::Polyline { points: points(p) },
            Geometry::Arrow { points: p } => Geometry::Arrow { points: points(p) },
            Geometry::Polygon { exterior, holes } => Geometry::Polygon {
                exterior: points(exterior),
                holes: holes.iter().map(|h| points(h)).collect(),
            },
            Geometry::Rectangle {
                x,
                y,
                width,
                height,
            } => {
                let a = self.pixel([*x, *y]);
                let b = self.pixel([x + width, y + height]);
                let bounds = Bounds::of_points(&[a, b]);
                Geometry::Rectangle {
                    x: bounds.min_x,
                    y: bounds.min_y,
                    width: bounds.width(),
                    height: bounds.height(),
                }
            }
            Geometry::Ellipse { cx, cy, rx, ry } => {
                let [cx, cy] = self.pixel([*cx, *cy]);
                Geometry::Ellipse {
                    cx,
                    cy,
                    rx: rx / (self.mpp_x * 1000.0),
                    ry: ry / (self.mpp_y * 1000.0),
                }
            }
        }
    }
}

/// Converts a parsed NDPA (nanometres from the slide centre) to level-0
/// pixels of a slide.
pub fn place(doc: &mut ImportDoc, frame: &Frame) {
    for layer in &mut doc.layers {
        for a in &mut layer.annotations {
            a.geometry = frame.geometry(&a.geometry);
        }
    }
    doc.units = Units::Pixels;
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name))
}

fn text(node: Node<'_, '_>, name: &str) -> Option<String> {
    child(node, name).map(|n| n.text().unwrap_or_default().trim().to_string())
}

fn number(node: Node<'_, '_>, name: &str) -> Option<f64> {
    text(node, name)
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn point_list(annotation: Node<'_, '_>) -> Option<Vec<[f64; 2]>> {
    let list = child(annotation, "pointlist")?;
    list.children()
        .filter(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case("point"))
        .map(|p| Some([number(p, "x")?, number(p, "y")?]))
        .collect()
}

/// Two points given as `x1 y1 x2 y2` (lines and arrows), or a point list.
fn segment(annotation: Node<'_, '_>) -> Option<Vec<[f64; 2]>> {
    match (
        number(annotation, "x1"),
        number(annotation, "y1"),
        number(annotation, "x2"),
        number(annotation, "y2"),
    ) {
        (Some(x1), Some(y1), Some(x2), Some(y2)) => Some(vec![[x1, y1], [x2, y2]]),
        _ => point_list(annotation),
    }
}

fn shape(annotation: Node<'_, '_>) -> Result<Geometry, String> {
    let kind = annotation
        .attribute("type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let unreadable = || format!("{kind} annotation with unreadable coordinates");
    match kind.as_str() {
        "freehand" => {
            let points = point_list(annotation).ok_or_else(unreadable)?;
            let closed = text(annotation, "closed").is_none_or(|c| c == "1");
            let rectangle = text(annotation, "specialtype")
                .is_some_and(|t| t.eq_ignore_ascii_case("rectangle"));
            if !closed {
                return Ok(Geometry::Polyline { points });
            }
            let b = Bounds::of_points(&points);
            let boxy = points.len() == 4
                && points.iter().all(|p| {
                    (p[0] == b.min_x || p[0] == b.max_x) && (p[1] == b.min_y || p[1] == b.max_y)
                });
            if rectangle && boxy {
                Ok(Geometry::Rectangle {
                    x: b.min_x,
                    y: b.min_y,
                    width: b.width(),
                    height: b.height(),
                })
            } else {
                Ok(Geometry::Polygon {
                    exterior: points,
                    holes: vec![],
                })
            }
        }
        "circle" => {
            let (Some(x), Some(y), Some(r)) = (
                number(annotation, "x"),
                number(annotation, "y"),
                number(annotation, "radius"),
            ) else {
                return Err(unreadable());
            };
            Ok(Geometry::Ellipse {
                cx: x,
                cy: y,
                rx: r,
                ry: r,
            })
        }
        "pin" | "pointer" => {
            let (Some(x), Some(y)) = (number(annotation, "x"), number(annotation, "y")) else {
                return Err(unreadable());
            };
            Ok(Geometry::Point { x, y })
        }
        "linearrow" | "arrow" => Ok(Geometry::Arrow {
            points: segment(annotation).ok_or_else(unreadable)?,
        }),
        "line" | "ruler" => Ok(Geometry::Polyline {
            points: segment(annotation).ok_or_else(unreadable)?,
        }),
        other => Err(format!("unsupported NDPA annotation type '{other}'")),
    }
}

/// Whether an XML root element is an NDPA document.
pub fn is_ndpa(root: Node<'_, '_>) -> bool {
    root.tag_name().name() == "annotations"
        && root
            .children()
            .any(|n| n.is_element() && n.tag_name().name() == "ndpviewstate")
}

/// Reads an NDPA document; coordinates stay in nanometres from the slide
/// centre ([`Units::NdpaNanometres`]) until [`place`]d on a slide.
pub fn parse(root: Node<'_, '_>, stem: &str) -> ImportDoc {
    let mut doc = ImportDoc {
        format: "Hamamatsu NDPA",
        units: Units::NdpaNanometres,
        ..Default::default()
    };
    // One layer per colour, in the order the colours appear.
    let mut layers: Vec<ImportLayer> = Vec::new();
    for state in root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "ndpviewstate")
    {
        if let Some(format) = text(state, "coordformat")
            && !format.eq_ignore_ascii_case("nanometers")
        {
            doc.skip(format!("annotation in unsupported coordinates ({format})"));
            continue;
        }
        let Some(annotation) = child(state, "annotation") else {
            doc.skip("view state without an annotation");
            continue;
        };
        let geometry = match shape(annotation) {
            Ok(g) => g,
            Err(reason) => {
                doc.skip(reason);
                continue;
            }
        };
        let color = annotation.attribute("color").and_then(Color::parse);
        let index = match layers.iter().position(|l| l.color == color) {
            Some(index) => index,
            None => {
                layers.push(ImportLayer {
                    name: stem.to_string(),
                    color,
                    ..Default::default()
                });
                layers.len() - 1
            }
        };
        let mut a = ImportAnnotation::new(geometry);
        a.name = text(state, "title").unwrap_or_default();
        a.description = text(state, "details").unwrap_or_default();
        layers[index].annotations.push(a);
    }
    if layers.len() > 1 {
        for layer in &mut layers {
            if let Some(color) = layer.color {
                layer.name = format!("{stem} {}", color.to_hex());
            }
        }
    }
    if layers.is_empty() {
        layers.push(ImportLayer {
            name: stem.to_string(),
            ..Default::default()
        });
    }
    doc.layers = layers;
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    const NDPA: &str = r##"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<annotations>
  <ndpviewstate id="1">
    <title>Tumour</title>
    <details>main mass</details>
    <coordformat>nanometers</coordformat>
    <annotation type="freehand" displayname="AnnotateFreehand" color="#ff0000">
      <closed>1</closed>
      <pointlist>
        <point><x>1000000</x><y>-2000000</y></point>
        <point><x>1500000</x><y>-2000000</y></point>
        <point><x>1500000</x><y>-1500000</y></point>
      </pointlist>
    </annotation>
  </ndpviewstate>
  <ndpviewstate id="2">
    <title>Edge</title>
    <coordformat>nanometers</coordformat>
    <annotation type="freehand" displayname="AnnotateRectangle" color="#ff0000">
      <closed>1</closed><specialtype>rectangle</specialtype>
      <pointlist>
        <point><x>0</x><y>0</y></point>
        <point><x>500000</x><y>0</y></point>
        <point><x>500000</x><y>250000</y></point>
        <point><x>0</x><y>250000</y></point>
      </pointlist>
    </annotation>
  </ndpviewstate>
  <ndpviewstate id="3">
    <title>Field</title>
    <annotation type="circle" displayname="AnnotateCircle" color="#00ff00">
      <x>1000000</x><y>-2000000</y><radius>250000</radius>
    </annotation>
  </ndpviewstate>
  <ndpviewstate id="4">
    <title>Look here</title>
    <annotation type="linearrow" displayname="AnnotateLineArrow" color="#00ff00">
      <x1>1100000</x1><y1>-2000000</y1><x2>1000000</x2><y2>-2000000</y2>
    </annotation>
  </ndpviewstate>
  <ndpviewstate id="5">
    <title>Mitosis</title>
    <annotation type="pin" color="#00ff00"><x>1000000</x><y>-1000000</y></annotation>
  </ndpviewstate>
  <ndpviewstate id="6">
    <annotation type="freehand"><closed>0</closed><pointlist><point><x>a</x><y>1</y></point></pointlist></annotation>
  </ndpviewstate>
  <ndpviewstate id="7"><annotation type="hologram"/></ndpviewstate>
</annotations>"##;

    fn frame() -> Frame {
        // 10 000 × 8 000 px at 0.5 µm/px; the image centre is 1 mm right of
        // and 2 mm above the slide centre.
        Frame {
            offset_x_nm: 1_000_000.0,
            offset_y_nm: -2_000_000.0,
            mpp_x: 0.5,
            mpp_y: 0.5,
            width: 10_000.0,
            height: 8_000.0,
        }
    }

    fn parsed() -> ImportDoc {
        let document = roxmltree::Document::parse(NDPA).unwrap();
        assert!(is_ndpa(document.root_element()));
        parse(document.root_element(), "case")
    }

    #[test]
    fn ndpa_files_are_read_in_nanometres_and_placed_with_the_slide_offsets() {
        let mut doc = parsed();
        assert_eq!(doc.units, Units::NdpaNanometres);
        assert_eq!(doc.skipped.len(), 2, "{:?}", doc.skipped);
        // Two colours, two layers.
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.layers[0].name, "case #FF0000");
        place(&mut doc, &frame());
        assert_eq!(doc.units, Units::Pixels);
        let red = &doc.layers[0].annotations;
        assert_eq!(red[0].name, "Tumour");
        assert_eq!(red[0].description, "main mass");
        // The image centre is at the offset; 1 px is 500 nm.
        let Geometry::Polygon { exterior, .. } = &red[0].geometry else {
            panic!()
        };
        assert_eq!(exterior[0], [5000.0, 4000.0]);
        assert_eq!(exterior[1], [6000.0, 4000.0]);
        assert_eq!(exterior[2], [6000.0, 5000.0]);
        assert_eq!(
            red[1].geometry,
            Geometry::Rectangle {
                x: 3000.0,
                y: 8000.0,
                width: 1000.0,
                height: 500.0
            }
        );
        let green = &doc.layers[1].annotations;
        assert_eq!(
            green[0].geometry,
            Geometry::Ellipse {
                cx: 5000.0,
                cy: 4000.0,
                rx: 500.0,
                ry: 500.0
            }
        );
        assert_eq!(
            green[1].geometry,
            Geometry::Arrow {
                points: vec![[5200.0, 4000.0], [5000.0, 4000.0]]
            }
        );
        assert_eq!(
            green[2].geometry,
            Geometry::Point {
                x: 5000.0,
                y: 6000.0
            }
        );
    }

    #[test]
    fn slides_that_cannot_place_ndpa_coordinates_say_why() {
        let info = FileInfo {
            width: 10_000,
            height: 8_000,
            mpp_x: Some(0.5),
            mpp_y: Some(0.5),
            ..Default::default()
        };
        let mut props = PropertyMap::new();
        props.insert("openslide.vendor".into(), "aperio".into());
        let err = Frame::of_slide(&props, &info).unwrap_err();
        assert!(err.contains("not one") && err.contains("aperio"), "{err}");
        props.insert("openslide.vendor".into(), "hamamatsu".into());
        let err = Frame::of_slide(&props, &info).unwrap_err();
        assert!(err.contains("XOffsetFromSlideCentre"), "{err}");
        props.insert("hamamatsu.XOffsetFromSlideCentre".into(), "1000000".into());
        props.insert("hamamatsu.YOffsetFromSlideCentre".into(), "-2000000".into());
        assert_eq!(Frame::of_slide(&props, &info).unwrap(), frame());
        let uncalibrated = FileInfo {
            mpp_x: None,
            mpp_y: None,
            ..info
        };
        assert!(
            Frame::of_slide(&props, &uncalibrated)
                .unwrap_err()
                .contains("pixel size")
        );
    }
}
