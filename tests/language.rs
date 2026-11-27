//! The plugin follows the language of eov's interface: what it hands eov
//! (tool names, button tooltips, the status line, the panel's texts) is
//! handed over again in the new language, while what it writes to the
//! database stays English.
//!
//! A test binary of its own: the language is global to a library, so it
//! must not change under the other tests (and its tests run one at a time).

use annotations::{AnnotationsPlugin, DATABASE_PATH_KEY};
use eov_plugin_api::i18n;
use eov_plugin_api::prelude::*;
use eov_plugin_api::testing::{MockHost, TestHarness, test_init};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

type H = TestHarness<AnnotationsPlugin>;

/// One test at a time: they change the language of the library.
fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A few texts of the plugin in German, as a catalog would have them.
const GERMAN: &str = r#"msgid ""
msgstr ""
"Language: de\n"
"Content-Type: text/plain; charset=UTF-8\n"
"Plural-Forms: nplurals=2; plural=(n != 1);\n"

msgid "Select and edit annotations"
msgstr "Anmerkungen auswählen und bearbeiten"

msgid "Annotations"
msgstr "Anmerkungen"

msgid "Show or hide annotations on this slide"
msgstr "Anmerkungen auf diesem Objektträger zeigen oder verbergen"

msgid "Counting {}"
msgstr "Zählung: {}"

msgid "No slide open"
msgstr "Kein Objektträger geöffnet"

msgid "Add point"
msgstr "Punkt hinzufügen"

msgid "Could not load this slide's annotations: {}"
msgstr "Die Anmerkungen dieses Objektträgers konnten nicht geladen werden: {}"

msgid "Annotation class: {}"
msgstr "Anmerkungsklasse: {}"

msgid "eov refused the hotkey: {}"
msgstr "eov hat das Tastenkürzel abgelehnt: {}"

msgid "in {n} layer"
msgid_plural "in {n} layers"
msgstr[0] "in {n} Ebene"
msgstr[1] "in {n} Ebenen"

msgid "{n} slide"
msgid_plural "{n} slides"
msgstr[0] "{n} Präparat"
msgstr[1] "{n} Präparate"

msgid "{n} annotation"
msgid_plural "{n} annotations"
msgstr[0] "{n} Anmerkung"
msgstr[1] "{n} Anmerkungen"

msgid "1 HPF · {area} mm² · {n} point"
msgid_plural "1 HPF · {area} mm² · {n} points"
msgstr[0] "1 HPF · {area} mm² · {n} Punkt"
msgstr[1] "1 HPF · {area} mm² · {n} Punkte"

msgid "Upgraded the annotation database from the pre-1.0 format: {slides}, {n} annotation."
msgid_plural "Upgraded the annotation database from the pre-1.0 format: {slides}, {n} annotations."
msgstr[0] "Anmerkungsdatenbank aus dem Format vor 1.0 übernommen: {slides}, {n} Anmerkung."
msgstr[1] "Anmerkungsdatenbank aus dem Format vor 1.0 übernommen: {slides}, {n} Anmerkungen."

msgid "Upgraded the annotation database from the pre-1.0 format: {slides}, {annotations}, {n} unreadable annotation skipped."
msgid_plural "Upgraded the annotation database from the pre-1.0 format: {slides}, {annotations}, {n} unreadable annotations skipped."
msgstr[0] "Anmerkungsdatenbank aus dem Format vor 1.0 übernommen: {slides}, {annotations}, {n} unlesbare Anmerkung ausgelassen."
msgstr[1] "Anmerkungsdatenbank aus dem Format vor 1.0 übernommen: {slides}, {annotations}, {n} unlesbare Anmerkungen ausgelassen."
"#;

/// Japanese has one plural form, which has the number: "Delete annotation"
/// as the singular of "Delete {n} annotations" would show "{n}".
const JAPANESE: &str = r#"msgid ""
msgstr ""
"Language: ja\n"
"Content-Type: text/plain; charset=UTF-8\n"
"Plural-Forms: nplurals=1; plural=0;\n"

msgid "Delete annotation"
msgid_plural "Delete {n} annotations"
msgstr[0] "{n} 件の注釈を削除"

msgctxt "Context menu"
msgid "Delete annotation"
msgstr "注釈を削除"

msgctxt "ImportDialog"
msgid "The import is one step: Ctrl+Z (⌘Z) removes everything it added."
msgstr "インポートは 1 回の操作として記録されます。Ctrl+Z（⌘Z）で、インポートで追加されたものをすべて元に戻せます。"
"#;

/// A plugin root with the plugin's manifest and the test catalogs.
fn translated_root(dir: &Path) -> PathBuf {
    let root = dir.join("plugin");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("plugin.toml"), include_str!("../plugin.toml")).unwrap();
    for (tag, catalog) in [("de", GERMAN), ("ja", JAPANESE)] {
        let catalogs = root.join("translations").join(tag).join("LC_MESSAGES");
        std::fs::create_dir_all(&catalogs).unwrap();
        std::fs::write(catalogs.join("annotations.po"), catalog).unwrap();
    }
    root
}

/// The plugin with the test catalogs, and its database: (plugin, folder,
/// database).
fn start() -> (H, PathBuf, PathBuf) {
    let mut init = test_init("annotations");
    let dir = PathBuf::from(&init.cache_dir);
    let db = dir.join("annotations.db");
    init.plugin_root = translated_root(&dir).to_string_lossy().into_owned();
    init.settings
        .insert(DATABASE_PATH_KEY.into(), json!(db.to_string_lossy()));
    let h: H = TestHarness::with_host(MockHost::new(), init).unwrap();
    settle(&h);
    (h, dir, db)
}

/// Slide 1, in `dir`.
fn slide(dir: &Path) -> FileInfo {
    FileInfo {
        file_id: 1,
        path: dir.join("a.svs").to_string_lossy().into_owned(),
        filename: "a.svs".into(),
        width: 1000,
        height: 1000,
        identity: Some("eov-slide-1:a".into()),
        ..Default::default()
    }
}

/// Opens a slide (file 1) and makes it the active one.
fn open_slide(h: &H, dir: &Path) {
    open_file(h, slide(dir));
}

/// Opens `file` and makes it the active one.
fn open_file(h: &H, file: FileInfo) {
    let file_id = file.file_id;
    h.state().snapshot.files.push(file.clone());
    h.send(HostEvent::FileOpened { file }).unwrap();
    h.send(HostEvent::ActiveChanged {
        pane: Some(1),
        file_id: Some(file_id),
    })
    .unwrap();
    settle(h);
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

fn texts(h: &H) -> (String, String, Option<String>, String) {
    let state = h.state();
    let sidebar = state
        .surfaces
        .values()
        .find(|s| s.open && s.spec.component == annotations::sidebar::SIDEBAR_COMPONENT)
        .expect("the panel is open");
    (
        state.tools[annotations::tools::SELECT].label.clone(),
        state.toolbar_buttons[annotations::tools::SIDEBAR_BUTTON]
            .tooltip
            .clone(),
        state.status.clone(),
        sidebar.properties["slide-title"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

fn switch(h: &H, language: &str) {
    h.send(HostEvent::LanguageChanged {
        language: language.into(),
    })
    .unwrap();
    settle(h);
}

#[test]
fn texts_follow_the_language_and_the_database_stays_english() {
    let _serial = serial();
    let (h, dir, db) = start();
    h.send(HostEvent::ToolbarButtonClicked {
        button_id: annotations::tools::SIDEBAR_BUTTON.into(),
    })
    .unwrap();
    h.send(HostEvent::ToolActivated {
        tool_id: annotations::tools::COUNT.into(),
    })
    .unwrap();
    settle(&h);
    assert_eq!(
        texts(&h),
        (
            "Select and edit annotations".to_string(),
            "Annotations".to_string(),
            Some("Counting Unclassified".to_string()),
            "No slide open".to_string(),
        )
    );

    switch(&h, "de");
    assert_eq!(
        texts(&h),
        (
            "Anmerkungen auswählen und bearbeiten".to_string(),
            "Anmerkungen".to_string(),
            // "Unclassified" is not in the catalog: it stays English.
            Some("Zählung: Unclassified".to_string()),
            "Kein Objektträger geöffnet".to_string(),
        )
    );
    let hud = h.calls_to(methods::HUD_UPDATE);
    assert_eq!(
        hud.last().and_then(|u| u["tooltip"].as_str()),
        Some("Anmerkungen auf diesem Objektträger zeigen oder verbergen")
    );

    // An edit: eov's undo history names it in German, the audit trail in
    // English.
    open_slide(&h, &dir);
    h.send(HostEvent::ShapeDrawn {
        tool_id: annotations::tools::POINT.into(),
        pane: 1,
        file_id: 1,
        shape: Shape::Point { x: 10.0, y: 10.0 },
        modifiers: Modifiers::default(),
    })
    .unwrap();
    settle(&h);
    let undo = h.state().undo.last().cloned().expect("an undo entry");
    assert_eq!(undo.label, "Punkt hinzufügen");
    let action: String = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT action FROM audit_log ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(action, "Add point");

    switch(&h, "en");
    let (tool, tooltip, ..) = texts(&h);
    assert_eq!(
        (tool.as_str(), tooltip.as_str()),
        ("Select and edit annotations", "Annotations")
    );
    h.send(HostEvent::Shutdown).unwrap();
}

/// The `log` calls the plugin made: (level, message).
fn logs(h: &H) -> Vec<(String, String)> {
    h.calls_to(methods::LOG)
        .iter()
        .map(|log| {
            (
                log["level"].as_str().unwrap_or_default().to_string(),
                log["message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_failed_load_is_told_in_the_language_of_the_interface_and_logged_in_english() {
    let _serial = serial();
    let (h, dir, db) = start();
    switch(&h, "de");
    // The database is open, but the slides cannot be read.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("ALTER TABLE slides RENAME TO gone")
        .unwrap();
    open_slide(&h, &dir);
    let warnings: Vec<String> = h
        .state()
        .notifications
        .iter()
        .filter(|n| n.level == NotifyLevel::Warning)
        .map(|n| n.message.clone())
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .starts_with("Die Anmerkungen dieses Objektträgers konnten nicht geladen werden: "),
        "{warnings:?}"
    );
    // The log is English, and nothing went through eov's warnings, whose
    // text the user would read as it is.
    let logs = logs(&h);
    assert!(
        logs.iter().any(|(level, message)| level == "info"
            && message.starts_with("cannot load the annotations of this slide: ")),
        "{logs:?}"
    );
    assert!(
        logs.iter()
            .all(|(level, _)| level != "warn" && level != "error"),
        "{logs:?}"
    );
    h.send(HostEvent::Shutdown).unwrap();
}

/// The HPF reticle's label is in the language of the interface on screen;
/// eov burns it into images (exports, copies of the view) in English, with
/// `.` decimals, which the plugin hands over with it.
#[test]
fn the_reticle_label_is_translated_and_burned_into_images_in_english() {
    let _serial = serial();
    let (h, dir, _) = start();
    switch(&h, "de");
    // Calibrated: an HPF has its size in mm².
    open_file(
        &h,
        FileInfo {
            width: 10_000,
            height: 8_000,
            mpp_x: Some(0.25),
            mpp_y: Some(0.25),
            ..slide(&dir)
        },
    );
    h.send(HostEvent::ToolActivated {
        tool_id: annotations::tools::FIELD.into(),
    })
    .unwrap();
    h.send(HostEvent::Pointer {
        tool_id: annotations::tools::FIELD.into(),
        pane: 1,
        file_id: 1,
        event: PointerEvent {
            kind: PointerKind::Move,
            button: PointerButton::Left,
            image_x: 5000.0,
            image_y: 4000.0,
            screen_x: 100.0,
            screen_y: 100.0,
            modifiers: Modifiers::default(),
            wheel_delta: 0.0,
        },
    })
    .unwrap();
    settle(&h);
    let reticle = h.state().objects("hpf.1");
    assert_eq!(reticle.len(), 1);
    assert_eq!(
        reticle[0].label.as_deref(),
        Some("1 HPF · 0,2376 mm² · 0 Punkte")
    );
    assert_eq!(
        reticle[0].export_label.as_deref(),
        Some("1 HPF · 0.2376 mm² · 0 points")
    );
    h.send(HostEvent::Shutdown).unwrap();
}

#[test]
fn deleting_from_the_context_menu_is_worded_without_a_number() {
    let _serial = serial();
    let (h, dir, _) = start();
    open_slide(&h, &dir);
    h.send(HostEvent::ShapeDrawn {
        tool_id: annotations::tools::POINT.into(),
        pane: 1,
        file_id: 1,
        shape: Shape::Point { x: 10.0, y: 10.0 },
        modifiers: Modifiers::default(),
    })
    .unwrap();
    settle(&h);
    switch(&h, "ja");
    let hit = {
        let state = h.state();
        let (layer_id, layer) = state
            .layers
            .iter()
            .find(|(id, layer)| id.starts_with("ann.1.") && !layer.objects.is_empty())
            .expect("the point's layer");
        ObjectRef {
            layer_id: layer_id.clone(),
            object_id: layer.objects.keys().next().unwrap().clone(),
        }
    };
    let context = MenuContext {
        pane: 1,
        view: ViewInfo {
            pane: 1,
            file_id: 1,
            center_x: 500.0,
            center_y: 500.0,
            zoom: 1.0,
            width: 1000.0,
            height: 1000.0,
            scale_factor: 1.0,
            rotation_deg: 0.0,
            flipped: false,
            visible: Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 1000.0,
            },
        },
        image_x: 10.0,
        image_y: 10.0,
        hit: Some(hit),
        selection: Vec::new(),
    };
    let items = h.context_menu(context.clone()).unwrap();
    let delete = items
        .iter()
        .find(|item| item.item_id == "delete")
        .expect("a delete item");
    assert_eq!(delete.label, "注釈を削除");
    // Where the number is shown, the text with the number is used: eov's
    // undo history.
    h.send(HostEvent::ContextMenuItemSelected {
        item_id: "delete".into(),
        context,
    })
    .unwrap();
    settle(&h);
    let undo = h.state().undo.last().cloned().expect("an undo entry");
    assert_eq!(undo.label, "1 件の注釈を削除");
    h.send(HostEvent::Shutdown).unwrap();
}

#[test]
fn a_language_change_asks_eov_again_why_it_refused_a_key() {
    let _serial = serial();
    let (h, _dir, _) = start();
    h.send(HostEvent::ToolbarButtonClicked {
        button_id: annotations::tools::SIDEBAR_BUTTON.into(),
    })
    .unwrap();
    settle(&h);
    let sidebar = h
        .state()
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.component == annotations::sidebar::SIDEBAR_COMPONENT)
        .map(|(id, _)| id.clone())
        .expect("the panel is open");
    let classes = |h: &H| -> Vec<Value> {
        h.state().surfaces[&sidebar].properties["classes"]
            .as_array()
            .unwrap()
            .clone()
    };
    let (kept, refused) = {
        let classes = classes(&h);
        (classes[0].clone(), classes[1].clone())
    };
    for (class, key) in [(&kept, "q"), (&refused, "y")] {
        h.ui_callback(
            &sidebar,
            "class-hotkey",
            vec![class["id"].clone(), json!(key)],
        )
        .unwrap();
        settle(&h);
    }
    let kept_id = format!("class-{}", kept["id"].as_str().unwrap());
    let refused_id = format!("class-{}", refused["id"].as_str().unwrap());
    // eov's reason, in English and as eov shows it in `language`.
    const IN_ENGLISH: &str = "Hotkey 'y' was ignored: it is the same key as other:tool.";
    const IN_GERMAN: &str = "Tastenkürzel „y“ wurde ignoriert: other:tool hat dieselbe Taste.";
    let refusal = |id: &str, language: &str| HostEvent::RegistrationRefused {
        what: "hotkey".into(),
        id: id.into(),
        reason: IN_ENGLISH.into(),
        shown_reason: Some(
            if language == "de" {
                IN_GERMAN
            } else {
                IN_ENGLISH
            }
            .into(),
        ),
    };
    let note = |h: &H| -> String {
        classes(h)
            .into_iter()
            .find(|c| c["id"] == refused["id"])
            .and_then(|c| c["hotkey-note"].as_str().map(str::to_string))
            .unwrap()
    };
    // eov refused one class's key and one tool's.
    for id in [refused_id.as_str(), annotations::tools::POLYGON] {
        h.send(refusal(id, "en")).unwrap();
    }
    settle(&h);
    assert_eq!(note(&h), format!("eov refused the hotkey: {IN_ENGLISH}"));
    for language in ["de", "en"] {
        let before = h.state().calls.len();
        switch(&h, language);
        let calls = h.state().calls[before..].to_vec();
        let about = |id: &str| -> Vec<String> {
            calls
                .iter()
                .filter(|c| {
                    (c.method == methods::HOTKEY_REGISTER && c.args["hotkey_id"] == id)
                        || (c.method == methods::HOTKEY_UNREGISTER && c.args["id"] == id)
                })
                .map(|c| c.method.clone())
                .collect()
        };
        // The binding eov accepted is renamed in place...
        assert_eq!(about(&kept_id), vec![methods::HOTKEY_REGISTER.to_string()]);
        // ...the key it refused is offered once more, so that eov says why
        // in the new language (it does not warn the user again)...
        assert_eq!(
            about(&refused_id),
            vec![methods::HOTKEY_REGISTER.to_string()]
        );
        // ...and the tool whose key it refused is renamed without it.
        let tools: Vec<&Value> = calls
            .iter()
            .filter(|c| c.method == methods::TOOL_REGISTER)
            .map(|c| &c.args)
            .collect();
        assert_eq!(tools.len(), annotations::tools::tool_specs().len());
        for tool in tools {
            assert_eq!(
                tool["hotkey"].is_null(),
                tool["tool_id"] == annotations::tools::POLYGON,
                "{tool}"
            );
        }
        // Until eov answers, the class has no note: a key eov accepts now
        // keeps none.
        assert_eq!(note(&h), "");
        h.send(refusal(&refused_id, language)).unwrap();
        settle(&h);
    }
    assert_eq!(note(&h), format!("eov refused the hotkey: {IN_ENGLISH}"));
    let description = |language: &str, class: &Value| {
        let name = class["name"].as_str().unwrap();
        match language {
            "de" => format!("Anmerkungsklasse: {name}"),
            _ => format!("Annotation class: {name}"),
        }
    };
    assert_eq!(
        h.state().hotkeys[&kept_id].description,
        description("en", &kept)
    );
    switch(&h, "de");
    h.send(refusal(&refused_id, "de")).unwrap();
    settle(&h);
    for (id, class) in [(&kept_id, &kept), (&refused_id, &refused)] {
        assert_eq!(h.state().hotkeys[id].description, description("de", class));
    }
    // The Classes tab says why the key does nothing, in German only.
    assert_eq!(
        note(&h),
        format!("eov hat das Tastenkürzel abgelehnt: {IN_GERMAN}")
    );
    h.send(HostEvent::Shutdown).unwrap();
}

#[test]
fn each_number_in_a_text_takes_its_own_plural_form() {
    let _serial = serial();
    let german = i18n::by_tag("de").unwrap();
    i18n::set_catalog(
        german,
        Some(i18n::Catalog::parse(german, GERMAN.as_bytes()).unwrap()),
    );
    let upgraded = |slides, annotations, skipped| {
        annotations::db::DbNote::Upgraded {
            slides,
            annotations,
            skipped,
        }
        .text()
    };
    let texts = (
        upgraded(1, 5, 0),
        upgraded(3, 1, 2),
        annotations::sidebar::in_layers(1),
        annotations::sidebar::in_layers(4),
    );
    i18n::set_catalog(i18n::source(), None);
    assert_eq!(
        texts,
        (
            "Anmerkungsdatenbank aus dem Format vor 1.0 übernommen: 1 Präparat, 5 Anmerkungen."
                .to_string(),
            "Anmerkungsdatenbank aus dem Format vor 1.0 übernommen: 3 Präparate, 1 Anmerkung, 2 unlesbare Anmerkungen ausgelassen."
                .to_string(),
            "in 1 Ebene".to_string(),
            "in 4 Ebenen".to_string(),
        )
    );
}

/// The plugin's open panel.
fn sidebar_id(h: &H) -> String {
    h.state()
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Sidebar)
        .map(|(id, _)| id.clone())
        .expect("the panel is open")
}

/// Answers the newest file dialog the plugin waits for with `path`.
fn answer_dialog(h: &H, path: &Path) {
    let pending = h.ui_callback("", "__pending", vec![]).unwrap();
    let request_id = pending
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .max_by_key(|id| id.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()))
        .expect("a pending request")
        .to_string();
    h.send(HostEvent::DialogResult {
        request_id,
        paths: vec![path.to_string_lossy().into_owned()],
    })
    .unwrap();
    settle(h);
}

#[test]
fn latin_values_keep_their_order_in_right_to_left_languages() {
    let _serial = serial();
    let (h, dir, _db) = start();
    open_slide(&h, &dir);
    // Hebrew without a catalog: English texts, with the values they insert
    // isolated as in every right-to-left language.
    switch(&h, "he");
    h.send(HostEvent::ToolbarButtonClicked {
        button_id: annotations::tools::SIDEBAR_BUTTON.into(),
    })
    .unwrap();
    settle(&h);
    // The numbers of the HPF size (its units are the translation's).
    let sidebar = sidebar_id(&h);
    let field_text = h.state().surfaces[&sidebar].properties["field-text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        field_text,
        "1 HPF = \u{2068}0.2376\u{2069} mm² (field number \u{2068}22\u{2069} at \u{2068}40\u{2069}×, ⌀ \u{2068}0.550\u{2069} mm)"
    );
    let file = dir.join("regions.geojson");
    std::fs::write(
        &file,
        r#"{"type": "FeatureCollection", "features": [{"type": "Feature",
            "geometry": {"type": "Polygon", "coordinates": [[[10,10],[110,10],[110,110],[10,110],[10,10]]]},
            "properties": {}}]}"#,
    )
    .unwrap();
    h.ui_callback(&sidebar_id(&h), "menu", vec![json!("import")])
        .unwrap();
    settle(&h);
    answer_dialog(&h, &file);
    let summary = h
        .state()
        .surfaces
        .values()
        .find(|s| s.open && s.spec.kind == SurfaceKind::Modal)
        .and_then(|s| s.properties["summary"].as_str().map(str::to_string))
        .expect("the import dialog");
    // The format's name is the line's first word: isolated, it does not
    // make the whole line run from left to right.
    assert_eq!(summary, "\u{2068}GeoJSON\u{2069} · 1 annotation in 1 layer");
    switch(&h, "en");
    h.send(HostEvent::Shutdown).unwrap();
}

/// The open import dialog: (surface id, height).
fn import_dialog(h: &H) -> Option<(String, Option<f32>)> {
    h.state()
        .surfaces
        .iter()
        .find(|(_, s)| s.open && s.spec.kind == SurfaceKind::Modal)
        .map(|(id, s)| (id.clone(), s.spec.height))
}

#[test]
fn the_import_dialog_is_taller_where_its_texts_take_more_lines() {
    let _serial = serial();
    let (h, dir, _db) = start();
    open_slide(&h, &dir);
    h.send(HostEvent::ToolbarButtonClicked {
        button_id: annotations::tools::SIDEBAR_BUTTON.into(),
    })
    .unwrap();
    settle(&h);
    let file = dir.join("cells.geojson");
    std::fs::write(
        &file,
        r#"{"type": "FeatureCollection", "features": [{"type": "Feature",
            "geometry": {"type": "Point", "coordinates": [10, 10]}, "properties": {}}]}"#,
    )
    .unwrap();
    let import = |h: &H| {
        h.ui_callback(&sidebar_id(h), "menu", vec![json!("import")])
            .unwrap();
        settle(h);
        answer_dialog(h, &file);
        import_dialog(h).expect("the import dialog")
    };
    // The first import makes the layer; the next ones offer what to do with
    // it, which fills the dialog in English.
    let (id, height) = import(&h);
    assert_eq!(height, Some(400.0));
    h.ui_callback(&id, "import-confirm", vec![json!(0)])
        .unwrap();
    settle(&h);
    let (id, height) = import(&h);
    assert_eq!(height, Some(400.0));
    h.ui_callback(&id, "import-cancel", vec![]).unwrap();
    settle(&h);
    // In Japanese the last sentence takes two lines.
    switch(&h, "ja");
    let (id, height) = import(&h);
    assert!(height.is_some_and(|h| h > 400.0), "{height:?}");
    h.ui_callback(&id, "import-cancel", vec![]).unwrap();
    settle(&h);
    switch(&h, "en");
    h.send(HostEvent::Shutdown).unwrap();
}
