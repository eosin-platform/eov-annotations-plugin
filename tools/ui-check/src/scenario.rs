//! Realistic plugin states, produced through the SDK's mock host: the
//! properties the plugin pushes to its surfaces in each.

use annotations::AnnotationsPlugin;
use annotations::sidebar;
use eov_plugin_api::prelude::*;
use eov_plugin_api::testing::{MockHost, TestHarness, test_init};
use serde_json::{Map, Value as Json, json};
use std::path::{Path, PathBuf};

type H = TestHarness<AnnotationsPlugin>;

fn settle(h: &H) {
    for _ in 0..50 {
        h.ui_callback("", "__sync", vec![]).unwrap();
        let posted: Vec<String> = std::mem::take(&mut h.state().posted);
        if posted.is_empty() {
            return;
        }
        for token in posted {
            h.send(HostEvent::Posted { token }).unwrap();
        }
    }
}

fn info(id: FileId, name: &str, dir: &Path) -> FileInfo {
    let path = dir.join(format!("{name}.svs"));
    if !path.exists() {
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&path, bytes).unwrap();
    }
    FileInfo {
        file_id: id,
        path: path.to_string_lossy().into_owned(),
        filename: format!("{name}.svs"),
        width: 20_000,
        height: 16_000,
        mpp_x: Some(0.25),
        mpp_y: Some(0.25),
        identity: Some(format!("eov-slide-1:{name}")),
        ..Default::default()
    }
}

fn open(h: &H, file: FileInfo) {
    let id = file.file_id;
    h.state().snapshot.files.push(file.clone());
    h.send(HostEvent::FileOpened { file }).unwrap();
    activate(h, id);
}

fn activate(h: &H, id: FileId) {
    h.send(HostEvent::ActiveChanged {
        pane: Some(1),
        file_id: Some(id),
    })
    .unwrap();
    settle(h);
}

fn draw(h: &H, tool: &str, file_id: FileId, shape: Shape, modifiers: Modifiers) {
    h.send(HostEvent::ShapeDrawn {
        tool_id: tool.into(),
        pane: 1,
        file_id,
        shape,
        modifiers,
    })
    .unwrap();
    settle(h);
}

fn square(x: f64, y: f64, s: f64) -> Shape {
    Shape::Polygon {
        exterior: vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]],
        holes: vec![],
    }
}

fn call(h: &H, surface: &str, name: &str, args: Vec<Json>) {
    if let Err(e) = h.ui_callback(surface, name, args) {
        eprintln!("note: callback {name} failed in the scenario: {e}");
    }
    settle(h);
}

fn sidebar_id(h: &H) -> Option<String> {
    h.state()
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Sidebar)
        .map(|(id, _)| id.clone())
}

fn properties_of(h: &H, surface: &str) -> Map<String, Json> {
    h.state().surfaces[surface].properties.clone()
}

fn pending_request(h: &H) -> Option<String> {
    let ids = h.ui_callback("", "__pending", vec![]).ok()?;
    ids.as_array()?
        .iter()
        .filter_map(Json::as_str)
        .max_by_key(|id| {
            id.rsplit('-')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        })
        .map(str::to_string)
}

/// Answers the newest file dialog with `path`.
fn answer(h: &H, path: &Path) -> Result<(), String> {
    let request_id = pending_request(h).ok_or("no file dialog")?;
    h.send(HostEvent::DialogResult {
        request_id,
        paths: vec![path.to_string_lossy().into_owned()],
    })
    .map_err(|e| e.to_string())?;
    settle(h);
    Ok(())
}

fn row_id(props: &Map<String, Json>, kind: i64, nth: usize) -> Option<(String, String)> {
    props["rows"]
        .as_array()?
        .iter()
        .filter(|r| r["kind"].as_i64() == Some(kind))
        .nth(nth)
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["layer-id"].as_str().unwrap().to_string(),
            )
        })
}

pub struct Samples {
    pub sidebar: Vec<(String, Map<String, Json>)>,
    pub import: Vec<(String, Map<String, Json>)>,
}

const GEOJSON: &str = r#"{"type": "FeatureCollection", "features": [
    {"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [[[10,10],[110,10],[110,110],[10,110],[10,10]]]},
     "properties": {"objectType": "annotation", "name": "Tumour", "classification": {"name": "Tumour", "color": [200, 0, 0]}}},
    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-50, 20]}, "properties": {}}
]}"#;

/// A second reader of slide-a: most of the large region, some of the
/// counted cells and one more.
const SECOND_READER: &str = r#"{"type": "FeatureCollection", "features": [
    {"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [[[400,100],[2100,100],[2100,2100],[400,2100],[400,100]]]},
     "properties": {"classification": {"name": "Tumour"}}},
    {"type": "Feature", "geometry": {"type": "MultiPoint", "coordinates": [[252,401],[302,399],[405,400],[600,410],[900,900]]},
     "properties": {"classification": {"name": "Positive"}}},
    {"type": "Feature", "geometry": {"type": "MultiPoint", "coordinates": [[201,400],[500,402]]},
     "properties": {"classification": {"name": "Negative"}}}
]}"#;

pub fn samples() -> Result<Samples, String> {
    let mut init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    // A database of the plugin before eov 1.0 in the host's legacy folder
    // (only its presence matters here); the offer is declined, so the Data
    // tab keeps offering it.
    let legacy = dir.join("legacy-config");
    std::fs::create_dir_all(&legacy).map_err(|e| e.to_string())?;
    std::fs::write(legacy.join("annotations.db"), b"").map_err(|e| e.to_string())?;
    init.legacy_config_dir = Some(legacy.to_string_lossy().into_owned());
    let h = TestHarness::<AnnotationsPlugin>::with_host(MockHost::new(), init)
        .map_err(|e| e.to_string())?;
    settle(&h);
    let offer = pending_request(&h).ok_or("the pre-1.0 database was not offered")?;
    h.send(HostEvent::ConfirmResult {
        request_id: offer,
        confirmed: false,
    })
    .map_err(|e| e.to_string())?;
    settle(&h);
    let mut out = Samples {
        sidebar: vec![],
        import: vec![],
    };
    h.send(HostEvent::ToolbarButtonClicked {
        button_id: annotations::tools::SIDEBAR_BUTTON.into(),
    })
    .map_err(|e| e.to_string())?;
    settle(&h);
    let sb = sidebar_id(&h).ok_or("the sidebar did not open")?;
    out.sidebar
        .push(("no slide".into(), properties_of(&h, &sb)));

    // An old slide, so the Data tab lists another slide.
    open(&h, info(9, "old-slide", &dir));
    draw(
        &h,
        "point",
        9,
        Shape::Point { x: 50.0, y: 50.0 },
        Modifiers::default(),
    );
    h.send(HostEvent::FileClosed { file_id: 9 })
        .map_err(|e| e.to_string())?;
    h.state().snapshot.files.retain(|f| f.file_id != 9);
    settle(&h);

    open(&h, info(1, "slide-a", &dir));
    out.sidebar
        .push(("empty slide".into(), properties_of(&h, &sb)));
    for shape in [square(100.0, 100.0, 2000.0), square(5000.0, 5000.0, 400.0)] {
        draw(&h, "polygon", 1, shape, Modifiers::default());
    }
    draw(
        &h,
        "ellipse",
        1,
        Shape::Ellipse {
            cx: 9000.0,
            cy: 9000.0,
            rx: 300.0,
            ry: 200.0,
        },
        Modifiers::default(),
    );
    draw(
        &h,
        "polyline",
        1,
        Shape::Polyline {
            points: vec![[10.0, 10.0], [400.0, 300.0], [800.0, 100.0]],
        },
        Modifiers::default(),
    );
    draw(
        &h,
        "arrow",
        1,
        Shape::Polyline {
            points: vec![[1200.0, 900.0], [1500.0, 700.0]],
        },
        Modifiers::default(),
    );
    draw(
        &h,
        "text",
        1,
        Shape::Point { x: 300.0, y: 300.0 },
        Modifiers::default(),
    );
    draw(
        &h,
        "point",
        1,
        Shape::Point { x: 700.0, y: 700.0 },
        Modifiers::default(),
    );
    // Count positive cells (click) and negative ones (Shift+click).
    let class_index = |name: &str| -> Option<usize> {
        properties_of(&h, &sb)["class-names"]
            .as_array()?
            .iter()
            .position(|n| n == name)
    };
    let (positive, negative) = (
        class_index("Positive").ok_or("no Positive class")?,
        class_index("Negative").ok_or("no Negative class")?,
    );
    call(&h, &sb, "count-active", vec![json!(positive)]);
    call(&h, &sb, "count-alternate", vec![json!(negative)]);
    for i in 0..12 {
        let shift = Modifiers {
            shift: i % 3 == 0,
            ..Default::default()
        };
        draw(
            &h,
            "count",
            1,
            Shape::Point {
                x: 200.0 + 50.0 * i as f64,
                y: 400.0,
            },
            shift,
        );
    }
    draw(
        &h,
        "field",
        1,
        Shape::Point {
            x: 1000.0,
            y: 1000.0,
        },
        Modifiers::default(),
    );
    out.sidebar
        .push(("annotated slide".into(), properties_of(&h, &sb)));

    // The large region selected, with a class, a name and a property:
    // details, measurements and a tally with densities.
    let props = properties_of(&h, &sb);
    let (first, layer) = props["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|r| {
                r["kind"] == 1
                    && r["detail"]
                        .as_str()
                        .is_some_and(|d| d.contains("0.2500 mm²"))
            })
        })
        .map(|r| {
            (
                r["id"].as_str().unwrap_or_default().to_string(),
                r["layer-id"].as_str().unwrap_or_default().to_string(),
            )
        })
        .ok_or("the 0.25 mm² region is not listed")?;
    call(
        &h,
        &sb,
        "row-clicked",
        vec![json!(first), json!(layer), json!(false)],
    );
    let owner = properties_of(&h, &sb)["detail-id"].clone();
    call(&h, &sb, "property-add", vec![owner.clone()]);
    call(
        &h,
        &sb,
        "property-committed",
        vec![owner.clone(), json!(0), json!("grade"), json!("2")],
    );
    call(
        &h,
        &sb,
        "detail-name-committed",
        vec![owner, json!("Region of interest")],
    );
    let owner = properties_of(&h, &sb)["detail-id"].clone();
    call(
        &h,
        &sb,
        "comment-add",
        vec![owner, json!("Looks like DCIS; please check the margin.")],
    );
    let tumour = class_index("Tumour").ok_or("no Tumour class")?;
    call(&h, &sb, "detail-class-chosen", vec![json!(tumour)]);
    call(&h, &sb, "detail-status-chosen", vec![json!(1)]);
    out.sidebar
        .push(("one selected".into(), properties_of(&h, &sb)));
    call(&h, &sb, "tab-changed", vec![json!(sidebar::TAB_COUNT)]);
    out.sidebar
        .push(("count tab".into(), properties_of(&h, &sb)));

    // Several selected.
    let props = properties_of(&h, &sb);
    let (second, layer2) = row_id(&props, 1, 1).ok_or("no second row")?;
    call(
        &h,
        &sb,
        "row-clicked",
        vec![json!(second), json!(layer2), json!(true)],
    );
    out.sidebar
        .push(("two selected".into(), properties_of(&h, &sb)));

    // Filtered, collapsed, layer settings, classes and data tab.
    call(&h, &sb, "filter-edited", vec![json!("poly")]);
    out.sidebar
        .push(("filtered".into(), properties_of(&h, &sb)));
    call(&h, &sb, "filter-edited", vec![json!("")]);
    call(&h, &sb, "toggle-collapsed", vec![json!(layer)]);
    call(&h, &sb, "class-new", vec![]);
    // A class hotkey eov refused.
    let tumour = properties_of(&h, &sb)["classes"]
        .as_array()
        .and_then(|c| c.first())
        .and_then(|c| c["id"].as_str())
        .map(str::to_string)
        .ok_or("no classes")?;
    call(&h, &sb, "class-hotkey", vec![json!(tumour), json!("q")]);
    h.send(HostEvent::RegistrationRefused {
        what: "hotkey".into(),
        id: format!("{}{tumour}", annotations::tools::CLASS_HOTKEY_PREFIX),
        reason: "another plugin uses Q".into(),
        shown_reason: Some("another plugin uses Q".into()),
    })
    .map_err(|e| e.to_string())?;
    settle(&h);
    call(&h, &sb, "tab-changed", vec![json!(sidebar::TAB_CLASSES)]);
    out.sidebar.push(("classes".into(), properties_of(&h, &sb)));
    // A blinded copy for a second reader, and the comparison with what
    // that reader drew.
    let blinded = dir.join("slide-a.blinded.json");
    call(&h, &sb, "menu", vec![json!("export-blinded")]);
    answer(&h, &blinded)?;
    if !blinded.exists() {
        return Err("the blinded copy was not written".into());
    }
    let reader2 = dir.join("reader-2.geojson");
    std::fs::write(&reader2, SECOND_READER).map_err(|e| e.to_string())?;
    call(&h, &sb, "menu", vec![json!("compare")]);
    answer(&h, &reader2)?;
    call(&h, &sb, "tab-changed", vec![json!(sidebar::TAB_DATA)]);
    call(&h, &sb, "orphans-refresh", vec![]);
    let props = properties_of(&h, &sb);
    if props["compare-rows"].as_array().is_none_or(|r| r.len() < 3) {
        return Err(format!(
            "the comparison with the second reader is missing: {:?}",
            props["compare-rows"]
        ));
    }
    if props["orphans"].as_array().is_none_or(|o| o.is_empty()) {
        return Err("the scenario has no other slides to list".into());
    }
    if props["legacy-db"].as_str().is_none_or(str::is_empty) {
        return Err("the Data tab does not offer the pre-1.0 database".into());
    }
    out.sidebar.push(("data tab".into(), props));

    // The import preview; then the same file again, whose layer now exists.
    let path = dir.join("cells.geojson");
    std::fs::write(&path, GEOJSON).map_err(|e| e.to_string())?;
    for label in ["import preview", "import with conflicts"] {
        call(&h, &sb, "menu", vec![json!("import")]);
        let request_id = pending_request(&h).ok_or("no file dialog for the import")?;
        h.send(HostEvent::DialogResult {
            request_id,
            paths: vec![path.to_string_lossy().into_owned()],
        })
        .map_err(|e| e.to_string())?;
        settle(&h);
        let modal = h
            .state()
            .surfaces
            .iter()
            .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Modal)
            .map(|(id, s)| {
                (
                    id.clone(),
                    s.spec.ui_path.clone(),
                    s.spec.component.clone(),
                    s.properties.clone(),
                )
            });
        let (modal_id, ui_path, component, props) =
            modal.ok_or("the import dialog did not open")?;
        if ui_path != sidebar::IMPORT_UI || component != sidebar::IMPORT_COMPONENT {
            return Err(format!("unexpected modal {ui_path} / {component}"));
        }
        out.import.push((label.into(), props));
        call(&h, &modal_id, "import-confirm", vec![json!(0)]);
    }
    if out
        .import
        .last()
        .is_none_or(|(_, p)| p["conflicts"].as_str().is_none_or(str::is_empty))
    {
        return Err("the second import shows no conflicts".into());
    }
    // An NDPA on a slide that is not an NDPI: the preview explains.
    let ndpa = dir.join("case.ndpa");
    std::fs::write(
        &ndpa,
        r##"<?xml version="1.0"?><annotations><ndpviewstate id="1"><title>T</title>
           <annotation type="pin" color="#ff0000"><x>0</x><y>0</y></annotation></ndpviewstate></annotations>"##,
    )
    .map_err(|e| e.to_string())?;
    call(&h, &sb, "menu", vec![json!("import")]);
    let request_id = pending_request(&h).ok_or("no file dialog for the NDPA import")?;
    h.send(HostEvent::DialogResult {
        request_id,
        paths: vec![ndpa.to_string_lossy().into_owned()],
    })
    .map_err(|e| e.to_string())?;
    settle(&h);
    let modal = h
        .state()
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Modal)
        .map(|(id, s)| (id.clone(), s.properties.clone()));
    let (modal_id, props) = modal.ok_or("the NDPA import dialog did not open")?;
    if props["blocked"].as_str().is_none_or(str::is_empty) {
        return Err("the NDPA preview is not blocked".into());
    }
    out.import.push(("ndpa blocked".into(), props));
    call(&h, &modal_id, "import-cancel", vec![]);
    Ok(out)
}
