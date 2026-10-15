use super::*;
use crate::change::Change;
use crate::model::{Class, Color, Geometry, Layer, LayerRole, SlideRecord, Status, now_ms};
use std::sync::Arc;

/// The DDL of the pre-1.0 plugin, verbatim.
const LEGACY_DDL: &str = r#"
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS annotation_layers (
    id TEXT PRIMARY KEY,
    fingerprint BLOB NOT NULL CHECK(length(fingerprint) = 32),
    name TEXT NOT NULL CHECK(length(name) <= 255),
    notes TEXT,
    color TEXT NOT NULL CHECK(length(color) = 7),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS annotations (
    id TEXT PRIMARY KEY,
    annotation_layer_id TEXT NOT NULL,
    type TEXT NOT NULL CHECK(type IN ('point', 'ellipse', 'polygon', 'bitmask')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (annotation_layer_id) REFERENCES annotation_layers(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS annotation_points (
    annotation_id TEXT PRIMARY KEY,
    x_level0 REAL NOT NULL,
    y_level0 REAL NOT NULL,
    FOREIGN KEY (annotation_id) REFERENCES annotations(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS annotation_polygons (
    annotation_id TEXT PRIMARY KEY,
    FOREIGN KEY (annotation_id) REFERENCES annotations(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS annotation_polygon_vertices (
    annotation_id TEXT NOT NULL,
    vertex_index INTEGER NOT NULL,
    x_level0 REAL NOT NULL,
    y_level0 REAL NOT NULL,
    PRIMARY KEY (annotation_id, vertex_index),
    FOREIGN KEY (annotation_id) REFERENCES annotation_polygons(annotation_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS annotation_metadata (
    annotation_id TEXT NOT NULL,
    row_index INTEGER NOT NULL,
    key TEXT NOT NULL DEFAULT '',
    value TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (annotation_id, row_index),
    FOREIGN KEY (annotation_id) REFERENCES annotations(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS export_metadata_settings (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    author TEXT NOT NULL DEFAULT '',
    organization TEXT NOT NULL DEFAULT '',
    project_name TEXT NOT NULL DEFAULT '',
    license TEXT NOT NULL DEFAULT ''
);
INSERT OR IGNORE INTO export_metadata_settings (id, author, organization, project_name, license)
VALUES (1, 'Dr. Example', '', '', '');
"#;

const FP: [u8; 32] = [7u8; 32];

fn legacy_db(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(LEGACY_DDL).unwrap();
    conn.execute(
        "INSERT INTO annotation_layers VALUES ('L1', ?1, 'Mitoses', NULL, '#FF0000', 100, 200)",
        [FP.as_slice()],
    )
    .unwrap();
    // ANN-4: a colour that passes the old CHECK but is not a colour.
    conn.execute(
        "INSERT INTO annotation_layers VALUES ('L2', ?1, 'Bad colour', 'n', '##aébcd', 100, 200)",
        [FP.as_slice()],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO annotations VALUES ('P1', 'L1', 'point', 10, 11);
         INSERT INTO annotation_points VALUES ('P1', 5.5, 6.5);
         INSERT INTO annotation_metadata VALUES ('P1', 0, 'grade', '2');
         INSERT INTO annotation_metadata VALUES ('P1', 1, 'note', 'x');
         INSERT INTO annotations VALUES ('G1', 'L2', 'polygon', 10, 11);
         INSERT INTO annotation_polygons VALUES ('G1');
         INSERT INTO annotation_polygon_vertices VALUES ('G1', 0, 0, 0);
         INSERT INTO annotation_polygon_vertices VALUES ('G1', 1, 1000, 1000);
         INSERT INTO annotation_polygon_vertices VALUES ('G1', 2, 1000, 0);
         INSERT INTO annotation_polygon_vertices VALUES ('G1', 3, 0, 1000);
         INSERT INTO annotations VALUES ('G2', 'L2', 'polygon', 10, 11);
         INSERT INTO annotation_polygons VALUES ('G2');
         INSERT INTO annotation_polygon_vertices VALUES ('G2', 0, 0, 0);
         INSERT INTO annotation_polygon_vertices VALUES ('G2', 1, 1, 1);",
    )
    .unwrap();
}

fn layer(slide_id: i64, id: &str) -> Layer {
    Layer {
        id: id.to_string(),
        slide_id,
        name: format!("Layer {id}"),
        notes: String::new(),
        color: Color(1, 2, 3),
        visible: true,
        opacity: 1.0,
        locked: false,
        role: LayerRole::Normal,
        sort_order: 0,
        created_at: 1,
        updated_at: 1,
    }
}

fn point(id: &str, layer_id: &str, x: f64) -> Annotation {
    Annotation {
        id: id.to_string(),
        layer_id: layer_id.to_string(),
        class_id: None,
        geometry: Geometry::Point { x, y: 1.0 },
        name: String::new(),
        description: String::new(),
        status: Status::Draft,
        author: "me".into(),
        locked: false,
        properties: vec![("k".into(), "v".into())],
        comments: vec![],
        created_at: 5,
        updated_at: 5,
    }
}

fn new_slide(db: &mut Db) -> SlideRecord {
    db.upsert_slide(&SlideRecord {
        identity: Some("eov-slide-1:abc".into()),
        filename: "a.svs".into(),
        width: 1000,
        height: 1000,
        created_at: now_ms(),
        last_opened_at: now_ms(),
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn fresh_databases_get_the_current_schema_and_indexes() {
    let db = Db::open_in_memory();
    let conn = db.connection();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    for index in [
        "layers_slide",
        "annotations_layer",
        "slides_identity",
        "annotations_class",
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
                [index],
                |r| r.get(0),
            )
            .unwrap();
        assert!(exists, "{index}");
    }
}

#[test]
fn the_pre_1_0_schema_is_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.db");
    legacy_db(&path);
    let (mut db, report) = Db::open(&path, dir.path()).unwrap();
    assert_eq!(report.migrated_from, Some(1));
    assert!(
        report.notes.iter().any(|n| n.contains("1 unreadable")),
        "{report:?}"
    );
    assert!(!db.is_read_only());
    let slide = db.find_slide(None, Some(&FP), None).unwrap().unwrap();
    let loaded = db.load_slide(slide.id).unwrap();
    assert_eq!(loaded.layers.len(), 2);
    let bad = loaded
        .layers
        .iter()
        .find(|l| l.name == "Bad colour")
        .unwrap();
    assert_eq!(bad.notes, "n");
    assert_eq!(
        loaded.annotations.len(),
        2,
        "the 2-vertex polygon is skipped"
    );
    let p1 = loaded.annotations.iter().find(|a| a.id == "P1").unwrap();
    assert_eq!(p1.geometry, Geometry::Point { x: 5.5, y: 6.5 });
    assert_eq!(
        p1.properties,
        vec![("grade".into(), "2".into()), ("note".into(), "x".into())]
    );
    assert_eq!(p1.created_at, 10_000, "seconds become milliseconds");
    // The bowtie is flagged (ANN-3) and checked only once.
    assert!(loaded.invalid.contains("G1"));
    assert_eq!(
        db.setting("export.author").unwrap().as_deref(),
        Some("Dr. Example")
    );
    // The old tables are kept, renamed.
    let kept: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM legacy_v1_annotation_layers",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 2);
    drop(db);
    // Re-opening does not migrate again.
    let (_db, report) = Db::open(&path, dir.path()).unwrap();
    assert_eq!(report.migrated_from, None);
}

#[test]
fn changes_apply_atomically_and_check_their_preconditions() {
    let mut db = Db::open_in_memory();
    let slide = new_slide(&mut db);
    let l = layer(slide.id, "L");
    let a = Arc::new(point("A", "L", 1.0));
    db.apply(
        &[
            Change::Layer {
                before: None,
                after: Some(l.clone()),
            },
            Change::Annotation {
                before: None,
                after: Some(a.clone()),
            },
        ],
        Some(&Audit {
            action: "Add point".into(),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(db.load_slide(slide.id).unwrap().annotations.len(), 1);

    // An update that expects a stale version is refused, and nothing of
    // the transaction is applied.
    let mut stale = point("A", "L", 1.0);
    stale.updated_at = 999;
    let mut moved = point("A", "L", 50.0);
    moved.updated_at = 10;
    let b = Arc::new(point("B", "L", 2.0));
    let result = db.apply(
        &[
            Change::Annotation {
                before: None,
                after: Some(b),
            },
            Change::Annotation {
                before: Some(Arc::new(stale)),
                after: Some(Arc::new(moved.clone())),
            },
        ],
        None,
    );
    assert!(matches!(result, Err(DbError::Conflict(_))), "{result:?}");
    let loaded = db.load_slide(slide.id).unwrap();
    assert_eq!(loaded.annotations.len(), 1, "B was rolled back");

    // The right version passes.
    db.apply(
        &[Change::Annotation {
            before: Some(a.clone()),
            after: Some(Arc::new(moved)),
        }],
        None,
    )
    .unwrap();
    let audit: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(audit, 1);
}

#[test]
fn layers_are_soft_deleted_restored_and_purged() {
    let mut db = Db::open_in_memory();
    let slide = new_slide(&mut db);
    let l = layer(slide.id, "L");
    let contents: Vec<Arc<Annotation>> = (0..3)
        .map(|i| Arc::new(point(&format!("a{i}"), "L", i as f64)))
        .collect();
    db.apply(
        &[Change::RestoreLayer {
            layer: l.clone(),
            contents: contents.clone(),
        }],
        None,
    )
    .unwrap();
    assert_eq!(db.load_slide(slide.id).unwrap().annotations.len(), 3);
    db.apply(
        &[Change::RemoveLayer {
            layer: l.clone(),
            contents: contents.clone(),
        }],
        None,
    )
    .unwrap();
    let loaded = db.load_slide(slide.id).unwrap();
    assert!(loaded.layers.is_empty() && loaded.annotations.is_empty());
    // Undo restores everything.
    db.apply(
        &[Change::RestoreLayer {
            layer: l.clone(),
            contents: contents.clone(),
        }],
        None,
    )
    .unwrap();
    assert_eq!(db.load_slide(slide.id).unwrap().annotations.len(), 3);
    // Purging only removes old deletions.
    db.apply(&[Change::RemoveLayer { layer: l, contents }], None)
        .unwrap();
    assert_eq!(db.purge_deleted(now_ms() - PURGE_AFTER_MS).unwrap(), 0);
    assert!(db.purge_deleted(now_ms() + 1000).unwrap() >= 1);
}

#[test]
fn classes_are_soft_deleted_and_names_can_be_reused() {
    let mut db = Db::open_in_memory();
    let class = Class {
        id: "c1".into(),
        name: "Tumour".into(),
        color: Color(255, 0, 0),
        hotkey: Some("q".into()),
        visible: true,
        opacity: 0.5,
        sort_order: 0,
        created_at: 1,
        updated_at: 1,
    };
    db.apply(
        &[Change::Class {
            before: None,
            after: Some(class.clone()),
        }],
        None,
    )
    .unwrap();
    assert_eq!(db.load_classes().unwrap(), vec![class.clone()]);
    db.apply(
        &[Change::Class {
            before: Some(class.clone()),
            after: None,
        }],
        None,
    )
    .unwrap();
    assert!(db.load_classes().unwrap().is_empty());
    let mut again = class.clone();
    again.id = "c2".into();
    db.apply(
        &[Change::Class {
            before: None,
            after: Some(again),
        }],
        None,
    )
    .unwrap();
    assert_eq!(db.load_classes().unwrap().len(), 1);
}

#[test]
fn slides_are_found_by_identity_fingerprint_or_fallback() {
    let mut db = Db::open_in_memory();
    let created = db
        .upsert_slide(&SlideRecord {
            legacy_fingerprint: Some(FP),
            created_at: 1,
            last_opened_at: 1,
            ..Default::default()
        })
        .unwrap();
    assert!(db.has_unmatched_legacy_slides().unwrap());
    // The legacy fingerprint matches; the identity is then attached.
    let found = db
        .find_slide(Some("eov-slide-1:x"), Some(&FP), None)
        .unwrap()
        .unwrap();
    assert_eq!(found.id, created.id);
    let mut update = found.clone();
    update.identity = Some("eov-slide-1:x".into());
    update.filename = "renamed.svs".into();
    db.upsert_slide(&update).unwrap();
    assert!(!db.has_unmatched_legacy_slides().unwrap());
    let by_identity = db
        .find_slide(Some("eov-slide-1:x"), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(by_identity.filename, "renamed.svs");
    // A different identity never takes over a fingerprint already claimed.
    assert!(
        db.find_slide(Some("eov-slide-1:other"), Some(&FP), None)
            .unwrap()
            .is_none()
    );
    let fallback = db
        .upsert_slide(&SlideRecord {
            fallback_key: Some("file:a.tif:10x10".into()),
            created_at: 1,
            last_opened_at: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        db.find_slide(None, None, Some("file:a.tif:10x10"))
            .unwrap()
            .unwrap()
            .id,
        fallback.id
    );
}

#[cfg(unix)]
#[test]
fn read_only_databases_open_read_only_and_old_ones_are_migrated_in_a_copy() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("teaching.db");
    legacy_db(&path);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let scratch = dir.path().join("scratch");
    let (mut db, report) = Db::open(&path, &scratch).unwrap();
    assert!(db.is_read_only());
    assert!(report.notes.iter().any(|n| n.contains("read-only")));
    let slide = db.find_slide(None, Some(&FP), None).unwrap().unwrap();
    assert_eq!(db.load_slide(slide.id).unwrap().annotations.len(), 2);
    assert!(matches!(db.apply(&[], None), Err(DbError::ReadOnly)));
    // The original file is untouched.
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn external_writes_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let (mut db, _) = Db::open(&path, dir.path()).unwrap();
    assert!(!db.changed_externally());
    let (mut other, _) = Db::open(&path, dir.path()).unwrap();
    other.set_setting("x", "1").unwrap();
    assert!(db.changed_externally());
    assert!(!db.changed_externally());
    // Own writes do not count.
    db.set_setting("y", "2").unwrap();
    assert!(!db.changed_externally());
}

#[test]
fn backups_and_merges_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.db");
    let (mut db, _) = Db::open(&path, dir.path()).unwrap();
    let slide = new_slide(&mut db);
    let l = layer(slide.id, "L");
    let mut commented = point("A", "L", 1.0);
    commented.comments.push(crate::model::Comment {
        id: "c1".into(),
        author: "Dr. B".into(),
        text: "Agree".into(),
        created_at: 7,
    });
    db.apply(
        &[Change::RestoreLayer {
            layer: l,
            contents: vec![Arc::new(commented)],
        }],
        None,
    )
    .unwrap();
    let backup = dir.path().join("backup.db");
    db.backup_to(&backup).unwrap();

    // Merge the backup into an empty database: everything comes back.
    let (mut fresh, _) = Db::open(&dir.path().join("fresh.db"), dir.path()).unwrap();
    let (slides, layers, annotations) = fresh.merge_from(&backup, &dir.path().join("s")).unwrap();
    assert_eq!((slides, layers, annotations), (1, 1, 1));
    // Merging again adds nothing.
    assert_eq!(
        fresh.merge_from(&backup, &dir.path().join("s")).unwrap(),
        (0, 0, 0)
    );
    let found = fresh
        .find_slide(Some("eov-slide-1:abc"), None, None)
        .unwrap()
        .unwrap();
    let merged = fresh.load_slide(found.id).unwrap().annotations;
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].comments[0].text, "Agree", "comments are merged");

    // A pre-1.0 database can be merged too.
    let legacy = dir.path().join("old.db");
    legacy_db(&legacy);
    let (_, _, annotations) = fresh.merge_from(&legacy, &dir.path().join("s")).unwrap();
    assert_eq!(annotations, 2);
    // The source is untouched.
    let conn = rusqlite::Connection::open(&legacy).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    // Not a database at all.
    let junk = dir.path().join("junk.db");
    std::fs::write(&junk, b"this is not sqlite at all, not even close......").unwrap();
    assert!(fresh.merge_from(&junk, &dir.path().join("s")).is_err());
}

#[test]
fn unreadable_rows_are_skipped_not_fatal() {
    let mut db = Db::open_in_memory();
    let slide = new_slide(&mut db);
    db.apply(
        &[Change::RestoreLayer {
            layer: layer(slide.id, "L"),
            contents: vec![Arc::new(point("A", "L", 1.0))],
        }],
        None,
    )
    .unwrap();
    db.connection()
        .execute(
            "INSERT INTO annotations (id, layer_id, kind, geometry, min_x, min_y, max_x, max_y, created_at, updated_at)
             VALUES ('bad', 'L', 'polygon', '{not json', 0, 0, 0, 0, 1, 1)",
            [],
        )
        .unwrap();
    let loaded = db.load_slide(slide.id).unwrap();
    assert_eq!(loaded.annotations.len(), 1);
    assert_eq!(loaded.skipped, 1);
}

#[test]
fn version_2_databases_gain_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(schema::SCHEMA_V2).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute_batch(
            "INSERT INTO slides (id, identity, created_at, last_opened_at) VALUES (1, 'eov-slide-1:abc', 1, 1);
             INSERT INTO layers (id, slide_id, name, color, created_at, updated_at) VALUES ('L', 1, 'L', '#FF0000', 1, 1);
             INSERT INTO annotations (id, layer_id, kind, geometry, min_x, min_y, max_x, max_y, created_at, updated_at)
             VALUES ('p', 'L', 'point', '{\"kind\":\"point\",\"x\":5.0,\"y\":5.0}', 5, 5, 5, 5, 1, 1);",
        )
        .unwrap();
    }
    let (mut db, report) = Db::open(&path, dir.path()).unwrap();
    assert_eq!(report.migrated_from, None, "a silent upgrade");
    let version: i64 = db
        .connection()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let loaded = db.load_slide(1).unwrap();
    let before = loaded.annotations[0].clone();
    assert!(before.comments.is_empty());
    let mut after = (*before).clone();
    after.comments.push(crate::model::Comment {
        id: "c".into(),
        author: "me".into(),
        text: "Mitosis?".into(),
        created_at: 9,
    });
    after.updated_at = 2;
    db.apply(
        &[Change::Annotation {
            before: Some(before),
            after: Some(Arc::new(after)),
        }],
        None,
    )
    .unwrap();
    assert_eq!(
        db.load_slide(1).unwrap().annotations[0].comments[0].text,
        "Mitosis?"
    );
}
