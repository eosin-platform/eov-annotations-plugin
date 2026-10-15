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

pub fn samples() -> Result<Samples, String> {
    let init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let h = TestHarness::<AnnotationsPlugin>::with_host(MockHost::new(), init)
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
    call(&h, &sb, "tab-changed", vec![json!(sidebar::TAB_DATA)]);
    call(&h, &sb, "orphans-refresh", vec![]);
    let props = properties_of(&h, &sb);
    if props["orphans"].as_array().is_none_or(|o| o.is_empty()) {
        return Err("the scenario has no other slides to list".into());
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
    Ok(out)
}
