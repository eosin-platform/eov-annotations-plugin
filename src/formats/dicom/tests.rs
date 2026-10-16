use super::part10::{FileObject, Value as Raw};
use super::*;
use crate::model::{LayerRole, SlideRecord};
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

fn layer(id: &str, name: &str, color: Color) -> Layer {
    Layer {
        id: id.into(),
        slide_id: 1,
        name: name.into(),
        notes: format!("notes of {name}"),
        color,
        visible: true,
        opacity: 1.0,
        locked: false,
        role: LayerRole::Normal,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    }
}

fn annotation(layer: &str, class: Option<&str>, name: &str, geometry: Geometry) -> Arc<Annotation> {
    Arc::new(Annotation {
        id: format!("{layer}-{name}"),
        layer_id: layer.into(),
        class_id: class.map(str::to_string),
        geometry,
        name: name.into(),
        description: if class.is_some() {
            format!("about {name}")
        } else {
            String::new()
        },
        status: if name.ends_with('!') {
            Status::Verified
        } else {
            Status::Draft
        },
        author: "Dr. A".into(),
        locked: false,
        properties: vec![("grade".into(), "3".into())],
        comments: vec![],
        created_at: 0,
        updated_at: 0,
    })
}

const LONG_CLASS: &str =
    "A class with a name much longer than the sixty-four characters of LO \\ and a backslash";

fn sample() -> (ExportSlide, Vec<Class>) {
    let classes = vec![
        class("c1", "Tumour", Color(0xD5, 0x5E, 0x00)),
        class("c2", "Stroma", Color(0x56, 0xB4, 0xE9)),
        class("c3", LONG_CLASS, Color(1, 2, 3)),
    ];
    let regions = vec![
        annotation(
            "A",
            Some("c1"),
            "Region one!",
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
            "A",
            None,
            "Wide",
            Geometry::Ellipse {
                cx: 500.0,
                cy: 400.0,
                rx: 120.5,
                ry: 60.25,
            },
        ),
        annotation(
            "A",
            None,
            "Tall",
            Geometry::Ellipse {
                cx: 300.0,
                cy: 300.0,
                rx: 10.0,
                ry: 25.5,
            },
        ),
        annotation(
            "A",
            Some("c1"),
            "Region two",
            Geometry::Polygon {
                exterior: vec![[1200.0, 100.0], [1300.0, 100.0], [1250.0, 180.0]],
                holes: vec![],
            },
        ),
    ];
    let marks = vec![
        annotation(
            "B",
            Some("c1"),
            "Mitosis 1",
            Geometry::Point { x: 12.5, y: 13.75 },
        ),
        annotation("B", None, "Margin", Geometry::Text { x: 50.0, y: 60.0 }),
        annotation(
            "B",
            None,
            "Path",
            Geometry::Polyline {
                points: vec![[0.0, 0.0], [10.0, 5.0], [20.0, 0.0]],
            },
        ),
        annotation(
            "B",
            Some("c2"),
            "Here",
            Geometry::Arrow {
                points: vec![[300.0, 300.0], [350.0, 280.0]],
            },
        ),
        annotation("B", Some("c3"), "Long", Geometry::Point { x: 7.0, y: 8.0 }),
    ];
    let slide = ExportSlide {
        record: SlideRecord {
            id: 1,
            identity: Some("eov-slide-1:abc".into()),
            filename: "/secret/MRN-12345/case.svs".into(),
            width: 2000,
            height: 1000,
            mpp_x: Some(0.25),
            mpp_y: Some(0.25),
            ..Default::default()
        },
        quickhash1: Some("qh".into()),
        bounds: None,
        layers: vec![
            (layer("A", "Regions", Color(0x12, 0x34, 0x56)), regions),
            (layer("B", "Marks", Color(0xAB, 0xCD, 0xEF)), marks),
        ],
    };
    (slide, classes)
}

fn options() -> ExportOptions {
    ExportOptions {
        author: "Reader^Ann".into(),
        generator: "eov annotations plugin (test)".into(),
        // 2026-10-15T09:30:00Z
        exported_at: 1_792_056_600_000,
        ..Default::default()
    }
}

fn exported() -> Export {
    let (slide, classes) = sample();
    export(&slide, &classes, &options(), None).unwrap()
}

fn read(bytes: &[u8]) -> FileObject {
    part10::read(bytes).unwrap()
}

/// Equal within the rounding of 32-bit floats.
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

fn near_points(p: &[[f64; 2]], q: &[[f64; 2]]) -> bool {
    p.len() == q.len()
        && p.iter()
            .zip(q)
            .all(|(a, b)| near(a[0], b[0]) && near(a[1], b[1]))
}

fn same_shape(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::Point { x, y }, Geometry::Point { x: u, y: v })
        | (Geometry::Text { x, y }, Geometry::Text { x: u, y: v }) => near(*x, *u) && near(*y, *v),
        (Geometry::Polyline { points: p }, Geometry::Polyline { points: q })
        | (Geometry::Arrow { points: p }, Geometry::Arrow { points: q }) => near_points(p, q),
        (
            Geometry::Polygon {
                exterior: e,
                holes: h,
            },
            Geometry::Polygon {
                exterior: f,
                holes: k,
            },
        ) => {
            near_points(e, f)
                && h.len() == k.len()
                && h.iter().zip(k).all(|(h, k)| near_points(h, k))
        }
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

fn groups_of(file: &FileObject) -> &[DataSet] {
    file.dataset.items(tag::ANNOTATION_GROUP_SEQUENCE)
}

fn group<'a>(file: &'a FileObject, label: &str, graphic: &str) -> &'a DataSet {
    groups_of(file)
        .iter()
        .find(|g| {
            g.text(tag::ANNOTATION_GROUP_LABEL).as_deref() == Some(label)
                && g.text(tag::GRAPHIC_TYPE).as_deref() == Some(graphic)
        })
        .unwrap_or_else(|| panic!("no {graphic} group labelled {label}"))
}

fn floats(item: &DataSet) -> Vec<f32> {
    item.f32s(tag::POINT_COORDINATES_DATA).unwrap().unwrap()
}

/// The file without eov's private attributes: what other programs read.
fn without_private(bytes: &[u8]) -> Vec<u8> {
    fn strip(ds: &DataSet) -> DataSet {
        let mut out = DataSet::new();
        for (tag, element) in ds.iter() {
            if tag.0 == PRIVATE_TAG_GROUP {
                continue;
            }
            let value = match &element.value {
                Raw::Items(items) => Raw::Items(items.iter().map(strip).collect()),
                other => other.clone(),
            };
            out.insert(*tag, element.vr, value);
        }
        out
    }
    let file = read(bytes);
    let sop_instance = file.dataset.text(tag::SOP_INSTANCE_UID).unwrap();
    part10::write(
        &part10::FileMeta {
            sop_class_uid: SOP_CLASS_UID,
            sop_instance_uid: &sop_instance,
            implementation_class_uid: IMPLEMENTATION_CLASS_UID,
            implementation_version_name: "TEST",
        },
        &strip(&file.dataset),
    )
    .unwrap()
}

#[test]
fn exports_read_back_with_shapes_classes_names_and_layers() {
    let (slide, classes) = sample();
    let out = export(&slide, &classes, &options(), None).unwrap();
    assert_eq!(out.count, 10);
    let doc = parse(&out.bytes, "case").unwrap();
    assert_eq!(doc.format, "DICOM bulk annotations");
    assert!(doc.skipped.is_empty(), "{:?}", doc.skipped);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    assert_eq!(
        doc.layers
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>(),
        ["Regions", "Marks"]
    );
    for (layer, annotations) in &slide.layers {
        let imported = doc.layers.iter().find(|l| l.name == layer.name).unwrap();
        assert_eq!(imported.color, Some(layer.color), "{}", layer.name);
        assert_eq!(imported.notes, layer.notes);
        assert_eq!(imported.annotations.len(), annotations.len());
        for a in annotations {
            let b = imported
                .annotations
                .iter()
                .find(|b| b.name == a.name)
                .unwrap_or_else(|| panic!("{} is missing", a.name));
            assert!(
                same_shape(&a.geometry, &b.geometry),
                "{}: {:?} != {:?}",
                a.name,
                a.geometry,
                b.geometry
            );
            let class = a
                .class_id
                .as_deref()
                .and_then(|id| classes.iter().find(|c| c.id == id))
                .map(|c| c.name.clone());
            assert_eq!(b.class, class, "{}", a.name);
            assert_eq!(b.description, a.description);
            assert_eq!(b.status, a.status);
            // Not exported.
            assert!(b.author.is_empty() && b.properties.is_empty());
        }
    }
    // Count and HPF field layers keep their role; hidden layers stay hidden.
    let (mut slide, classes) = sample();
    slide.layers[0].0.role = LayerRole::Fields;
    slide.layers[1].0.visible = false;
    let again = parse(
        &export(&slide, &classes, &options(), None).unwrap().bytes,
        "x",
    )
    .unwrap();
    assert_eq!(again.layers[0].role, LayerRole::Fields);
    assert_eq!(again.layers[0].visible, None);
    assert_eq!(again.layers[1].role, LayerRole::Normal);
    assert_eq!(again.layers[1].visible, Some(false));
    // Class colours are exact; the long class name survives its label.
    for c in &classes {
        let imported = doc.classes.iter().find(|i| i.name == c.name).unwrap();
        assert_eq!(imported.color, Some(c.color), "{}", c.name);
    }
    // The slide is recognised by its eov identity and size.
    assert_eq!(doc.slide.identity.as_deref(), Some("eov-slide-1:abc"));
    assert_eq!(
        (doc.slide.width, doc.slide.height),
        (Some(2000), Some(1000))
    );
    assert_eq!(doc.slide.mpp_x, Some(0.25));
    assert_eq!(doc.slide.dicom, None);
    // The export note says what other viewers do not see.
    let notes = out.notes.join(" ");
    for expected in [
        "refers to no image",
        "1 hole in 1 region",
        "the text of 1 text label",
        "the heads of 1 arrow",
        "the names, descriptions and statuses of",
        "labelled with its class",
        "eov reads them back",
        "Authors, comments, properties and times are not exported",
    ] {
        assert!(notes.contains(expected), "{expected}: {notes}");
    }
}

#[test]
fn other_programs_see_the_shapes_with_labels_as_classes() {
    let doc = parse(&without_private(&exported().bytes), "case").unwrap();
    assert!(doc.skipped.is_empty(), "{:?}", doc.skipped);
    // A layer per label: classes, or the layer for unclassified shapes.
    let mut names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
    names.sort_unstable();
    let long_label: String = LONG_CLASS.replace('\\', "/").chars().take(64).collect();
    let long_label = long_label.trim();
    let mut expected = vec!["Marks", "Regions", "Stroma", "Tumour", long_label];
    expected.sort_unstable();
    assert_eq!(names, expected);
    let tumour = doc.layers.iter().find(|l| l.name == "Tumour").unwrap();
    assert!(
        tumour
            .annotations
            .iter()
            .all(|a| a.class.as_deref() == Some("Tumour"))
    );
    // The hole, the text, the arrowhead and the names are gone.
    let region = tumour
        .annotations
        .iter()
        .find_map(|a| match &a.geometry {
            Geometry::Polygon { exterior, holes } if exterior.len() == 4 => Some(holes),
            _ => None,
        })
        .unwrap();
    assert!(region.is_empty());
    let marks = doc.layers.iter().find(|l| l.name == "Marks").unwrap();
    assert!(
        marks
            .annotations
            .iter()
            .any(|a| matches!(a.geometry, Geometry::Point { x, .. } if x == 50.0))
    );
    let stroma = doc.layers.iter().find(|l| l.name == "Stroma").unwrap();
    assert!(
        stroma
            .annotations
            .iter()
            .any(|a| matches!(a.geometry, Geometry::Polyline { .. }))
    );
    assert!(
        doc.layers
            .iter()
            .flat_map(|l| &l.annotations)
            .all(|a| a.name.is_empty())
    );
    // Colours come from the Recommended Display CIELab Values.
    let class = doc.classes.iter().find(|c| c.name == "Stroma").unwrap();
    assert_eq!(class.color, Some(Color(0x56, 0xB4, 0xE9)));
}

#[test]
fn exports_follow_the_microscopy_bulk_simple_annotations_iod() {
    let out = exported();
    let file = read(&out.bytes);
    let (meta, ds) = (&file.meta, &file.dataset);
    assert_eq!(&out.bytes[..128], &[0u8; 128][..]);
    assert_eq!(&out.bytes[128..132], b"DICM");
    assert_eq!(
        meta.text(part10::TRANSFER_SYNTAX_UID).as_deref(),
        Some(part10::EXPLICIT_VR_LITTLE_ENDIAN)
    );
    assert_eq!(
        meta.text(part10::MEDIA_STORAGE_SOP_CLASS_UID).as_deref(),
        Some(SOP_CLASS_UID)
    );
    assert_eq!(
        meta.text(part10::MEDIA_STORAGE_SOP_INSTANCE_UID),
        ds.text(tag::SOP_INSTANCE_UID)
    );
    assert_eq!(
        meta.text(part10::IMPLEMENTATION_CLASS_UID).as_deref(),
        Some(IMPLEMENTATION_CLASS_UID)
    );
    assert_eq!(
        meta.bytes(part10::FILE_META_INFORMATION_VERSION),
        Some(&[0u8, 1][..])
    );
    let text = |tag: Tag| ds.text(tag).unwrap_or_else(|| panic!("{tag} is missing"));
    assert_eq!(text(tag::SOP_CLASS_UID), SOP_CLASS_UID);
    assert_eq!(text(tag::SPECIFIC_CHARACTER_SET), "ISO_IR 192");
    assert_eq!(text(tag::MODALITY), "ANN");
    assert_eq!(text(tag::ANNOTATION_COORDINATE_TYPE), "2D");
    assert_eq!(text(tag::PIXEL_ORIGIN_INTERPRETATION), "VOLUME");
    assert_eq!(text(tag::CONTENT_LABEL), "EOV_ANNOTATIONS");
    assert_eq!(text(tag::CONTENT_CREATOR_NAME), "Reader^Ann");
    assert_eq!(text(tag::CONTENT_DATE), "20261015");
    assert_eq!(text(tag::CONTENT_TIME), "093000");
    assert_eq!(text(tag::INSTANCE_NUMBER), "1");
    assert_eq!(text(tag::MANUFACTURER), "eov");
    for tag in [
        tag::MANUFACTURER_MODEL_NAME,
        tag::DEVICE_SERIAL_NUMBER,
        tag::SOFTWARE_VERSIONS,
        tag::CONTENT_DESCRIPTION,
    ] {
        assert!(!text(tag).is_empty(), "{tag}");
    }
    // A study of its own: new UIDs, the export's date, no patient data.
    for tag in [
        tag::SOP_INSTANCE_UID,
        tag::STUDY_INSTANCE_UID,
        tag::SERIES_INSTANCE_UID,
    ] {
        let uid = text(tag);
        assert!(uid.starts_with("2.25.") && is_uid(&uid), "{uid}");
    }
    assert_eq!(text(tag::STUDY_DATE), "20261015");
    for tag in [
        tag::PATIENT_NAME,
        tag::PATIENT_ID,
        tag::PATIENT_BIRTH_DATE,
        tag::PATIENT_SEX,
        tag::REFERRING_PHYSICIAN_NAME,
        tag::STUDY_ID,
        tag::ACCESSION_NUMBER,
    ] {
        assert_eq!(text(tag), "", "{tag} is type 2: present and empty");
    }
    assert!(!ds.contains(tag::REFERENCED_IMAGE_SEQUENCE));
    assert!(!ds.contains(tag::REFERENCED_SERIES_SEQUENCE));
    assert!(!ds.contains(tag::FRAME_OF_REFERENCE_UID));
    // Nothing of the slide's path.
    assert!(!String::from_utf8_lossy(&out.bytes).contains("MRN-12345"));
    // Groups: one per layer, class and kind of shape.
    let groups = groups_of(&file);
    // Regions: Tumour polygons, a Stroma rectangle, unclassified ellipses;
    // Marks: a Tumour point, text, a line, a Stroma arrow, a long class.
    assert_eq!(groups.len(), 8);
    let mut uids = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        assert_eq!(g.uint(tag::ANNOTATION_GROUP_NUMBER), Some(i as u64 + 1));
        let uid = g.text(tag::ANNOTATION_GROUP_UID).unwrap();
        assert!(is_uid(&uid) && !uids.contains(&uid));
        uids.push(uid);
        assert!(!g.text(tag::ANNOTATION_GROUP_LABEL).unwrap().is_empty());
        assert!(g.text(tag::ANNOTATION_GROUP_LABEL).unwrap().chars().count() <= 64);
        assert_eq!(
            g.text(tag::ANNOTATION_GROUP_GENERATION_TYPE).as_deref(),
            Some("MANUAL")
        );
        assert_eq!(
            g.text(tag::ANNOTATION_APPLIES_TO_ALL_OPTICAL_PATHS)
                .as_deref(),
            Some("YES")
        );
        for sequence in [
            tag::ANNOTATION_PROPERTY_CATEGORY_CODE_SEQUENCE,
            tag::ANNOTATION_PROPERTY_TYPE_CODE_SEQUENCE,
        ] {
            let code = &g.items(sequence)[0];
            assert_eq!(code.text(tag::CODE_VALUE).as_deref(), Some("91723000"));
            assert_eq!(
                code.text(tag::CODING_SCHEME_DESIGNATOR).as_deref(),
                Some("SCT")
            );
            assert_eq!(
                code.text(tag::CODE_MEANING).as_deref(),
                Some("Anatomical Structure")
            );
        }
        assert_eq!(g.get(tag::NUMBER_OF_ANNOTATIONS).unwrap().vr, Vr::UL);
        assert_eq!(g.get(tag::POINT_COORDINATES_DATA).unwrap().vr, Vr::OF);
        assert_eq!(
            g.u16s(tag::RECOMMENDED_DISPLAY_CIELAB_VALUE).unwrap().len(),
            3
        );
        let graphic = g.text(tag::GRAPHIC_TYPE).unwrap();
        let lines = matches!(graphic.as_str(), "POLYLINE" | "POLYGON");
        assert_eq!(
            g.contains(tag::LONG_PRIMITIVE_POINT_INDEX_LIST),
            lines,
            "{graphic}"
        );
        if lines {
            assert_eq!(
                g.get(tag::LONG_PRIMITIVE_POINT_INDEX_LIST).unwrap().vr,
                Vr::OL
            );
        }
    }
    // Unclassified shapes are labelled with their layer; the description
    // names the layer and what DICOM cannot show.
    let text_group = groups
        .iter()
        .find(|g| {
            g.text(tag::ANNOTATION_GROUP_DESCRIPTION)
                .is_some_and(|d| d.contains("text labels"))
        })
        .unwrap();
    assert_eq!(
        text_group.text(tag::ANNOTATION_GROUP_LABEL).as_deref(),
        Some("Marks")
    );
    assert_eq!(text_group.text(tag::GRAPHIC_TYPE).as_deref(), Some("POINT"));
    let regions = group(&file, "Tumour", "POLYGON");
    let description = regions.text(tag::ANNOTATION_GROUP_DESCRIPTION).unwrap();
    assert!(
        description.contains("\"Regions\"") && description.contains("holes"),
        "{description}"
    );
    assert_eq!(regions.uint(tag::NUMBER_OF_ANNOTATIONS), Some(2));
}

#[test]
fn coordinates_are_image_relative_with_pixel_corners_at_integers() {
    // The centre of the top-left pixel, the slide's corners, an ellipse of
    // each orientation, and a polygon whose last vertex repeats its first.
    let classes = [];
    let l = layer("L", "Grid", Color(0, 0, 0));
    let slide = ExportSlide {
        record: SlideRecord {
            width: 1000,
            height: 800,
            ..Default::default()
        },
        quickhash1: None,
        bounds: None,
        layers: vec![(
            l,
            vec![
                annotation("L", None, "centre", Geometry::Point { x: 0.5, y: 0.5 }),
                annotation(
                    "L",
                    None,
                    "slide",
                    Geometry::Rectangle {
                        x: 0.0,
                        y: 0.0,
                        width: 1000.0,
                        height: 800.0,
                    },
                ),
                annotation(
                    "L",
                    None,
                    "wide",
                    Geometry::Ellipse {
                        cx: 500.0,
                        cy: 400.0,
                        rx: 100.0,
                        ry: 50.0,
                    },
                ),
                annotation(
                    "L",
                    None,
                    "tall",
                    Geometry::Ellipse {
                        cx: 500.0,
                        cy: 400.0,
                        rx: 50.0,
                        ry: 100.0,
                    },
                ),
                annotation(
                    "L",
                    None,
                    "closed",
                    Geometry::Polygon {
                        exterior: vec![[1.0, 1.0], [9.0, 1.0], [9.0, 9.0], [1.0, 1.0]],
                        holes: vec![],
                    },
                ),
                annotation(
                    "L",
                    None,
                    "triangle",
                    Geometry::Polygon {
                        exterior: vec![[20.0, 20.0], [30.0, 20.0], [25.0, 30.0]],
                        holes: vec![],
                    },
                ),
                annotation(
                    "L",
                    None,
                    "line",
                    Geometry::Polyline {
                        points: vec![[0.0, 0.0], [1.0, 1.0]],
                    },
                ),
            ],
        )],
    };
    let out = export(&slide, &classes, &options(), None).unwrap();
    let file = read(&out.bytes);
    // (column, row) = (x, y): no half-pixel shift either way.
    assert_eq!(floats(group(&file, "Grid", "POINT")), [0.5, 0.5]);
    assert_eq!(
        floats(group(&file, "Grid", "RECTANGLE")),
        [0.0, 0.0, 1000.0, 0.0, 1000.0, 800.0, 0.0, 800.0]
    );
    // Major axis first.
    assert_eq!(
        floats(group(&file, "Grid", "ELLIPSE")),
        [
            400.0, 400.0, 600.0, 400.0, 500.0, 350.0, 500.0, 450.0, // wide
            500.0, 300.0, 500.0, 500.0, 450.0, 400.0, 550.0, 400.0, // tall
        ]
    );
    // Polygons are implicitly closed; the index list counts values from 1.
    let polygons = group(&file, "Grid", "POLYGON");
    assert_eq!(
        floats(polygons),
        [
            1.0, 1.0, 9.0, 1.0, 9.0, 9.0, 20.0, 20.0, 30.0, 20.0, 25.0, 30.0
        ]
    );
    assert_eq!(
        polygons
            .u32s(tag::LONG_PRIMITIVE_POINT_INDEX_LIST)
            .unwrap()
            .unwrap(),
        [1, 7]
    );
    assert_eq!(
        group(&file, "Grid", "POLYLINE")
            .u32s(tag::LONG_PRIMITIVE_POINT_INDEX_LIST)
            .unwrap()
            .unwrap(),
        [1]
    );
    // And back.
    let doc = parse(&out.bytes, "x").unwrap();
    let shapes: Vec<&Geometry> = doc.layers[0]
        .annotations
        .iter()
        .map(|a| &a.geometry)
        .collect();
    assert!(shapes.contains(&&Geometry::Point { x: 0.5, y: 0.5 }));
    assert!(shapes.contains(&&Geometry::Rectangle {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0
    }));
    for (rx, ry) in [(100.0, 50.0), (50.0, 100.0)] {
        assert!(shapes.contains(&&Geometry::Ellipse {
            cx: 500.0,
            cy: 400.0,
            rx,
            ry
        }));
    }
    assert!(shapes.contains(&&Geometry::Polygon {
        exterior: vec![[1.0, 1.0], [9.0, 1.0], [9.0, 9.0]],
        holes: vec![]
    }));
}

fn source() -> Source {
    let props: PropertyMap = [
        ("openslide.vendor", "dicom"),
        ("dicom.StudyInstanceUID", "1.2.826.0.1.3680043.10.1"),
        ("dicom.SeriesInstanceUID", "1.2.826.0.1.3680043.10.2"),
        ("dicom.SOPInstanceUID", "1.2.826.0.1.3680043.10.3"),
        ("dicom.SOPClassUID", WSI_SOP_CLASS_UID),
        ("dicom.FrameOfReferenceUID", "1.2.826.0.1.3680043.10.4"),
        ("dicom.PositionReferenceIndicator", "SLIDE_CORNER"),
        ("dicom.PatientName", "Doe^Jane"),
        ("dicom.PatientID", "P-0001"),
        ("dicom.PatientBirthDate", "19700101"),
        ("dicom.PatientSex", "F"),
        ("dicom.StudyDate", "20260101"),
        ("dicom.StudyTime", "120000.5"),
        ("dicom.AccessionNumber", "A-7"),
        ("dicom.StudyID", "S-1"),
        ("dicom.ReferringPhysicianName", ""),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    Source::of_slide(&props).unwrap()
}

#[test]
fn dicom_slides_are_referenced_and_their_study_joined() {
    let source = source();
    let (slide, classes) = sample();
    let out = export(&slide, &classes, &options(), Some(&source)).unwrap();
    let file = read(&out.bytes);
    let ds = &file.dataset;
    let text = |tag: Tag| ds.text(tag).unwrap_or_default();
    assert_eq!(text(tag::STUDY_INSTANCE_UID), source.study_instance_uid);
    assert_ne!(text(tag::SERIES_INSTANCE_UID), source.series_instance_uid);
    assert_eq!(
        text(tag::FRAME_OF_REFERENCE_UID),
        "1.2.826.0.1.3680043.10.4"
    );
    assert_eq!(text(tag::POSITION_REFERENCE_INDICATOR), "SLIDE_CORNER");
    assert_eq!(text(tag::PATIENT_NAME), "Doe^Jane");
    assert_eq!(text(tag::PATIENT_ID), "P-0001");
    assert_eq!(text(tag::PATIENT_BIRTH_DATE), "19700101");
    assert_eq!(text(tag::PATIENT_SEX), "F");
    assert_eq!(text(tag::STUDY_DATE), "20260101");
    assert_eq!(text(tag::STUDY_TIME), "120000.5");
    assert_eq!(text(tag::ACCESSION_NUMBER), "A-7");
    let image = &ds.items(tag::REFERENCED_IMAGE_SEQUENCE)[0];
    assert_eq!(
        image.text(tag::REFERENCED_SOP_CLASS_UID).as_deref(),
        Some(WSI_SOP_CLASS_UID)
    );
    assert_eq!(
        image.text(tag::REFERENCED_SOP_INSTANCE_UID),
        Some(source.sop_instance_uid.clone())
    );
    let series = &ds.items(tag::REFERENCED_SERIES_SEQUENCE)[0];
    assert_eq!(
        series.text(tag::SERIES_INSTANCE_UID),
        Some(source.series_instance_uid.clone())
    );
    assert_eq!(series.items(tag::REFERENCED_INSTANCE_SEQUENCE).len(), 1);
    assert!(out.notes[0].contains("joins its study"), "{:?}", out.notes);
    // Read back, the file names what it was made for.
    let doc = parse(&out.bytes, "x").unwrap();
    let reference = doc.slide.dicom.unwrap();
    assert_eq!(reference.study, Some(source.study_instance_uid.clone()));
    assert_eq!(
        reference.series,
        std::slice::from_ref(&source.series_instance_uid)
    );
    assert!(reference.instances.contains(&source.sop_instance_uid));
    // Slides that are not DICOM images, or lack their UIDs, are not
    // referred to.
    let mut props = PropertyMap::new();
    assert_eq!(Source::of_slide(&props), None);
    props.insert("dicom.StudyInstanceUID".into(), "1.2.3".into());
    props.insert("dicom.SeriesInstanceUID".into(), "1.2.4".into());
    props.insert("dicom.SOPClassUID".into(), WSI_SOP_CLASS_UID.into());
    props.insert("dicom.SOPInstanceUID".into(), "not a uid".into());
    assert_eq!(Source::of_slide(&props), None);
    // Odd values copied from a slide are dropped rather than written.
    props.insert("dicom.SOPInstanceUID".into(), "1.2.5".into());
    props.insert("dicom.PatientSex".into(), "female".into());
    props.insert("dicom.StudyDate".into(), "2026-01-01".into());
    let odd = Source::of_slide(&props).unwrap();
    assert_eq!(
        (odd.patient_sex.as_str(), odd.study_date.as_str()),
        ("", "")
    );
}

#[test]
fn slides_are_matched_by_their_dicom_references() {
    let reference = Reference {
        study: Some("1.2".into()),
        series: vec!["1.2.3".into()],
        instances: vec!["1.2.3.4".into()],
    };
    let props = |series: &str, instance: &str| -> PropertyMap {
        [
            ("dicom.SeriesInstanceUID", series),
            ("dicom.SOPInstanceUID", instance),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    };
    let (text, mismatch) = match_slide(&reference, &props("1.2.3", "1.2.3.4"));
    assert!(!mismatch && text.contains("this slide"), "{text}");
    let (text, mismatch) = match_slide(&reference, &props("1.2.3", "1.2.3.9"));
    assert!(mismatch && text.contains("another image"), "{text}");
    let (text, mismatch) = match_slide(&reference, &props("9.9", "9.9.9"));
    assert!(mismatch && text.contains("different DICOM slide"), "{text}");
    let (text, mismatch) = match_slide(&reference, &props("", ""));
    assert!(mismatch && text.contains("not a DICOM image"), "{text}");
    let series_only = Reference {
        instances: vec![],
        ..reference
    };
    assert!(!match_slide(&series_only, &props("1.2.3", "1.2.3.9")).1);
}

// -- other producers ----------------------------------------------------------

/// An element in implicit VR little endian (`value` of even length).
fn implicit(tag: Tag, value: &[u8]) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "{tag}");
    let mut out = Vec::new();
    out.extend_from_slice(&tag.0.to_le_bytes());
    out.extend_from_slice(&tag.1.to_le_bytes());
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// A sequence of undefined length whose items have undefined lengths.
fn implicit_sequence(tag: Tag, items: &[Vec<u8>]) -> Vec<u8> {
    let marker = |out: &mut Vec<u8>, group: u16, element: u16, length: u32| {
        out.extend_from_slice(&group.to_le_bytes());
        out.extend_from_slice(&element.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
    };
    let mut out = Vec::new();
    marker(&mut out, tag.0, tag.1, u32::MAX);
    for item in items {
        marker(&mut out, 0xFFFE, 0xE000, u32::MAX);
        out.extend_from_slice(item);
        marker(&mut out, 0xFFFE, 0xE00D, 0);
    }
    marker(&mut out, 0xFFFE, 0xE0DD, 0);
    out
}

fn le<T: Copy, const N: usize>(values: &[T], bytes: fn(T) -> [u8; N]) -> Vec<u8> {
    values.iter().flat_map(|v| bytes(*v)).collect()
}

fn code(value: &[u8], scheme: &[u8], meaning: &[u8]) -> Vec<u8> {
    [
        implicit(tag::CODE_VALUE, value),
        implicit(tag::CODING_SCHEME_DESIGNATOR, scheme),
        implicit(tag::CODE_MEANING, meaning),
    ]
    .concat()
}

/// A file with an explicit VR meta group naming `syntax` and `body` as its
/// data set.
fn part10_file(syntax: &str, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out.extend_from_slice(b"DICM");
    let mut uid = syntax.as_bytes().to_vec();
    if uid.len() % 2 == 1 {
        uid.push(0);
    }
    out.extend_from_slice(&[0x02, 0x00, 0x10, 0x00, b'U', b'I']);
    out.extend_from_slice(&(uid.len() as u16).to_le_bytes());
    out.extend_from_slice(&uid);
    out.extend_from_slice(body);
    out
}

/// A minimal bulk annotation object in implicit VR, as another producer
/// might write it: no eov attributes, a closing vertex repeated, a rotated
/// rectangle and ellipse, a group labelled only by its property type,
/// measurements, and two groups eov cannot use.
fn foreign_implicit_file() -> Vec<u8> {
    let nuclei = [
        implicit(tag::ANNOTATION_GROUP_NUMBER, &le(&[1u16], u16::to_le_bytes)),
        implicit(
            tag::RECOMMENDED_DISPLAY_CIELAB_VALUE,
            &le(&rgb_to_lab(Color(255, 0, 0)), u16::to_le_bytes),
        ),
        implicit(
            tag::POINT_COORDINATES_DATA,
            &le(
                &[
                    10.0f32, 10.0, 20.0, 10.0, 15.0, 20.0, // a triangle
                    100.0, 100.0, 140.0, 100.0, 140.0, 140.0, 100.0, 140.0, 100.0, 100.0,
                ],
                f32::to_le_bytes,
            ),
        ),
        implicit(
            tag::LONG_PRIMITIVE_POINT_INDEX_LIST,
            &le(&[1u32, 7], u32::to_le_bytes),
        ),
        implicit_sequence(
            tag::MEASUREMENTS_SEQUENCE,
            &[[
                implicit_sequence(
                    tag::MEASUREMENT_UNITS_CODE_SEQUENCE,
                    &[code(b"um2 ", b"UCUM", b"um2 ")],
                ),
                implicit_sequence(
                    tag::CONCEPT_NAME_CODE_SEQUENCE,
                    &[code(b"42798000", b"SCT ", b"Area")],
                ),
                implicit_sequence(
                    tag::MEASUREMENT_VALUES_SEQUENCE,
                    &[implicit(
                        tag::FLOATING_POINT_VALUES,
                        &le(&[12.5f32, 400.0], f32::to_le_bytes),
                    )],
                ),
            ]
            .concat()],
        ),
        implicit(tag::ANNOTATION_GROUP_UID, b"1.2.3.4.5.6\0"),
        implicit(tag::ANNOTATION_GROUP_LABEL, b"Nuclei"),
        implicit(tag::ANNOTATION_GROUP_GENERATION_TYPE, b"AUTOMATIC "),
        implicit_sequence(
            tag::ANNOTATION_PROPERTY_TYPE_CODE_SEQUENCE,
            &[code(b"84640000", b"SCT ", b"Nucleus ")],
        ),
        implicit(tag::NUMBER_OF_ANNOTATIONS, &le(&[2u32], u32::to_le_bytes)),
        implicit(tag::GRAPHIC_TYPE, b"POLYGON "),
    ]
    .concat();
    let cells = [
        implicit(
            tag::POINT_COORDINATES_DATA,
            &le(&[5.5f32, 6.5, 7.25, 8.75], f32::to_le_bytes),
        ),
        implicit(tag::ANNOTATION_GROUP_LABEL, b"Lymphocyte"),
        // Wrong: the data hold two.
        implicit(tag::NUMBER_OF_ANNOTATIONS, &le(&[3u32], u32::to_le_bytes)),
        implicit(tag::GRAPHIC_TYPE, b"POINT "),
    ]
    .concat();
    let boxes = [
        implicit(
            tag::DOUBLE_POINT_COORDINATES_DATA,
            &le(
                &[
                    0.0f64, 0.0, 50.0, 0.0, 50.0, 30.0, 0.0, 30.0, // axis-aligned
                    100.0, 50.0, 150.0, 100.0, 100.0, 150.0, 50.0, 100.0, // a diamond
                ],
                f64::to_le_bytes,
            ),
        ),
        implicit(tag::ANNOTATION_GROUP_LABEL, b""),
        implicit_sequence(
            tag::ANNOTATION_PROPERTY_TYPE_CODE_SEQUENCE,
            &[code(b"108369006 ", b"SCT ", b"Tumor ")],
        ),
        implicit(tag::GRAPHIC_TYPE, b"RECTANGLE "),
    ]
    .concat();
    let fields = [
        implicit(
            tag::POINT_COORDINATES_DATA,
            &le(
                &[
                    500.0f32, 400.0, 500.0, 600.0, 450.0, 500.0, 550.0, 500.0, // tall
                    600.0, 600.0, 700.0, 700.0, 680.0, 620.0, 620.0, 680.0, // rotated
                ],
                f32::to_le_bytes,
            ),
        ),
        implicit(tag::ANNOTATION_GROUP_LABEL, b"Fields"),
        implicit(tag::GRAPHIC_TYPE, b"ELLIPSE "),
    ]
    .concat();
    let odd = [
        // Three values: not (column, row) pairs.
        implicit(
            tag::POINT_COORDINATES_DATA,
            &le(&[1.0f32, 2.0, 3.0], f32::to_le_bytes),
        ),
        implicit(tag::ANNOTATION_GROUP_LABEL, b"Odd "),
        implicit(tag::GRAPHIC_TYPE, b"POINT "),
    ]
    .concat();
    let unknown = [
        implicit(
            tag::POINT_COORDINATES_DATA,
            &le(&[1.0f32, 2.0], f32::to_le_bytes),
        ),
        implicit(tag::ANNOTATION_GROUP_LABEL, b"Cloud "),
        implicit(tag::GRAPHIC_TYPE, b"MULTIPOINT"),
    ]
    .concat();
    let body = [
        implicit(tag::SOP_CLASS_UID, SOP_CLASS_UID.as_bytes()),
        implicit(tag::STUDY_INSTANCE_UID, b"1.2.3.4\0"),
        implicit(tag::REFERENCED_IMAGE_SEQUENCE, &{
            let item = [
                implicit(tag::REFERENCED_SOP_CLASS_UID, WSI_SOP_CLASS_UID.as_bytes()),
                implicit(tag::REFERENCED_SOP_INSTANCE_UID, b"1.2.3.4.9\0"),
            ]
            .concat();
            // A defined-length sequence with a defined-length item.
            let mut out = Vec::new();
            out.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            out.extend_from_slice(&(item.len() as u32).to_le_bytes());
            out.extend_from_slice(&item);
            out
        }),
        implicit(tag::PIXEL_ORIGIN_INTERPRETATION, b"VOLUME"),
        implicit(tag::ANNOTATION_COORDINATE_TYPE, b"2D"),
        implicit_sequence(
            tag::ANNOTATION_GROUP_SEQUENCE,
            &[nuclei, cells, boxes, fields, odd, unknown],
        ),
    ]
    .concat();
    part10_file(part10::IMPLICIT_VR_LITTLE_ENDIAN, &body)
}

#[test]
fn a_hand_assembled_implicit_vr_file_from_another_producer_is_read() {
    let bytes = foreign_implicit_file();
    assert_eq!(read(&bytes).syntax, part10::Syntax::ImplicitLittle);
    let doc = parse(&bytes, "slide").unwrap();
    let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Nuclei", "Lymphocyte", "Tumor", "Fields"]);
    // Labels (or, without one, the property type) are classes.
    for layer in &doc.layers {
        assert!(
            layer
                .annotations
                .iter()
                .all(|a| a.class.as_deref() == Some(layer.name.as_str()))
        );
    }
    let nuclei = &doc.layers[0];
    assert_eq!(
        nuclei.annotations[0].geometry,
        Geometry::Polygon {
            exterior: vec![[10.0, 10.0], [20.0, 10.0], [15.0, 20.0]],
            holes: vec![]
        }
    );
    // The repeated closing vertex is dropped.
    assert_eq!(
        nuclei.annotations[1].geometry,
        Geometry::Polygon {
            exterior: vec![
                [100.0, 100.0],
                [140.0, 100.0],
                [140.0, 140.0],
                [100.0, 140.0]
            ],
            holes: vec![]
        }
    );
    // Measurements become properties.
    assert_eq!(
        nuclei.annotations[0].properties,
        [("Area [um2]".to_string(), "12.5".to_string())]
    );
    assert_eq!(
        nuclei.annotations[1].properties,
        [("Area [um2]".to_string(), "400".to_string())]
    );
    let red = doc.classes.iter().find(|c| c.name == "Nuclei").unwrap();
    assert_eq!(red.color, Some(Color(255, 0, 0)));
    assert_eq!(nuclei.color, Some(Color(255, 0, 0)));
    let cells = &doc.layers[1].annotations;
    assert_eq!(cells[0].geometry, Geometry::Point { x: 5.5, y: 6.5 });
    assert_eq!(cells[1].geometry, Geometry::Point { x: 7.25, y: 8.75 });
    let boxes = &doc.layers[2].annotations;
    assert_eq!(
        boxes[0].geometry,
        Geometry::Rectangle {
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 30.0
        }
    );
    assert!(
        matches!(&boxes[1].geometry, Geometry::Polygon { exterior, .. } if exterior.len() == 4)
    );
    let fields = &doc.layers[3].annotations;
    assert_eq!(
        fields[0].geometry,
        Geometry::Ellipse {
            cx: 500.0,
            cy: 500.0,
            rx: 50.0,
            ry: 100.0
        }
    );
    let Geometry::Polygon { exterior, .. } = &fields[1].geometry else {
        panic!("{:?}", fields[1].geometry)
    };
    // The rotated ellipse's polygon passes through the ends of its axes.
    assert_eq!(exterior.len(), 64);
    assert!(
        exterior
            .iter()
            .any(|p| (p[0] - 700.0).abs() < 1e-3 && (p[1] - 700.0).abs() < 1e-3)
    );
    assert!(
        exterior
            .iter()
            .any(|p| (p[0] - 680.0).abs() < 1e-3 && (p[1] - 620.0).abs() < 1e-3)
    );
    // What could not be used is skipped with a reason, what was odd noted.
    assert_eq!(doc.skipped.len(), 2, "{:?}", doc.skipped);
    assert!(
        doc.skipped
            .keys()
            .any(|k| k.contains("(column, row) pairs"))
    );
    assert!(doc.skipped.keys().any(|k| k.contains("MULTIPOINT")));
    let warnings = doc.warnings.join(" ");
    assert!(warnings.contains("says it has 3"), "{warnings}");
    assert!(warnings.contains("2 rectangles and ellipses"), "{warnings}");
    // The image it was made for.
    let reference = doc.slide.dicom.unwrap();
    assert_eq!(reference.instances, ["1.2.3.4.9"]);
    assert_eq!(reference.study.as_deref(), Some("1.2.3.4"));
}

/// A bulk annotation object written with our own writer from `groups`.
fn explicit_file(edit: impl FnOnce(&mut DataSet), groups: Vec<DataSet>) -> Vec<u8> {
    let mut ds = DataSet::new();
    ds.put_text(tag::SOP_CLASS_UID, Vr::UI, SOP_CLASS_UID);
    ds.put_text(tag::ANNOTATION_COORDINATE_TYPE, Vr::CS, "2D");
    ds.put_items(tag::ANNOTATION_GROUP_SEQUENCE, groups);
    edit(&mut ds);
    part10::write(
        &part10::FileMeta {
            sop_class_uid: SOP_CLASS_UID,
            sop_instance_uid: "1.2.3",
            implementation_class_uid: IMPLEMENTATION_CLASS_UID,
            implementation_version_name: "TEST",
        },
        &ds,
    )
    .unwrap()
}

fn line_group(label: &str, values: &[f32], starts: Option<&[u32]>) -> DataSet {
    let mut g = DataSet::new();
    g.put_text(tag::ANNOTATION_GROUP_LABEL, Vr::LO, label);
    g.put_text(tag::GRAPHIC_TYPE, Vr::CS, "POLYLINE");
    g.put_f32s(tag::POINT_COORDINATES_DATA, Vr::OF, values);
    if let Some(starts) = starts {
        g.put_u32s(tag::LONG_PRIMITIVE_POINT_INDEX_LIST, Vr::OL, starts);
    }
    g
}

#[test]
fn point_index_lists_that_count_points_are_recognised() {
    let values = [0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 50.0, 50.0, 60.0, 60.0];
    // Value indices (1, 7): lines of 3 and 2 points.
    let doc = parse(
        &explicit_file(|_| {}, vec![line_group("Lines", &values, Some(&[1, 7]))]),
        "x",
    )
    .unwrap();
    let lines: Vec<usize> = doc.layers[0]
        .annotations
        .iter()
        .map(|a| a.geometry.vertex_count())
        .collect();
    assert_eq!(lines, [3, 2]);
    assert!(doc.warnings.is_empty());
    // Point indices (1, 4) cannot be value indices of 2D points.
    let doc = parse(
        &explicit_file(|_| {}, vec![line_group("Lines", &values, Some(&[1, 4]))]),
        "x",
    )
    .unwrap();
    let lines: Vec<usize> = doc.layers[0]
        .annotations
        .iter()
        .map(|a| a.geometry.vertex_count())
        .collect();
    assert_eq!(lines, [3, 2]);
    assert!(
        doc.warnings[0].contains("counts points"),
        "{:?}",
        doc.warnings
    );
    // 64-bit (OV) index lists are read too.
    let mut group = line_group("Lines", &values, None);
    group.put_bytes(
        tag::LONG_PRIMITIVE_POINT_INDEX_LIST,
        Vr::OV,
        [1u64, 7].iter().flat_map(|v| v.to_le_bytes()).collect(),
    );
    let doc = parse(&explicit_file(|_| {}, vec![group]), "x").unwrap();
    assert_eq!(doc.layers[0].annotations.len(), 2);
}

#[test]
fn malformed_groups_are_skipped_with_reasons() {
    let values = [0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 50.0, 50.0];
    let mut unreadable = line_group("Unreadable", &[], Some(&[1]));
    unreadable.put_bytes(tag::POINT_COORDINATES_DATA, Vr::OF, vec![0; 6]);
    let mut no_type = line_group("No type", &values, Some(&[1]));
    no_type.remove(tag::GRAPHIC_TYPE);
    let mut nan = DataSet::new();
    nan.put_text(tag::ANNOTATION_GROUP_LABEL, Vr::LO, "NaN");
    nan.put_text(tag::GRAPHIC_TYPE, Vr::CS, "POINT");
    nan.put_f32s(
        tag::POINT_COORDINATES_DATA,
        Vr::OF,
        &[f32::NAN, 1.0, 2.0, 3.0],
    );
    let mut uncounted = line_group("Uncounted", &values, None);
    uncounted.put_u32(tag::NUMBER_OF_ANNOTATIONS, 1);
    let mut odd_box = DataSet::new();
    odd_box.put_text(tag::GRAPHIC_TYPE, Vr::CS, "RECTANGLE");
    odd_box.put_f32s(tag::POINT_COORDINATES_DATA, Vr::OF, &values[..6]);
    let mut empty_list = line_group("Empty list", &values, None);
    empty_list.put_bytes(tag::LONG_PRIMITIVE_POINT_INDEX_LIST, Vr::OL, Vec::new());
    let mut bad_list = line_group("Bad list", &values, None);
    // Six bytes: not 32-bit words (odd lengths would be padded).
    bad_list.put_bytes(
        tag::LONG_PRIMITIVE_POINT_INDEX_LIST,
        Vr::OL,
        vec![1, 0, 0, 0, 7, 0],
    );
    let groups = vec![
        line_group("Decreasing", &values, Some(&[5, 1])),
        line_group("Beyond", &values, Some(&[1, 99])),
        line_group("Zero", &values, Some(&[0, 3])),
        line_group("No list", &values, None),
        unreadable,
        no_type,
        nan,
        uncounted,
        odd_box,
        empty_list,
        bad_list,
        DataSet::new(),
    ];
    let doc = parse(&explicit_file(|_| {}, groups), "x").unwrap();
    let total: usize = doc.skipped.values().sum();
    assert_eq!(total, 11, "{:?}", doc.skipped);
    for reason in [
        "point index list that does not fit",
        "without a point index list",
        "unreadable coordinates",
        "without a graphic type",
        "non-finite coordinates",
        "four points each",
        "empty point index list",
        "unreadable point index list",
        "without coordinates",
    ] {
        assert!(
            doc.skipped.keys().any(|k| k.contains(reason)),
            "{reason}: {:?}",
            doc.skipped
        );
    }
    // The point that is fine, and the one line of a group that says it has
    // one, are read.
    let read: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(read, ["NaN", "Uncounted"]);
    assert_eq!(doc.layers[1].annotations[0].geometry.vertex_count(), 4);
}

#[test]
fn unusable_objects_are_refused_with_a_reason() {
    let group = || line_group("Lines", &[0.0, 0.0, 1.0, 1.0], Some(&[1]));
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            explicit_file(
                |ds| ds.put_text(tag::ANNOTATION_COORDINATE_TYPE, Vr::CS, "3D"),
                vec![group()],
            ),
            "3D annotations",
        ),
        (
            explicit_file(
                |ds| ds.put_text(tag::PIXEL_ORIGIN_INTERPRETATION, Vr::CS, "FRAME"),
                vec![group()],
            ),
            "single frames",
        ),
        (
            explicit_file(
                |ds| {
                    ds.remove(tag::ANNOTATION_COORDINATE_TYPE);
                },
                vec![group()],
            ),
            "2D or 3D",
        ),
        (explicit_file(|_| {}, vec![]), "no annotation groups"),
        (
            explicit_file(
                |ds| ds.put_text(tag::SOP_CLASS_UID, Vr::UI, WSI_SOP_CLASS_UID),
                vec![group()],
            ),
            "open it as a slide",
        ),
        (
            explicit_file(
                |ds| ds.put_text(tag::SOP_CLASS_UID, Vr::UI, "1.2.840.10008.5.1.4.1.1.88.34"),
                vec![group()],
            ),
            "structured report",
        ),
        (
            explicit_file(
                |ds| ds.put_text(tag::SOP_CLASS_UID, Vr::UI, "1.2.840.10008.5.1.4.1.1.2"),
                vec![group()],
            ),
            "not an annotation object",
        ),
        (
            part10_file(
                "1.2.840.10008.1.2.1.99",
                &implicit(tag::SOP_CLASS_UID, b"1.2\0"),
            ),
            "deflated",
        ),
    ];
    for (bytes, expected) in cases {
        let error = parse(&bytes, "x").unwrap_err();
        assert!(error.contains(expected), "{expected}: {error}");
    }
    // Through the format detection: a .dcm that is not DICOM.
    let error = super::super::parse(b"\xff\xfebinary", "slide.dcm").unwrap_err();
    assert!(error.contains("DICM"), "{error}");
}

#[test]
fn truncated_and_damaged_files_never_panic() {
    let bytes = exported().bytes;
    for end in 0..bytes.len() {
        let _ = parse(&bytes[..end], "x");
    }
    let foreign = foreign_implicit_file();
    for end in 0..foreign.len() {
        let _ = parse(&foreign[..end], "x");
    }
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for source in [&bytes, &foreign] {
        for _ in 0..5_000 {
            let mut damaged = source.clone();
            for _ in 0..(next() % 8 + 1) {
                let at = 132 + (next() as usize) % (damaged.len() - 132);
                damaged[at] = next() as u8;
            }
            let _ = parse(&damaged, "x");
        }
    }
}

#[test]
fn empty_slides_are_not_exported() {
    let (mut slide, classes) = sample();
    for (_, annotations) in &mut slide.layers {
        annotations.clear();
    }
    assert!(
        export(&slide, &classes, &options(), None)
            .unwrap_err()
            .contains("no annotations")
    );
}

#[test]
fn cielab_colours_round_trip() {
    assert_eq!(rgb_to_lab(Color(255, 255, 255)), [65_535, 0x8080, 0x8080]);
    assert_eq!(rgb_to_lab(Color(0, 0, 0)), [0, 0x8080, 0x8080]);
    for r in (0..=255u8).step_by(5) {
        for g in (0..=255u8).step_by(5) {
            for b in (0..=255u8).step_by(5) {
                let color = Color(r, g, b);
                assert_eq!(lab_to_rgb(rgb_to_lab(color)), color);
            }
        }
    }
    for hex in crate::model::PALETTE {
        let color = Color::parse(hex).unwrap();
        assert_eq!(lab_to_rgb(rgb_to_lab(color)), color, "{hex}");
    }
}

#[test]
fn uids_are_valid() {
    for _ in 0..100 {
        let uid = new_uid();
        assert!(is_uid(&uid) && uid.starts_with("2.25."), "{uid}");
        // No leading zeros in the integer.
        assert!(!uid[5..].starts_with('0') || uid.len() == 6);
    }
    assert!(is_uid(IMPLEMENTATION_CLASS_UID));
    assert!(!is_uid("1..2") && !is_uid("1.2.") && !is_uid("1.a") && !is_uid(""));
    assert!(IMPLEMENTATION_VERSION_NAME.len() <= 16);
}
