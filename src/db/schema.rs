//! Schema versions and migrations (ANN-16).
//!
//! | `user_version` | Schema |
//! |---|---|
//! | 0 (no tables) | empty database |
//! | 0 with `annotation_layers` | pre-1.0 plugin (called version 1 here) |
//! | 2 | slides, classes, layers, annotations, audit trail |
//! | 3 | current: comments on annotations (a JSON column) |

use super::{DbError, DbResult, OpenReport};
use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 3;

/// Version 1 is the unversioned schema of the pre-1.0 plugin.
pub const LEGACY_VERSION: i64 = 1;

pub(crate) const SCHEMA_V2: &str = r#"
CREATE TABLE slides (
    id INTEGER PRIMARY KEY,
    identity TEXT,
    legacy_fingerprint BLOB CHECK (legacy_fingerprint IS NULL OR length(legacy_fingerprint) = 32),
    fallback_key TEXT,
    filename TEXT NOT NULL DEFAULT '',
    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,
    mpp_x REAL,
    mpp_y REAL,
    vendor TEXT,
    created_at INTEGER NOT NULL,
    last_opened_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX slides_identity ON slides(identity) WHERE identity IS NOT NULL;
CREATE UNIQUE INDEX slides_fingerprint ON slides(legacy_fingerprint) WHERE legacy_fingerprint IS NOT NULL;
CREATE UNIQUE INDEX slides_fallback ON slides(fallback_key) WHERE fallback_key IS NOT NULL;

CREATE TABLE classes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    color TEXT NOT NULL CHECK (length(color) = 7),
    hotkey TEXT,
    visible INTEGER NOT NULL DEFAULT 1,
    opacity REAL NOT NULL DEFAULT 1.0,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX classes_name ON classes(name COLLATE NOCASE) WHERE deleted = 0;

CREATE TABLE layers (
    id TEXT PRIMARY KEY,
    slide_id INTEGER NOT NULL REFERENCES slides(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(name) <= 200),
    notes TEXT NOT NULL DEFAULT '',
    color TEXT NOT NULL CHECK (length(color) = 7),
    visible INTEGER NOT NULL DEFAULT 1,
    opacity REAL NOT NULL DEFAULT 1.0,
    locked INTEGER NOT NULL DEFAULT 0,
    role TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX layers_slide ON layers(slide_id, deleted);

CREATE TABLE annotations (
    id TEXT PRIMARY KEY,
    layer_id TEXT NOT NULL REFERENCES layers(id) ON DELETE CASCADE,
    class_id TEXT REFERENCES classes(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    geometry TEXT NOT NULL,
    min_x REAL NOT NULL,
    min_y REAL NOT NULL,
    max_x REAL NOT NULL,
    max_y REAL NOT NULL,
    geometry_ok INTEGER NOT NULL DEFAULT 1,
    name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'verified', 'rejected')),
    author TEXT NOT NULL DEFAULT '',
    locked INTEGER NOT NULL DEFAULT 0,
    properties TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX annotations_layer ON annotations(layer_id, deleted);
CREATE INDEX annotations_class ON annotations(class_id);

CREATE TABLE slide_state (
    slide_id INTEGER PRIMARY KEY REFERENCES slides(id) ON DELETE CASCADE,
    current_layer TEXT,
    state TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    author TEXT NOT NULL DEFAULT '',
    slide_id INTEGER,
    action TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_slide ON audit_log(slide_id, at);
"#;

/// From version 2 to 3: comments on annotations, oldest first, as JSON
/// (`[{"id", "author", "text", "created_at"}]`), written with the
/// annotation in one transaction like its other fields.
pub(crate) const V2_TO_V3: &str =
    "ALTER TABLE annotations ADD COLUMN comments TEXT NOT NULL DEFAULT '[]';";

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

/// The schema version of a database (0: empty).
pub fn detect_version(conn: &Connection) -> rusqlite::Result<i64> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 && table_exists(conn, "annotation_layers")? {
        return Ok(LEGACY_VERSION);
    }
    Ok(version)
}

/// Brings the database to [`SCHEMA_VERSION`].
///
/// The version is checked again inside an immediate (write-locked)
/// transaction, so two eov windows opening a new or old database at the
/// same time do not both try to create or migrate it.
pub fn migrate(conn: &mut Connection, report: &mut OpenReport) -> DbResult<()> {
    if detect_version(conn)? == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version = detect_version(&tx)?;
    match version {
        SCHEMA_VERSION => {}
        v if v > SCHEMA_VERSION => {
            return Err(DbError::Failed(format!(
                "the database was created by a newer version of the annotations plugin (schema {v}); \
                 update the plugin"
            )));
        }
        0 => {
            tx.execute_batch(SCHEMA_V2)?;
            tx.execute_batch(V2_TO_V3)?;
        }
        LEGACY_VERSION => {
            report.migrated_from = Some(LEGACY_VERSION);
            super::legacy::migrate_v1(&tx, report)?;
            tx.execute_batch(V2_TO_V3)?;
        }
        2 => tx.execute_batch(V2_TO_V3)?,
        other => return Err(DbError::Failed(format!("unknown schema version {other}"))),
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}
