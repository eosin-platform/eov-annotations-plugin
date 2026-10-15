//! End-to-end tests of the plugin through the SDK's mock host (the same
//! FFI adapter the real host uses). Everything happens in temporary
//! directories; nothing touches the home directory.

use super::*;
use crate::tools::{ARROW, BRUSH, COUNT, FIELD, POINT, POLYGON, RECTANGLE, TEXT};
use eov_plugin_api::testing::{MockHost, TestHarness, test_init};
use std::path::Path;

type H = TestHarness<AnnotationsPlugin>;

fn slide_file(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    if !path.exists() {
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&path, bytes).unwrap();
    }
    path.to_string_lossy().into_owned()
}

fn info(id: FileId, identity: &str, dir: &Path) -> FileInfo {
    FileInfo {
        file_id: id,
        path: slide_file(dir, &format!("{identity}.svs")),
        filename: format!("{identity}.svs"),
        width: 10_000,
        height: 8_000,
        mpp_x: Some(0.25),
        mpp_y: Some(0.25),
        identity: Some(format!("eov-slide-1:{identity}")),
        ..Default::default()
    }
}

fn start(init: PluginInit) -> H {
    TestHarness::with_host(MockHost::new(), init).unwrap()
}

fn fresh() -> (H, PathBuf) {
    let init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    (start(init), dir)
}

/// Waits for the workers and delivers what the plugin posted to itself.
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

fn draw(h: &H, tool: &str, file_id: FileId, shape: Shape) {
    draw_with(h, tool, file_id, shape, Modifiers::default());
}

fn draw_with(h: &H, tool: &str, file_id: FileId, shape: Shape, modifiers: Modifiers) {
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

/// Every object of our host layers for a file: (host layer, object).
fn objects(h: &H, file_id: FileId) -> Vec<(String, OverlayObject)> {
    let prefix = format!("ann.{file_id}.");
    let state = h.state();
    state
        .layers
        .iter()
        .filter(|(id, _)| id.starts_with(&prefix))
        .flat_map(|(id, layer)| layer.objects.values().map(move |o| (id.clone(), o.clone())))
        .collect()
}

fn reference(layer: &str, object: &OverlayObject) -> ObjectRef {
    ObjectRef {
        layer_id: layer.to_string(),
        object_id: object.object_id.clone(),
    }
}

fn square(x: f64, y: f64, s: f64) -> Shape {
    Shape::Polygon {
        exterior: vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]],
        holes: vec![],
    }
}

fn last_undo(h: &H) -> UndoPushArgs {
    h.state().undo.last().cloned().expect("an undo entry")
}

fn undo(h: &H) {
    h.send(HostEvent::Undo {
        entry_id: last_undo(h).entry_id,
    })
    .unwrap();
    settle(h);
}

fn notifications(h: &H) -> Vec<String> {
    h.state()
        .notifications
        .iter()
        .map(|n| {
            format!(
                "{:?}: {} {}",
                n.level,
                n.message,
                n.detail.clone().unwrap_or_default()
            )
        })
        .collect()
}

fn notified(h: &H, text: &str) -> bool {
    notifications(h).iter().any(|n| n.contains(text))
}

fn open_sidebar(h: &H) -> String {
    h.send(HostEvent::ToolbarButtonClicked {
        button_id: tools::SIDEBAR_BUTTON.into(),
    })
    .unwrap();
    settle(h);
    let state = h.state();
    state
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Sidebar)
        .map(|(id, _)| id.clone())
        .expect("sidebar open")
}

fn prop(h: &H, surface: &str, name: &str) -> Value {
    h.state().surfaces[surface]
        .properties
        .get(name)
        .cloned()
        .unwrap_or(Value::Null)
}

fn callback(h: &H, surface: &str, name: &str, args: Vec<Value>) {
    h.ui_callback(surface, name, args).unwrap();
    settle(h);
}

/// The newest dialog request the plugin waits for.
fn pending_request(h: &H) -> String {
    let ids = h.ui_callback("", "__pending", vec![]).unwrap();
    ids.as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .max_by_key(|id| {
            id.rsplit('-')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        })
        .expect("a pending request")
        .to_string()
}

fn answer_dialog(h: &H, path: &Path) {
    let request_id = pending_request(h);
    h.send(HostEvent::DialogResult {
        request_id,
        paths: vec![path.to_string_lossy().into_owned()],
    })
    .unwrap();
    settle(h);
}

fn confirm(h: &H) {
    let request_id = pending_request(h);
    h.send(HostEvent::ConfirmResult {
        request_id,
        confirmed: true,
    })
    .unwrap();
    settle(h);
}

fn modal(h: &H) -> Option<(String, Map<String, Value>)> {
    h.state()
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Modal)
        .map(|(id, s)| (id.clone(), s.properties.clone()))
}

#[test]
fn registers_tools_and_opens_its_database_in_the_data_dir() {
    let init = test_init("annotations");
    let db = PathBuf::from(&init.data_dir).join("annotations.db");
    let h = start(init);
    settle(&h);
    {
        let state = h.state();
        assert_eq!(state.tools.len(), tools::tool_specs().len());
        for (id, spec) in &state.tools {
            assert!(spec.hotkey.is_some(), "{id} lost its hotkey");
        }
        assert!(state.toolbar_buttons.contains_key(tools::SIDEBAR_BUTTON));
        assert!(state.hud_buttons.contains_key(tools::VISIBILITY_HUD));
    }
    assert!(
        db.exists(),
        "the database lives in the plugin data directory"
    );
    // Default classes exist.
    let sidebar = open_sidebar(&h);
    assert!(prop(&h, &sidebar, "classes").as_array().unwrap().len() >= 5);
}

#[test]
fn points_are_saved_shown_and_undone_through_the_host_history() {
    let init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let h = start(init.clone());
    open(&h, info(1, "a", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 100.0, y: 200.0 });
    let shown = objects(&h, 1);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].1.shape, Shape::Point { x: 100.0, y: 200.0 });
    let entry = last_undo(&h);
    assert_eq!(entry.file_id, Some(1), "undo is scoped to the slide");
    assert_eq!(entry.label, "Add point");

    h.send(HostEvent::Undo {
        entry_id: entry.entry_id.clone(),
    })
    .unwrap();
    settle(&h);
    assert!(objects(&h, 1).is_empty());
    h.send(HostEvent::Redo {
        entry_id: entry.entry_id.clone(),
    })
    .unwrap();
    settle(&h);
    assert_eq!(objects(&h, 1).len(), 1);

    // A restart finds the point in the database.
    drop(h);
    let h = start(init);
    open(&h, info(1, "a", &dir));
    assert_eq!(objects(&h, 1).len(), 1);
}

#[test]
fn points_outside_the_slide_are_refused_with_a_message() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POINT, 1, Shape::Point { x: -5.0, y: 10.0 });
    assert!(objects(&h, 1).is_empty());
    assert!(notified(&h, "outside the slide"));
}

#[test]
fn self_intersecting_polygons_are_split_and_the_user_told() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(
        &h,
        POLYGON,
        1,
        Shape::Polygon {
            exterior: vec![[0.0, 0.0], [1000.0, 1000.0], [1000.0, 0.0], [0.0, 1000.0]],
            holes: vec![],
        },
    );
    assert_eq!(objects(&h, 1).len(), 2);
    assert!(notified(&h, "crossed itself"));
    assert_eq!(h.state().undo.len(), 1, "both parts are one undo step");
}

#[test]
fn edits_are_saved_and_invalid_edits_are_reverted() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POLYGON, 1, square(100.0, 100.0, 100.0));
    let (layer, object) = objects(&h, 1).remove(0);
    let moved = square(150.0, 150.0, 100.0);
    h.send(HostEvent::ObjectEdited {
        object: reference(&layer, &object),
        shape: moved.clone(),
    })
    .unwrap();
    settle(&h);
    assert_eq!(objects(&h, 1)[0].1.shape, moved);
    assert_eq!(last_undo(&h).label, "Edit shape");

    // A vertex drag that makes the outline cross itself is undone (the host
    // shows the edit optimistically).
    let twisted = Shape::Polygon {
        exterior: vec![
            [150.0, 150.0],
            [250.0, 250.0],
            [250.0, 150.0],
            [150.0, 250.0],
        ],
        holes: vec![],
    };
    h.state()
        .layers
        .get_mut(&layer)
        .unwrap()
        .objects
        .get_mut(&object.object_id)
        .unwrap()
        .shape = twisted.clone();
    h.send(HostEvent::ObjectEdited {
        object: reference(&layer, &object),
        shape: twisted,
    })
    .unwrap();
    settle(&h);
    assert_eq!(
        objects(&h, 1)[0].1.shape,
        moved,
        "the host shows the stored shape again"
    );
    assert!(notified(&h, "cross itself"));
    // Undo of the valid edit restores the original.
    undo(&h);
    assert_eq!(objects(&h, 1)[0].1.shape, square(100.0, 100.0, 100.0));
}

#[test]
fn deletes_respect_locks_and_are_one_undo_step() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    draw(&h, POINT, 1, Shape::Point { x: 20.0, y: 20.0 });
    let all = objects(&h, 1);
    assert_eq!(all.len(), 2);
    // Lock the first through the sidebar.
    let sidebar = open_sidebar(&h);
    callback(
        &h,
        &sidebar,
        "row-clicked",
        vec![json!(all[0].1.object_id), json!("x"), json!(false)],
    );
    callback(&h, &sidebar, "detail-locked-toggled", vec![json!(true)]);
    let refs: Vec<ObjectRef> = all.iter().map(|(l, o)| reference(l, o)).collect();
    h.send(HostEvent::ObjectsDeleteRequested { objects: refs })
        .unwrap();
    settle(&h);
    let left = objects(&h, 1);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].1.object_id, all[0].1.object_id);
    assert_eq!(left[0].1.editable, Some(false));
    assert!(notified(&h, "locked"));
    undo(&h);
    assert_eq!(objects(&h, 1).len(), 2);
}

#[test]
fn undo_entries_belong_to_their_own_slide() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    open(&h, info(2, "b", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    draw(&h, POINT, 2, Shape::Point { x: 10.0, y: 10.0 });
    let entries = h.state().undo.clone();
    assert_eq!(entries[0].file_id, Some(1));
    assert_eq!(entries[1].file_id, Some(2));
    // A late undo for a closed slide is reported, not applied elsewhere.
    h.send(HostEvent::FileClosed { file_id: 1 }).unwrap();
    h.send(HostEvent::Undo {
        entry_id: entries[0].entry_id.clone(),
    })
    .unwrap();
    settle(&h);
    assert!(notified(&h, "can no longer be undone"));
    assert_eq!(objects(&h, 2).len(), 1);
}

#[test]
fn the_same_slide_in_two_tabs_shares_one_copy() {
    let (h, dir) = fresh();
    open(&h, info(1, "same", &dir));
    open(&h, info(2, "same", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    assert_eq!(objects(&h, 1).len(), 1);
    assert_eq!(objects(&h, 2).len(), 1, "the other tab shows the point too");
    h.send(HostEvent::FileClosed { file_id: 1 }).unwrap();
    draw(&h, POINT, 2, Shape::Point { x: 20.0, y: 20.0 });
    assert_eq!(objects(&h, 2).len(), 2);
}

#[test]
fn selection_and_hover_follow_the_viewport_and_the_sidebar() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(
        &h,
        RECTANGLE,
        1,
        Shape::Rectangle {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 400.0,
        },
    );
    let (layer, object) = objects(&h, 1).remove(0);
    let sidebar = open_sidebar(&h);
    // The new annotation is selected, in the host too.
    assert_eq!(
        h.state().layers[&layer].selection,
        vec![object.object_id.clone()]
    );
    assert_eq!(prop(&h, &sidebar, "has-detail"), json!(true));
    let measurements = prop(&h, &sidebar, "detail-measurements");
    let area = measurements
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == "Area")
        .unwrap()
        .clone();
    // 400 px at 0.25 µm/px = 100 µm → 10 000 µm².
    assert_eq!(area["value"], "0.01000 mm²");
    // The viewport clears the selection.
    h.send(HostEvent::SelectionChanged { objects: vec![] })
        .unwrap();
    settle(&h);
    assert_eq!(prop(&h, &sidebar, "has-detail"), json!(false));
    // Hover in the viewport highlights the row.
    h.send(HostEvent::ObjectHovered {
        object: Some(reference(&layer, &object)),
        pane: 1,
    })
    .unwrap();
    settle(&h);
    let rows = prop(&h, &sidebar, "rows");
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(object.object_id))
        .unwrap()
        .clone();
    assert_eq!(row["hovered"], json!(true));
    // Clicking the row selects it in the viewport.
    let (_, annotation_layer) = overlay::parse_host_layer_id(&layer).unwrap();
    callback(
        &h,
        &sidebar,
        "row-clicked",
        vec![
            json!(object.object_id),
            json!(annotation_layer),
            json!(false),
        ],
    );
    assert_eq!(
        h.state().layers[&layer].selection,
        vec![object.object_id.clone()]
    );
    // Hovering the row highlights the object in the viewport (the host's
    // hover style), and leaving clears it; the object itself is unchanged.
    callback(&h, &sidebar, "row-hovered", vec![json!(object.object_id)]);
    assert_eq!(
        h.state().layers[&layer].highlighted,
        vec![object.object_id.clone()]
    );
    assert_eq!(
        h.state().layers[&layer].objects[&object.object_id].style,
        object.style
    );
    callback(&h, &sidebar, "row-hovered", vec![json!("")]);
    assert!(h.state().layers[&layer].highlighted.is_empty());
}

#[test]
fn density_follows_edits_of_the_region_and_the_points() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    let sidebar = open_sidebar(&h);
    callback(&h, &sidebar, "tab-changed", vec![json!(sidebar::TAB_COUNT)]);
    // Count with the first class, Shift+click counts the second.
    callback(&h, &sidebar, "count-active", vec![json!(1)]);
    callback(&h, &sidebar, "count-alternate", vec![json!(2)]);
    for i in 0..3 {
        draw(
            &h,
            COUNT,
            1,
            Shape::Point {
                x: 100.0 + i as f64 * 10.0,
                y: 100.0,
            },
        );
    }
    draw_with(
        &h,
        COUNT,
        1,
        Shape::Point { x: 150.0, y: 150.0 },
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    // A 1000 px square region (0.0625 mm²), drawn last so it is selected.
    draw(&h, POLYGON, 1, square(0.0, 0.0, 1000.0));
    let tally = prop(&h, &sidebar, "tally");
    let rows = tally.as_array().unwrap();
    let first = rows
        .iter()
        .find(|r| r["count"] == "3")
        .expect("3 of the active class");
    assert!(
        first["density"].as_str().unwrap().starts_with("48.0 /mm²"),
        "{first}"
    );
    assert!(rows.iter().any(|r| r["count"] == "1"));
    let index = prop(&h, &sidebar, "count-index");
    assert!(index.as_str().unwrap().contains("75.0 %"), "{index}");

    // ANN-2: shrink the region so two points fall outside; the panel
    // follows the edit.
    let (layer, region) = objects(&h, 1)
        .into_iter()
        .find(|(_, o)| matches!(o.shape, Shape::Polygon { .. }))
        .unwrap();
    h.send(HostEvent::ObjectEdited {
        object: reference(&layer, &region),
        shape: square(0.0, 0.0, 115.0),
    })
    .unwrap();
    settle(&h);
    let tally = prop(&h, &sidebar, "tally");
    assert!(
        tally.as_array().unwrap().iter().any(|r| r["count"] == "2"),
        "{tally}"
    );
    // Clearing the counts is one undoable step.
    callback(&h, &sidebar, "count-clear", vec![]);
    confirm(&h);
    assert_eq!(objects(&h, 1).len(), 1);
    undo(&h);
    assert_eq!(objects(&h, 1).len(), 5);
}

#[test]
fn shift_and_alt_drawing_merge_into_and_cut_from_the_selected_region() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POLYGON, 1, square(0.0, 0.0, 1000.0));
    draw_with(
        &h,
        RECTANGLE,
        1,
        Shape::Rectangle {
            x: 400.0,
            y: 400.0,
            width: 200.0,
            height: 200.0,
        },
        Modifiers {
            alt: true,
            ..Default::default()
        },
    );
    let all = objects(&h, 1);
    assert_eq!(all.len(), 1, "no new annotation, the region was cut");
    let Shape::Polygon { holes, .. } = &all[0].1.shape else {
        panic!("{:?}", all[0].1.shape)
    };
    assert_eq!(holes.len(), 1);
    draw_with(
        &h,
        POLYGON,
        1,
        square(900.0, 900.0, 300.0),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(objects(&h, 1).len(), 1);
    assert_eq!(last_undo(&h).label, "Add to region");
}

const GEOJSON: &str = r#"{"type": "FeatureCollection", "features": [
    {"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [[[10,10],[110,10],[110,110],[10,110],[10,10]]]},
     "properties": {"objectType": "annotation", "classification": {"name": "Tumor", "color": [255, 0, 0]}}},
    {"type": "Feature", "geometry": {"type": "MultiPoint", "coordinates": [[20, 20], [30, 30]]},
     "properties": {"classification": {"name": "Lymphocyte"}}},
    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [-50, 20]}, "properties": {}}
]}"#;

#[test]
fn geojson_imports_are_validated_confirmed_atomic_and_undoable() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    let path = dir.join("cells.geojson");
    std::fs::write(&path, GEOJSON).unwrap();
    let sidebar = open_sidebar(&h);
    callback(&h, &sidebar, "menu", vec![json!("import")]);
    answer_dialog(&h, &path);
    let (modal_id, props) = modal(&h).expect("the import dialog");
    assert!(
        props["summary"]
            .as_str()
            .unwrap()
            .contains("3 annotation(s)"),
        "{props:?}"
    );
    assert!(
        props["notes"]
            .as_str()
            .unwrap()
            .contains("outside the slide")
    );
    assert!(
        objects(&h, 1).is_empty(),
        "nothing is written before confirmation"
    );
    let undo_before = h.state().undo.len();
    h.ui_callback(&modal_id, "import-confirm", vec![json!(0)])
        .unwrap();
    settle(&h);
    assert_eq!(objects(&h, 1).len(), 3);
    assert_eq!(h.state().undo.len(), undo_before + 1, "one undo step");
    let classes = prop(&h, &sidebar, "class-names");
    assert!(classes.as_array().unwrap().iter().any(|c| c == "Tumor"));
    undo(&h);
    assert!(objects(&h, 1).is_empty());
}

#[test]
fn imports_are_bound_to_the_slide_they_started_on() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    open(&h, info(2, "b", &dir));
    activate(&h, 1);
    let path = dir.join("cells.geojson");
    std::fs::write(&path, GEOJSON).unwrap();
    let sidebar = open_sidebar(&h);
    callback(&h, &sidebar, "menu", vec![json!("import")]);
    // The user switches tabs before answering the dialog (ANN-9).
    activate(&h, 2);
    answer_dialog(&h, &path);
    let (modal_id, _) = modal(&h).expect("the import dialog");
    h.ui_callback(&modal_id, "import-confirm", vec![json!(0)])
        .unwrap();
    settle(&h);
    assert_eq!(objects(&h, 1).len(), 3);
    assert!(objects(&h, 2).is_empty());

    // Closing the slide before confirming cancels the import.
    callback(&h, &sidebar, "menu", vec![json!("import")]);
    answer_dialog(&h, &path);
    let (modal_id, _) = modal(&h).expect("the import dialog");
    activate(&h, 1);
    h.send(HostEvent::FileClosed { file_id: 2 }).unwrap();
    settle(&h);
    assert!(!h.state().surfaces[&modal_id].open);
}

#[test]
fn malformed_imports_are_reported_and_change_nothing() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    let sidebar = open_sidebar(&h);
    for (name, content) in [
        (
            "bad.json",
            r###"{"annotation_layers": [{"name": "x", "color_hex": "##aébcd", "annotations": [{"type": "polygon", "vertices": []}]}]}"###,
        ),
        ("junk.xml", "<Annotations><Annotation>"),
        ("binary.geojson", "\u{0}\u{1}\u{2}"),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        callback(&h, &sidebar, "menu", vec![json!("import")]);
        answer_dialog(&h, &path);
        assert!(modal(&h).is_none(), "{name}");
    }
    assert!(objects(&h, 1).is_empty());
    let errors = notifications(&h)
        .into_iter()
        .filter(|n| n.contains("Could not import"))
        .count();
    assert_eq!(errors, 3, "{:?}", notifications(&h));
}

#[test]
fn exports_are_written_atomically_without_paths() {
    let (h, dir) = fresh();
    let file = info(1, "a", &dir);
    let full_path = file.path.clone();
    open(&h, file);
    draw(&h, POLYGON, 1, square(10.0, 10.0, 100.0));
    draw(&h, POINT, 1, Shape::Point { x: 5.0, y: 5.0 });
    let sidebar = open_sidebar(&h);
    for (item, name) in [
        ("export-geojson", "out.geojson"),
        ("export-eov", "out.json"),
        ("export-csv", "out.csv"),
        ("export-asap", "out.asap.xml"),
        ("export-imagescope", "out.imagescope.xml"),
    ] {
        let target = dir.join(name);
        callback(&h, &sidebar, "menu", vec![json!(item)]);
        answer_dialog(&h, &target);
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(!text.contains(&full_path), "{item} leaks the slide path");
        assert!(
            !text.contains("a.svs"),
            "{item} leaks the file name by default"
        );
    }
    let geojson: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("out.geojson")).unwrap()).unwrap();
    assert_eq!(geojson["features"].as_array().unwrap().len(), 2);
    let eov: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("out.json")).unwrap()).unwrap();
    assert_eq!(eov["format_version"], 2);
    assert_eq!(eov["slide"]["identity"], "eov-slide-1:a");
    let csv = std::fs::read_to_string(dir.join("out.csv")).unwrap();
    assert_eq!(csv.lines().count(), 3);
    // 100 px at 0.25 µm/px: 625 µm².
    assert!(csv.contains("625.0000"), "{csv}");
    assert!(notified(&h, "Exported 2 annotation(s)"));
    // Re-importing our own export recognises the slide.
    callback(&h, &sidebar, "menu", vec![json!("import")]);
    answer_dialog(&h, &dir.join("out.json"));
    let (modal_id, props) = modal(&h).unwrap();
    assert_eq!(props["match-text"], "Made for this slide.");
    assert_eq!(props["mismatch"], json!(false));
    h.ui_callback(&modal_id, "import-cancel", vec![]).unwrap();
    settle(&h);
    // The XML exports import as the same two annotations.
    for name in ["out.asap.xml", "out.imagescope.xml"] {
        callback(&h, &sidebar, "menu", vec![json!("import")]);
        answer_dialog(&h, &dir.join(name));
        let (modal_id, props) = modal(&h).unwrap();
        assert!(
            props["summary"]
                .as_str()
                .unwrap()
                .contains("2 annotation(s)"),
            "{name}: {props:?}"
        );
        h.ui_callback(&modal_id, "import-cancel", vec![]).unwrap();
        settle(&h);
    }
}

#[cfg(unix)]
#[test]
fn read_only_databases_are_viewable_but_not_editable() {
    use std::os::unix::fs::PermissionsExt;
    let mut init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let db_path = dir.join("teaching.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(crate::db::schema::SCHEMA_V2).unwrap();
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO slides (id, identity, created_at, last_opened_at) VALUES (1, 'eov-slide-1:a', 1, 1);
             INSERT INTO layers (id, slide_id, name, color, created_at, updated_at) VALUES ('L', 1, 'Teaching', '#FF0000', 1, 1);
             INSERT INTO annotations (id, layer_id, kind, geometry, min_x, min_y, max_x, max_y, created_at, updated_at)
             VALUES ('p', 'L', 'point', '{\"kind\":\"point\",\"x\":5.0,\"y\":5.0}', 5, 5, 5, 5, 1, 1);",
        )
        .unwrap();
    }
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    init.settings
        .insert(DATABASE_PATH_KEY.into(), json!(db_path.to_string_lossy()));
    let h = start(init);
    open(&h, info(1, "a", &dir));
    assert_eq!(
        objects(&h, 1).len(),
        1,
        "a read-only database can be viewed"
    );
    let sidebar = open_sidebar(&h);
    assert!(
        prop(&h, &sidebar, "banner")
            .as_str()
            .unwrap()
            .contains("read-only")
    );
    assert_eq!(prop(&h, &sidebar, "editable"), json!(false));
    draw(&h, POINT, 1, Shape::Point { x: 50.0, y: 50.0 });
    assert_eq!(objects(&h, 1).len(), 1);
    assert!(notified(&h, "read-only"));
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

const LEGACY_DDL: &str = r#"
CREATE TABLE annotation_layers (id TEXT PRIMARY KEY, fingerprint BLOB NOT NULL, name TEXT NOT NULL,
    notes TEXT, color TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE annotations (id TEXT PRIMARY KEY, annotation_layer_id TEXT NOT NULL, type TEXT NOT NULL,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE annotation_points (annotation_id TEXT PRIMARY KEY, x_level0 REAL NOT NULL, y_level0 REAL NOT NULL);
CREATE TABLE annotation_polygons (annotation_id TEXT PRIMARY KEY);
CREATE TABLE annotation_polygon_vertices (annotation_id TEXT NOT NULL, vertex_index INTEGER NOT NULL,
    x_level0 REAL NOT NULL, y_level0 REAL NOT NULL, PRIMARY KEY (annotation_id, vertex_index));
CREATE TABLE annotation_metadata (annotation_id TEXT NOT NULL, row_index INTEGER NOT NULL,
    key TEXT NOT NULL DEFAULT '', value TEXT NOT NULL DEFAULT '', PRIMARY KEY (annotation_id, row_index));
"#;

#[test]
fn pre_1_0_databases_are_migrated_and_found_by_fingerprint() {
    let init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let file = info(1, "old", &dir);
    let fingerprint = fileio::legacy_fingerprint(Path::new(&file.path)).unwrap();
    {
        let conn = rusqlite::Connection::open(PathBuf::from(&init.data_dir).join("annotations.db"))
            .unwrap();
        conn.execute_batch(LEGACY_DDL).unwrap();
        conn.execute(
            "INSERT INTO annotation_layers VALUES ('L1', ?1, 'Mitoses', NULL, '#FF0000', 100, 200)",
            [fingerprint.as_slice()],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO annotations VALUES ('P1', 'L1', 'point', 10, 11);
             INSERT INTO annotation_points VALUES ('P1', 5.5, 6.5);",
        )
        .unwrap();
    }
    let h = start(init.clone());
    open(&h, file.clone());
    let shown = objects(&h, 1);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].1.shape, Shape::Point { x: 5.5, y: 6.5 });
    assert!(notified(&h, "Upgraded the annotation database"));
    // The identity was attached: the slide is now found without the
    // fingerprint, even after the file changed.
    drop(h);
    std::fs::write(&file.path, b"re-saved").unwrap();
    let h = start(init);
    open(&h, file);
    assert_eq!(objects(&h, 1).len(), 1);
}

#[test]
fn classes_hotkeys_and_counting() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    let sidebar = open_sidebar(&h);
    callback(&h, &sidebar, "class-new", vec![]);
    let classes = prop(&h, &sidebar, "classes");
    let new = classes.as_array().unwrap().last().unwrap().clone();
    let id = new["id"].as_str().unwrap().to_string();
    callback(
        &h,
        &sidebar,
        "class-rename",
        vec![json!(id), json!("Blasts")],
    );
    callback(&h, &sidebar, "class-hotkey", vec![json!(id), json!("Q")]);
    let hotkey = format!("class-{id}");
    {
        let state = h.state();
        let spec = state.hotkeys.get(&hotkey).expect("a class hotkey");
        assert_eq!(spec.hotkey.key, "q");
        assert_eq!(spec.description, "Annotation class: Blasts");
        assert!(
            state
                .toolbar_buttons
                .keys()
                .all(|b| b == tools::SIDEBAR_BUTTON)
        );
    }
    // A key eov uses is refused here already...
    callback(&h, &sidebar, "class-hotkey", vec![json!(id), json!("f")]);
    assert!(notified(&h, "eov already uses"));
    // ...and one eov refuses is shown with the class.
    h.send(HostEvent::RegistrationRefused {
        what: "hotkey".into(),
        id: hotkey.clone(),
        reason: "another plugin uses Q".into(),
    })
    .unwrap();
    settle(&h);
    let row = prop(&h, &sidebar, "classes")
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(id))
        .unwrap()
        .clone();
    assert!(
        row["hotkey-note"]
            .as_str()
            .unwrap()
            .contains("another plugin uses Q")
    );
    // Choosing another key clears it and registers that one.
    callback(&h, &sidebar, "class-hotkey", vec![json!(id), json!("k")]);
    assert_eq!(h.state().hotkeys[&hotkey].hotkey.key, "k");
    let row = prop(&h, &sidebar, "classes")
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(row["hotkey-note"], "");
    // The hotkey makes the class active.
    h.send(HostEvent::HotkeyPressed {
        hotkey_id: hotkey.clone(),
        pane: Some(1),
    })
    .unwrap();
    settle(&h);
    assert_eq!(
        h.state().settings.get(SETTINGS_KEY).unwrap()["active_class"],
        json!(id)
    );
    draw(&h, COUNT, 1, Shape::Point { x: 10.0, y: 10.0 });
    let counted = objects(&h, 1);
    assert!(
        counted[0]
            .1
            .tooltip
            .as_deref()
            .unwrap()
            .starts_with("Blasts")
    );
    // Hiding the class hides its annotations; showing brings them back.
    callback(&h, &sidebar, "class-visible", vec![json!(id), json!(false)]);
    assert!(objects(&h, 1).is_empty());
    callback(&h, &sidebar, "class-visible", vec![json!(id), json!(true)]);
    assert_eq!(objects(&h, 1).len(), 1);
    // Deleting the class (after confirmation) is undoable.
    callback(&h, &sidebar, "class-delete", vec![json!(id)]);
    confirm(&h);
    assert!(!h.state().hotkeys.contains_key(&hotkey));
    undo(&h);
    assert!(h.state().hotkeys.contains_key(&hotkey));
}

fn pointer(h: &H, kind: PointerKind, image: [f64; 2], screen: [f64; 2]) {
    h.send(HostEvent::Pointer {
        tool_id: FIELD.into(),
        pane: 1,
        file_id: 1,
        event: PointerEvent {
            kind,
            button: PointerButton::Left,
            image_x: image[0],
            image_y: image[1],
            screen_x: screen[0],
            screen_y: screen[1],
            modifiers: Modifiers::default(),
            wheel_delta: 0.0,
        },
    })
    .unwrap();
    settle(h);
}

#[test]
fn hpf_fields_have_the_configured_size_and_the_reticle_follows_the_pointer() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    assert_eq!(h.state().tools[FIELD].kind, ToolKind::Custom);
    h.send(HostEvent::ToolActivated {
        tool_id: FIELD.into(),
    })
    .unwrap();
    settle(&h);
    // No reticle until the pointer is over the slide.
    assert!(!h.state().layers.contains_key("hpf.1"));
    pointer(&h, PointerKind::Move, [5000.0, 4000.0], [100.0, 100.0]);
    let reticle = h.state().objects("hpf.1");
    assert_eq!(reticle.len(), 1);
    // 0.2376 mm² → radius 275 µm → 1100 px at 0.25 µm/px.
    let Shape::Ellipse { cx, rx, .. } = reticle[0].shape else {
        panic!()
    };
    assert_eq!(cx, 5000.0);
    assert!((rx - 1100.0).abs() < 1.0, "{rx}");
    pointer(&h, PointerKind::Move, [6000.0, 4000.0], [140.0, 100.0]);
    let Shape::Ellipse { cx, .. } = h.state().objects("hpf.1")[0].shape else {
        panic!()
    };
    assert_eq!(cx, 6000.0, "the reticle follows the pointer");
    // A click places a field where it was pressed.
    pointer(&h, PointerKind::Down, [3000.0, 3000.0], [50.0, 50.0]);
    pointer(&h, PointerKind::Up, [3000.0, 3000.0], [50.0, 50.0]);
    let fields = objects(&h, 1);
    assert_eq!(fields.len(), 1);
    let Shape::Ellipse { cx, rx, .. } = fields[0].1.shape else {
        panic!("{:?}", fields[0].1.shape)
    };
    assert_eq!(cx, 3000.0);
    assert!((rx - 1100.0).abs() < 1.0);
    assert_eq!(fields[0].1.label.as_deref(), Some("HPF 1"));
    // A double click places one field, not two.
    pointer(&h, PointerKind::Down, [8000.0, 3000.0], [60.0, 50.0]);
    pointer(&h, PointerKind::Up, [8000.0, 3000.0], [60.0, 50.0]);
    pointer(&h, PointerKind::Down, [8000.0, 3000.0], [60.0, 50.0]);
    pointer(&h, PointerKind::DoubleClick, [8000.0, 3000.0], [60.0, 50.0]);
    pointer(&h, PointerKind::Up, [8000.0, 3000.0], [60.0, 50.0]);
    assert_eq!(objects(&h, 1).len(), 2);
    // A drag pans the view (the slide follows the pointer) and places nothing.
    pointer(&h, PointerKind::Down, [1000.0, 1000.0], [200.0, 200.0]);
    pointer(&h, PointerKind::Move, [1100.0, 1000.0], [230.0, 200.0]);
    pointer(&h, PointerKind::Up, [1100.0, 1000.0], [230.0, 200.0]);
    assert_eq!(objects(&h, 1).len(), 2);
    let pans = h.state().calls_to("view.pan");
    assert_eq!(pans.last().unwrap()["dx"], json!(-30.0));
    // The reticle leaves with the pointer.
    pointer(&h, PointerKind::Leave, [0.0, 0.0], [0.0, 0.0]);
    assert!(!h.state().layers.contains_key("hpf.1"));
    // Fields can be moved but not resized.
    let fields = objects(&h, 1);
    let (layer, object) = fields
        .iter()
        .find(|(_, o)| matches!(o.shape, Shape::Ellipse { cx, .. } if cx == 3000.0))
        .unwrap()
        .clone();
    h.send(HostEvent::ObjectEdited {
        object: reference(&layer, &object),
        shape: Shape::Ellipse {
            cx: 3500.0,
            cy: 3000.0,
            rx: 50.0,
            ry: 50.0,
        },
    })
    .unwrap();
    settle(&h);
    let moved = h.state().layers[&layer].objects[&object.object_id].clone();
    let Shape::Ellipse { cx, rx, .. } = moved.shape else {
        panic!()
    };
    assert_eq!(cx, 3500.0);
    assert!((rx - 1100.0).abs() < 1.0);
    // The tool claims no keys: eov's shortcuts, the other tools' keys,
    // class hotkeys and Escape keep working while it is active, and a stray
    // key event does nothing.
    assert_eq!(h.state().tools[FIELD].keys, Some(Vec::new()));
    let calls = h.state().calls.len();
    h.send(HostEvent::Key {
        tool_id: Some(FIELD.into()),
        pane: Some(1),
        key: "p".into(),
        modifiers: Modifiers::default(),
        pressed: true,
        repeat: false,
    })
    .unwrap();
    settle(&h);
    assert!(
        h.state().calls[calls..]
            .iter()
            .all(|c| !c.method.starts_with("tool.") && !c.method.starts_with("view.")),
        "no emulated keys"
    );
    h.send(HostEvent::ToolDeactivated {
        tool_id: FIELD.into(),
    })
    .unwrap();
    settle(&h);
    pointer(&h, PointerKind::Move, [5000.0, 4000.0], [100.0, 100.0]);
    assert!(
        !h.state().layers.contains_key("hpf.1"),
        "no reticle without the tool"
    );
}

#[test]
fn orphaned_annotations_can_be_attached_to_the_open_slide() {
    let init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let h = start(init.clone());
    open(&h, info(1, "before-deidentification", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    drop(h);
    // The same slide, re-saved: a different identity.
    let h = start(init);
    open(&h, info(1, "after-deidentification", &dir));
    assert!(objects(&h, 1).is_empty());
    let sidebar = open_sidebar(&h);
    callback(&h, &sidebar, "tab-changed", vec![json!(sidebar::TAB_DATA)]);
    let orphans = prop(&h, &sidebar, "orphans");
    let orphan = orphans
        .as_array()
        .unwrap()
        .first()
        .expect("an orphan")
        .clone();
    assert!(
        orphan["title"]
            .as_str()
            .unwrap()
            .contains("before-deidentification")
    );
    callback(&h, &sidebar, "orphan-relink", vec![orphan["id"].clone()]);
    confirm(&h);
    assert_eq!(objects(&h, 1).len(), 1);
    undo(&h);
    assert!(objects(&h, 1).is_empty());
}

#[test]
fn changes_from_another_eov_window_are_picked_up() {
    let mut init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    init.settings.insert(
        DATABASE_PATH_KEY.into(),
        json!(dir.join("shared.db").to_string_lossy()),
    );
    let mut other_init = test_init("annotations");
    other_init.settings = init.settings.clone();
    let a = start(init);
    let b = start(other_init);
    open(&a, info(1, "a", &dir));
    open(&b, info(7, "a", &dir));
    draw(&a, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    assert!(objects(&b, 7).is_empty());
    b.send(HostEvent::WindowFocusChanged { focused: true })
        .unwrap();
    settle(&b);
    assert_eq!(objects(&b, 7).len(), 1);
    assert!(notified(&b, "another eov window"));
}

#[test]
fn layers_can_be_created_renamed_hidden_and_deleted_with_undo() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    let sidebar = open_sidebar(&h);
    callback(&h, &sidebar, "layer-new", vec![]);
    let rows = prop(&h, &sidebar, "rows");
    let layers: Vec<Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == 0)
        .cloned()
        .collect();
    assert_eq!(layers.len(), 2);
    let new_layer = layers
        .iter()
        .find(|l| l["current"] == json!(true))
        .expect("the new layer is current")
        .clone();
    let id = new_layer["id"].as_str().unwrap().to_string();
    callback(
        &h,
        &sidebar,
        "layer-rename",
        vec![json!(id), json!("  Reader 2 ")],
    );
    let host_layer = overlay::host_layer_id(1, &id);
    assert_eq!(h.state().layers[&host_layer].spec.name, "Reader 2");
    // Too long a name is refused with a message.
    let error = h
        .ui_callback(
            &sidebar,
            "layer-rename",
            vec![json!(id), json!("x".repeat(500))],
        )
        .err();
    settle(&h);
    assert!(error.is_some() || notified(&h, "limit"));
    // Visibility is a single layer update.
    callback(&h, &sidebar, "layer-visible", vec![json!(id), json!(false)]);
    assert!(!h.state().layers[&host_layer].spec.visible);
    // New annotations go to the current layer.
    draw(&h, POINT, 1, Shape::Point { x: 20.0, y: 20.0 });
    assert_eq!(h.state().layers[&host_layer].objects.len(), 1);
    // Delete after confirmation; undo restores the layer and its contents.
    callback(&h, &sidebar, "layer-delete", vec![json!(id)]);
    confirm(&h);
    assert!(!h.state().layers.contains_key(&host_layer));
    undo(&h);
    assert_eq!(h.state().layers[&host_layer].objects.len(), 1);
}

#[test]
fn text_annotations_show_their_name() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, TEXT, 1, Shape::Point { x: 100.0, y: 100.0 });
    let (_, object) = objects(&h, 1).remove(0);
    assert!(matches!(&object.shape, Shape::Text { text, .. } if text == "Text"));
    let sidebar = open_sidebar(&h);
    callback(
        &h,
        &sidebar,
        "detail-name-committed",
        vec![json!(object.object_id), json!("Margin involved")],
    );
    let (_, object) = objects(&h, 1).remove(0);
    assert!(matches!(&object.shape, Shape::Text { text, .. } if text == "Margin involved"));
    assert_eq!(last_undo(&h).label, "Rename annotation");
}

#[test]
fn the_hud_button_hides_a_slides_annotations_in_its_pane() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    let click = |h: &H| {
        h.send(HostEvent::HudButtonClicked {
            button_id: tools::VISIBILITY_HUD.into(),
            item_id: None,
            pane: 2,
            view: Some(ViewInfo {
                pane: 2,
                file_id: 1,
                ..Default::default()
            }),
        })
        .unwrap();
        settle(h);
    };
    click(&h);
    let (layer, _) = objects(&h, 1).remove(0);
    {
        let state = h.state();
        assert!(
            state.layers[&layer].spec.visible,
            "still shown in other panes"
        );
        assert!(state.layers[&layer].hidden_panes.contains(&2));
        let updates = state.calls_to("hud.update");
        assert!(
            updates
                .iter()
                .any(|u| u["active"] == json!(true) && u["pane"] == json!(2))
        );
    }
    // Layers created later are hidden in that pane too.
    draw(&h, COUNT, 1, Shape::Point { x: 20.0, y: 20.0 });
    {
        let state = h.state();
        let counts = state
            .layers
            .iter()
            .find(|(id, l)| id.starts_with("ann.1.") && **id != layer && !l.objects.is_empty())
            .expect("the counts layer");
        assert!(counts.1.hidden_panes.contains(&2));
    }
    click(&h);
    assert!(h.state().layers[&layer].hidden_panes.is_empty());
}

#[test]
fn settings_and_unknown_input_are_handled_safely() {
    let mut init = test_init("annotations");
    init.settings.insert(
        SETTINGS_KEY.into(),
        json!({"point_radius": -3, "fill_opacity": "x", "field": {"mode": "area", "mm2": -1}}),
    );
    let h = start(init);
    settle(&h);
    // Unknown callbacks and events are ignored.
    assert_eq!(
        h.ui_callback("s", "no-such-callback", vec![]).unwrap(),
        Value::Null
    );
    h.send(HostEvent::Posted {
        token: "import:999".into(),
    })
    .unwrap();
    h.send(HostEvent::ObjectEdited {
        object: ObjectRef {
            layer_id: "not ours".into(),
            object_id: "x".into(),
        },
        shape: Shape::Point { x: 0.0, y: 0.0 },
    })
    .unwrap();
    settle(&h);
}

#[test]
fn a_new_database_says_where_older_annotations_are() {
    let init = test_init("annotations");
    let h = start(init.clone());
    settle(&h);
    let sidebar = open_sidebar(&h);
    let notes = prop(&h, &sidebar, "db-notes");
    assert!(notes.as_str().unwrap().contains("before 1.0"), "{notes}");
    drop(h);
    // The second start finds the database it created: no hint.
    let h = start(init);
    settle(&h);
    let sidebar = open_sidebar(&h);
    assert_eq!(prop(&h, &sidebar, "db-notes"), "");
}

fn legacy_database(dir: &Path, file: &FileInfo) -> PathBuf {
    let legacy = dir.join("legacy-config");
    std::fs::create_dir_all(&legacy).unwrap();
    let fingerprint = fileio::legacy_fingerprint(Path::new(&file.path)).unwrap();
    let conn = rusqlite::Connection::open(legacy.join("annotations.db")).unwrap();
    conn.execute_batch(LEGACY_DDL).unwrap();
    conn.execute(
        "INSERT INTO annotation_layers VALUES ('L1', ?1, 'Mitoses', NULL, '#FF0000', 100, 200)",
        [fingerprint.as_slice()],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO annotations VALUES ('P1', 'L1', 'point', 10, 11);
         INSERT INTO annotation_points VALUES ('P1', 5.5, 6.5);",
    )
    .unwrap();
    legacy
}

#[test]
fn the_database_from_before_1_0_is_offered_once_from_the_legacy_folder() {
    let mut init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let file = info(1, "old", &dir);
    let legacy = legacy_database(&dir, &file);
    init.legacy_config_dir = Some(legacy.to_string_lossy().into_owned());
    let h = start(init.clone());
    settle(&h);
    // Asked at start; nothing is merged before the answer.
    assert!(
        !h.ui_callback("", "__pending", vec![])
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    confirm(&h);
    assert!(notified(&h, "Merged 1 annotation(s)"));
    open(&h, file.clone());
    assert_eq!(objects(&h, 1).len(), 1);
    let sidebar = open_sidebar(&h);
    assert_eq!(prop(&h, &sidebar, "legacy-db"), "");
    // The old file is untouched, and it is not offered again.
    let conn = rusqlite::Connection::open(legacy.join("annotations.db")).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    drop(h);
    let h = start(init);
    settle(&h);
    assert!(
        h.ui_callback("", "__pending", vec![])
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_declined_legacy_merge_stays_available_in_the_data_tab() {
    let mut init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let file = info(1, "old", &dir);
    let legacy = legacy_database(&dir, &file);
    init.legacy_config_dir = Some(legacy.to_string_lossy().into_owned());
    let h = start(init.clone());
    settle(&h);
    let request_id = pending_request(&h);
    h.send(HostEvent::ConfirmResult {
        request_id,
        confirmed: false,
    })
    .unwrap();
    settle(&h);
    drop(h);
    // Not asked again, but offered in the Data tab.
    let h = start(init);
    settle(&h);
    assert!(
        h.ui_callback("", "__pending", vec![])
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let sidebar = open_sidebar(&h);
    let offered = prop(&h, &sidebar, "legacy-db");
    assert!(
        offered.as_str().unwrap().ends_with("annotations.db"),
        "{offered}"
    );
    callback(&h, &sidebar, "legacy-merge", vec![]);
    open(&h, file);
    assert_eq!(objects(&h, 1).len(), 1);
    assert_eq!(prop(&h, &sidebar, "legacy-db"), "");
}

#[test]
fn undo_records_the_host_dropped_are_freed() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(&h, POINT, 1, Shape::Point { x: 10.0, y: 10.0 });
    draw(&h, POINT, 1, Shape::Point { x: 20.0, y: 20.0 });
    let kept = |h: &H| h.ui_callback("", "__history", vec![]).unwrap();
    assert_eq!(kept(&h), json!(2));
    let first = h.state().undo[0].entry_id.clone();
    h.send(HostEvent::UndoDropped {
        entry_ids: vec![first],
    })
    .unwrap();
    settle(&h);
    assert_eq!(kept(&h), json!(1));
    // The remaining entry still undoes.
    undo(&h);
    assert_eq!(objects(&h, 1).len(), 1);
}

#[test]
fn arrows_are_lines_with_a_head_that_survive_edits_and_formats() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    draw(
        &h,
        ARROW,
        1,
        Shape::Polyline {
            points: vec![[100.0, 100.0], [500.0, 100.0]],
        },
    );
    let (layer, object) = objects(&h, 1).remove(0);
    // One object: the shaft, then the head as two barbs at the tip.
    let Shape::Polyline { points } = &object.shape else {
        panic!("{:?}", object.shape)
    };
    assert_eq!(points.len(), 2 + crate::model::ARROWHEAD_VERTICES);
    assert_eq!(points[3], [500.0, 100.0]);
    assert!(
        points[2][0] < 500.0 && points[4][0] < 500.0,
        "barbs point back"
    );
    let sidebar = open_sidebar(&h);
    assert_eq!(prop(&h, &sidebar, "detail-kind"), "arrow");
    let measurements = prop(&h, &sidebar, "detail-measurements");
    let length = measurements
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == "Length")
        .expect("an arrow has a length")
        .clone();
    // 400 px at 0.25 µm/px.
    assert_eq!(length["value"], "100.0 µm");
    // Moving the whole drawing moves the arrow; its head is redrawn.
    let moved: Vec<[f64; 2]> = points.iter().map(|p| [p[0], p[1] + 50.0]).collect();
    h.send(HostEvent::ObjectEdited {
        object: reference(&layer, &object),
        shape: Shape::Polyline { points: moved },
    })
    .unwrap();
    settle(&h);
    let Shape::Polyline { points } = h.state().layers[&layer].objects[&object.object_id]
        .shape
        .clone()
    else {
        panic!()
    };
    assert_eq!(&points[..2], &[[100.0, 150.0], [500.0, 150.0]]);
    // Dragging a barb changes nothing and redraws the head.
    let mut barb = points.clone();
    barb[2] = [0.0, 0.0];
    h.send(HostEvent::ObjectEdited {
        object: reference(&layer, &object),
        shape: Shape::Polyline { points: barb },
    })
    .unwrap();
    settle(&h);
    assert_eq!(
        h.state().layers[&layer].objects[&object.object_id].shape,
        Shape::Polyline {
            points: points.clone()
        }
    );
    // GeoJSON carries it as a LineString marked as an arrow; it comes back.
    let path = dir.join("arrow.geojson");
    callback(&h, &sidebar, "menu", vec![json!("export-geojson")]);
    answer_dialog(&h, &path);
    let exported: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let feature = &exported["features"][0];
    assert_eq!(feature["geometry"]["type"], "LineString");
    assert_eq!(feature["properties"]["eov"]["kind"], "arrow");
    open(&h, info(2, "b", &dir));
    callback(&h, &sidebar, "menu", vec![json!("import")]);
    answer_dialog(&h, &path);
    let (modal_id, _) = modal(&h).expect("the import dialog");
    h.ui_callback(&modal_id, "import-confirm", vec![json!(0)])
        .unwrap();
    settle(&h);
    let imported = objects(&h, 2);
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].1.shape, Shape::Polyline { points });
}

fn brush(h: &H, kind: PointerKind, image: [f64; 2], alt: bool) {
    h.send(HostEvent::Pointer {
        tool_id: BRUSH.into(),
        pane: 1,
        file_id: 1,
        event: PointerEvent {
            kind,
            button: PointerButton::Left,
            image_x: image[0],
            image_y: image[1],
            screen_x: image[0] / 2.0,
            screen_y: image[1] / 2.0,
            modifiers: Modifiers {
                alt,
                ..Default::default()
            },
            wheel_delta: 0.0,
        },
    })
    .unwrap();
    settle(h);
}

fn stroke(h: &H, from: [f64; 2], to: [f64; 2], alt: bool) {
    brush(h, PointerKind::Down, from, alt);
    for i in 1..=10 {
        let t = i as f64 / 10.0;
        brush(
            h,
            PointerKind::Move,
            [
                from[0] + (to[0] - from[0]) * t,
                from[1] + (to[1] - from[1]) * t,
            ],
            alt,
        );
    }
    brush(h, PointerKind::Up, to, alt);
}

fn area_of(h: &H, file: FileId) -> f64 {
    objects(h, file)
        .iter()
        .map(|(_, o)| geometry::area(&Geometry::from_shape(&o.shape).0))
        .sum()
}

#[test]
fn the_brush_paints_extends_and_erases_regions_one_undo_step_per_stroke() {
    let (h, dir) = fresh();
    open(&h, info(1, "a", &dir));
    // The pane shows level-0 at half size: 24 screen px are 48 level-0 px.
    h.state().snapshot.panes = vec![PaneInfo {
        pane: 1,
        index: 0,
        file_id: Some(1),
        view: Some(ViewInfo {
            pane: 1,
            file_id: 1,
            zoom: 0.5,
            width: 800.0,
            height: 600.0,
            ..Default::default()
        }),
    }];
    assert_eq!(
        h.state().tools[BRUSH].keys,
        Some(vec!["[".to_string(), "]".to_string()])
    );
    h.send(HostEvent::ToolActivated {
        tool_id: BRUSH.into(),
    })
    .unwrap();
    settle(&h);
    // Hovering shows the brush outline.
    brush(&h, PointerKind::Move, [500.0, 500.0], false);
    assert_eq!(h.state().objects("brush.1").len(), 1);
    let undo_before = h.state().undo.len();
    stroke(&h, [1000.0, 1000.0], [2000.0, 1000.0], false);
    assert_eq!(objects(&h, 1).len(), 1, "one new region");
    assert_eq!(h.state().undo.len(), undo_before + 1, "one undo step");
    // A stadium of 1000 × 48 plus a disc of radius 24.
    let painted = area_of(&h, 1);
    let expected = 1000.0 * 48.0 + std::f64::consts::PI * 24.0 * 24.0;
    assert!(
        (painted - expected).abs() / expected < 0.01,
        "{painted} vs {expected}"
    );
    // Painting over the selected region extends it.
    stroke(&h, [2000.0, 1000.0], [2000.0, 1500.0], false);
    assert_eq!(objects(&h, 1).len(), 1, "still one region");
    assert!(area_of(&h, 1) > painted * 1.4);
    // Alt erases: a cut across the first stroke splits the region.
    let before = area_of(&h, 1);
    stroke(&h, [1500.0, 900.0], [1500.0, 1100.0], true);
    assert_eq!(objects(&h, 1).len(), 2, "the cut split the region");
    assert!(area_of(&h, 1) < before);
    assert_eq!(last_undo(&h).label, "Erase");
    undo(&h);
    assert_eq!(objects(&h, 1).len(), 1);
    assert!((area_of(&h, 1) - before).abs() < 1.0);
    // [ and ] change the size.
    h.send(HostEvent::Key {
        tool_id: Some(BRUSH.into()),
        pane: Some(1),
        key: "]".into(),
        modifiers: Modifiers::default(),
        pressed: true,
        repeat: false,
    })
    .unwrap();
    settle(&h);
    assert_eq!(
        h.state().settings.get(SETTINGS_KEY).unwrap()["brush_size"],
        json!(30.0)
    );
    // Erasing with nothing selected says what to do.
    h.send(HostEvent::SelectionChanged { objects: vec![] })
        .unwrap();
    settle(&h);
    stroke(&h, [100.0, 100.0], [200.0, 100.0], true);
    assert!(notified(&h, "Select the region to erase from"));
    h.send(HostEvent::ToolDeactivated {
        tool_id: BRUSH.into(),
    })
    .unwrap();
    settle(&h);
    assert!(!h.state().layers.contains_key("brush.1"));
}
