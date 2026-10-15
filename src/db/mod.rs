//! The annotation database (SQLite).
//!
//! - One connection, owned by the database thread, opened once with
//!   `journal_mode = WAL`, `synchronous = NORMAL` and foreign keys on
//!   (ANN-1); every logical edit is one transaction with prepared, cached
//!   statements.
//! - The schema is versioned with `PRAGMA user_version` and upgraded by
//!   migrations, including the unversioned pre-1.0 schema (ANN-16).
//! - A database that cannot be written is opened read-only; an outdated
//!   read-only database is migrated in a private copy (ANN-13).
//! - Deletions are soft (a `deleted` timestamp) so undo never needs to
//!   replay large snapshots; old deleted rows are purged at startup.

mod legacy;
pub mod schema;

use crate::change::Change;
use crate::geometry;
use crate::model::{
    Annotation, Bounds, Class, Color, Comment, Geometry, Layer, LayerRole, SlideId, SlideRecord,
    Status,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, Transaction, params};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use schema::SCHEMA_VERSION;

/// How long a writer waits for another process holding the database.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Soft-deleted rows older than this are purged at startup.
pub const PURGE_AFTER_MS: i64 = 7 * 24 * 3600 * 1000;

/// An error from a database operation.
#[derive(Debug, Clone, PartialEq)]
pub enum DbError {
    /// The data changed since the change was prepared (another window).
    Conflict(String),
    /// The database is read-only.
    ReadOnly,
    /// Anything else (I/O, busy, constraint, corruption).
    Failed(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Conflict(what) => write!(f, "{what} was changed elsewhere in the meantime"),
            DbError::ReadOnly => f.write_str("the annotation database is read-only"),
            DbError::Failed(message) => f.write_str(message),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(e, _) = &error
            && e.code == rusqlite::ErrorCode::ReadOnly
        {
            return DbError::ReadOnly;
        }
        DbError::Failed(error.to_string())
    }
}

pub type DbResult<T> = Result<T, DbError>;

/// Layers restored by [`Db::apply`], with their annotations.
pub type Restored = Vec<(Layer, Vec<Arc<Annotation>>)>;

/// What happened while opening.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenReport {
    /// The schema was upgraded from this version.
    pub migrated_from: Option<i64>,
    /// Human-readable notes (rows skipped during migration, read-only
    /// reason, …).
    pub notes: Vec<String>,
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
    read_only: bool,
    data_version: i64,
}

fn is_read_only_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(e, _)
            if matches!(e.code, rusqlite::ErrorCode::ReadOnly | rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::PermissionDenied)
    )
}

fn configure(conn: &Connection, read_only: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if read_only {
        conn.pragma_update(None, "query_only", "ON")?;
    } else {
        // WAL may be refused on some network file systems; the default
        // rollback journal still works, just slower.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

impl Db {
    /// Opens (creating and migrating when needed) the database at `path`.
    /// `scratch_dir` holds a migrated copy of a read-only database.
    pub fn open(path: &Path, scratch_dir: &Path) -> Result<(Db, OpenReport), String> {
        let mut report = OpenReport::default();
        let exists = path.exists();
        let writable = !exists
            || std::fs::metadata(path)
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false);
        if writable {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            match Connection::open(path) {
                Ok(conn) => match Self::finish_open(conn, path, false, &mut report) {
                    Ok(db) => return Ok((db, report)),
                    Err(Some(error)) => return Err(error),
                    Err(None) => {}
                },
                Err(error) if is_read_only_error(&error) && exists => {}
                Err(error) => {
                    return Err(format!("cannot open {}: {error}", path.display()));
                }
            }
        }
        if !exists {
            return Err(format!("cannot create {}", path.display()));
        }
        // Read-only.
        report
            .notes
            .push("The annotation database is read-only; editing is disabled.".to_string());
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("cannot open {} read-only: {e}", path.display()))?;
        let version = schema::detect_version(&conn).map_err(|e| e.to_string())?;
        if version == SCHEMA_VERSION {
            configure(&conn, true).map_err(|e| e.to_string())?;
            let data_version = query_data_version(&conn);
            return Ok((
                Db {
                    conn,
                    path: path.to_path_buf(),
                    read_only: true,
                    data_version,
                },
                report,
            ));
        }
        // Migrate a private copy (ANN-13: a read-only teaching database must
        // still be viewable).
        drop(conn);
        std::fs::create_dir_all(scratch_dir)
            .map_err(|e| format!("cannot create {}: {e}", scratch_dir.display()))?;
        let copy = scratch_dir.join("read-only-copy.db");
        let _ = std::fs::remove_file(&copy);
        let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        source
            .execute("VACUUM INTO ?1", [copy.to_string_lossy().as_ref()])
            .map_err(|e| format!("cannot copy the read-only database: {e}"))?;
        drop(source);
        let conn = Connection::open(&copy).map_err(|e| e.to_string())?;
        let mut db = Self::finish_open(conn, &copy, false, &mut report)
            .map_err(|e| e.unwrap_or_else(|| "cannot migrate the read-only copy".to_string()))?;
        db.read_only = true;
        db.path = path.to_path_buf();
        db.conn
            .pragma_update(None, "query_only", "ON")
            .map_err(|e| e.to_string())?;
        Ok((db, report))
    }

    /// Configures and migrates a writable connection. `Err(None)` means the
    /// file turned out to be read-only.
    fn finish_open(
        mut conn: Connection,
        path: &Path,
        read_only: bool,
        report: &mut OpenReport,
    ) -> Result<Db, Option<String>> {
        match configure(&conn, read_only) {
            Ok(()) => {}
            Err(error) if is_read_only_error(&error) => return Err(None),
            Err(error) => return Err(Some(format!("cannot configure the database: {error}"))),
        }
        match schema::migrate(&mut conn, report) {
            Ok(()) => {}
            Err(DbError::ReadOnly) => return Err(None),
            Err(error) => {
                return Err(Some(format!(
                    "cannot upgrade the annotation database: {error}"
                )));
            }
        }
        let data_version = query_data_version(&conn);
        Ok(Db {
            conn,
            path: path.to_path_buf(),
            read_only,
            data_version,
        })
    }

    /// An in-memory database (tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Db {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn, false).unwrap();
        schema::migrate(&mut conn, &mut OpenReport::default()).unwrap();
        Db {
            conn,
            path: PathBuf::from(":memory:"),
            read_only: false,
            data_version: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    #[cfg(test)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Whether another connection committed since the last call (ANN-7).
    pub fn changed_externally(&mut self) -> bool {
        let version = query_data_version(&self.conn);
        let changed = version != self.data_version;
        self.data_version = version;
        changed
    }

    fn writable(&self) -> DbResult<()> {
        if self.read_only {
            Err(DbError::ReadOnly)
        } else {
            Ok(())
        }
    }

    // -- settings ------------------------------------------------------------------

    pub fn setting(&self, key: &str) -> DbResult<Option<String>> {
        Ok(self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
            .query_row([key], |r| r.get::<_, String>(0))
            .optional()?)
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> DbResult<()> {
        self.writable()?;
        self.conn
            .prepare_cached(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?
            .execute([key, value])?;
        Ok(())
    }

    // -- classes -------------------------------------------------------------------

    pub fn load_classes(&self) -> DbResult<Vec<Class>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name, color, hotkey, visible, opacity, sort_order, created_at, updated_at
             FROM classes WHERE deleted = 0 ORDER BY sort_order, name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Class {
                id: r.get(0)?,
                name: r.get(1)?,
                color: Color::parse(&r.get::<_, String>(2)?).unwrap_or(Color(0xCC, 0xCC, 0xCC)),
                hotkey: r.get::<_, Option<String>>(3)?.filter(|h| !h.is_empty()),
                visible: r.get(4)?,
                opacity: r.get::<_, f64>(5)?.clamp(0.0, 1.0) as f32,
                sort_order: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // -- slides --------------------------------------------------------------------

    fn slide_from_row(r: &Row<'_>) -> rusqlite::Result<SlideRecord> {
        let fingerprint: Option<Vec<u8>> = r.get(2)?;
        Ok(SlideRecord {
            id: r.get(0)?,
            identity: r.get(1)?,
            legacy_fingerprint: fingerprint.and_then(|v| <[u8; 32]>::try_from(v.as_slice()).ok()),
            fallback_key: r.get(3)?,
            filename: r.get(4)?,
            width: r.get::<_, i64>(5)?.max(0) as u64,
            height: r.get::<_, i64>(6)?.max(0) as u64,
            mpp_x: r.get(7)?,
            mpp_y: r.get(8)?,
            vendor: r.get(9)?,
            created_at: r.get(10)?,
            last_opened_at: r.get(11)?,
        })
    }

    const SLIDE_COLUMNS: &'static str = "id, identity, legacy_fingerprint, fallback_key, filename, width, height, mpp_x, mpp_y, vendor, created_at, last_opened_at";

    /// Finds a slide by identity, then legacy fingerprint, then fallback key.
    pub fn find_slide(
        &self,
        identity: Option<&str>,
        fingerprint: Option<&[u8; 32]>,
        fallback: Option<&str>,
    ) -> DbResult<Option<SlideRecord>> {
        if let Some(identity) = identity {
            let sql = format!(
                "SELECT {} FROM slides WHERE identity = ?1",
                Self::SLIDE_COLUMNS
            );
            if let Some(record) = self
                .conn
                .prepare_cached(&sql)?
                .query_row([identity], Self::slide_from_row)
                .optional()?
            {
                return Ok(Some(record));
            }
        }
        if let Some(fingerprint) = fingerprint {
            let sql = format!(
                "SELECT {} FROM slides WHERE legacy_fingerprint = ?1",
                Self::SLIDE_COLUMNS
            );
            if let Some(record) = self
                .conn
                .prepare_cached(&sql)?
                .query_row([fingerprint.as_slice()], Self::slide_from_row)
                .optional()?
            {
                // A fingerprint match belongs to another identity only if
                // the record already has a different one.
                if identity.is_none() || record.identity.is_none() {
                    return Ok(Some(record));
                }
            }
        }
        if let Some(fallback) = fallback {
            let sql = format!(
                "SELECT {} FROM slides WHERE fallback_key = ?1",
                Self::SLIDE_COLUMNS
            );
            return Ok(self
                .conn
                .prepare_cached(&sql)?
                .query_row([fallback], Self::slide_from_row)
                .optional()?);
        }
        Ok(None)
    }

    /// Whether slides keyed only by the pre-1.0 fingerprint exist (then a
    /// slide without a known identity needs its fingerprint computed).
    pub fn has_unmatched_legacy_slides(&self) -> DbResult<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM slides WHERE identity IS NULL AND legacy_fingerprint IS NOT NULL)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Creates a slide record, or updates the matched one with what is now
    /// known about it (identity, file name, size, spacing).
    pub fn upsert_slide(&mut self, record: &SlideRecord) -> DbResult<SlideRecord> {
        if self.read_only {
            // Nothing to persist; a virtual id keeps the slide usable.
            return Ok(record.clone());
        }
        let fingerprint = record.legacy_fingerprint.map(|f| f.to_vec());
        if record.id > 0 {
            self.conn
                .prepare_cached(
                    "UPDATE slides SET
                        identity = COALESCE(?2, identity),
                        legacy_fingerprint = COALESCE(?3, legacy_fingerprint),
                        fallback_key = COALESCE(fallback_key, ?4),
                        filename = ?5, width = ?6, height = ?7, mpp_x = ?8, mpp_y = ?9,
                        vendor = ?10, last_opened_at = ?11
                     WHERE id = ?1",
                )?
                .execute(params![
                    record.id,
                    record.identity,
                    fingerprint,
                    record.fallback_key,
                    record.filename,
                    record.width as i64,
                    record.height as i64,
                    record.mpp_x,
                    record.mpp_y,
                    record.vendor,
                    record.last_opened_at,
                ])?;
            let sql = format!("SELECT {} FROM slides WHERE id = ?1", Self::SLIDE_COLUMNS);
            return Ok(self
                .conn
                .prepare_cached(&sql)?
                .query_row([record.id], Self::slide_from_row)?);
        }
        self.conn
            .prepare_cached(
                "INSERT INTO slides (identity, legacy_fingerprint, fallback_key, filename, width, height,
                                     mpp_x, mpp_y, vendor, created_at, last_opened_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?
            .execute(params![
                record.identity,
                fingerprint,
                record.fallback_key,
                record.filename,
                record.width as i64,
                record.height as i64,
                record.mpp_x,
                record.mpp_y,
                record.vendor,
                record.created_at,
                record.last_opened_at,
            ])?;
        let mut created = record.clone();
        created.id = self.conn.last_insert_rowid();
        Ok(created)
    }

    /// Slides with annotations, for the "other slides" (relink) list.
    pub fn slide_summaries(&self) -> DbResult<Vec<SlideSummary>> {
        let sql = format!(
            "SELECT {}, (SELECT COUNT(*) FROM annotations a JOIN layers l ON a.layer_id = l.id
                          WHERE l.slide_id = s.id AND l.deleted = 0 AND a.deleted = 0),
                       (SELECT COUNT(*) FROM layers l WHERE l.slide_id = s.id AND l.deleted = 0)
             FROM slides s ORDER BY last_opened_at DESC",
            Self::SLIDE_COLUMNS
                .split(", ")
                .map(|c| format!("s.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(SlideSummary {
                record: Self::slide_from_row(r)?,
                annotations: r.get::<_, i64>(12)?.max(0) as usize,
                layers: r.get::<_, i64>(13)?.max(0) as usize,
            })
        })?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|s| s.layers > 0)
            .collect())
    }

    // -- slide contents ------------------------------------------------------------

    /// Loads the live layers and annotations of a slide. Annotations whose
    /// geometry cannot be read are skipped and counted; polygons from older
    /// versions are checked once for self-intersections.
    pub fn load_slide(&mut self, slide_id: SlideId) -> DbResult<LoadedSlide> {
        let layers = self.load_layers(slide_id)?;
        let mut annotations = Vec::new();
        let mut skipped = 0usize;
        let mut unchecked: Vec<String> = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT a.id, a.layer_id, a.class_id, a.geometry, a.name, a.description, a.status,
                        a.author, a.locked, a.properties, a.created_at, a.updated_at, a.geometry_ok, a.comments
                 FROM annotations a JOIN layers l ON a.layer_id = l.id
                 WHERE l.slide_id = ?1 AND l.deleted = 0 AND a.deleted = 0
                 ORDER BY a.created_at, a.id",
            )?;
            let mut rows = stmt.query([slide_id])?;
            while let Some(row) = rows.next()? {
                match annotation_from_row(row) {
                    Ok(annotation) => {
                        if row.get::<_, i64>(12)? == 0 {
                            unchecked.push(annotation.id.clone());
                        }
                        annotations.push(Arc::new(annotation));
                    }
                    Err(_) => skipped += 1,
                }
            }
        }
        let mut invalid = HashSet::new();
        if !unchecked.is_empty() {
            let unchecked: HashSet<&str> = unchecked.iter().map(String::as_str).collect();
            let mut now_ok = Vec::new();
            for a in &annotations {
                if !unchecked.contains(a.id.as_str()) {
                    continue;
                }
                let valid = match &a.geometry {
                    Geometry::Polygon { exterior, holes } => {
                        geometry::polygon_validity(exterior, holes) == geometry::Validity::Valid
                    }
                    _ => true,
                };
                if valid {
                    now_ok.push(a.id.clone());
                } else {
                    invalid.insert(a.id.clone());
                }
            }
            if !self.read_only {
                let tx = self.conn.transaction()?;
                {
                    let mut stmt =
                        tx.prepare_cached("UPDATE annotations SET geometry_ok = 1 WHERE id = ?1")?;
                    for id in &now_ok {
                        stmt.execute([id])?;
                    }
                }
                tx.commit()?;
            }
        }
        let (current_layer, view_state) = self
            .conn
            .prepare_cached("SELECT current_layer, state FROM slide_state WHERE slide_id = ?1")?
            .query_row([slide_id], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
            .unwrap_or_default();
        Ok(LoadedSlide {
            layers,
            annotations,
            invalid,
            skipped,
            current_layer,
            collapsed: collapsed_from_state(&view_state),
        })
    }

    /// Remembers which of a slide's layers the user collapsed or expanded
    /// (ANN-18), in `slide_state.state` next to any other keys there.
    pub fn set_collapsed_layers(
        &mut self,
        slide_id: SlideId,
        collapsed: &HashMap<String, bool>,
    ) -> DbResult<()> {
        self.writable()?;
        let tx = self.conn.transaction()?;
        let state: Option<String> = tx
            .prepare_cached("SELECT state FROM slide_state WHERE slide_id = ?1")?
            .query_row([slide_id], |r| r.get(0))
            .optional()?;
        let mut object = state
            .and_then(|state| serde_json::from_str::<serde_json::Map<String, Value>>(&state).ok())
            .unwrap_or_default();
        let collapsed: BTreeMap<&String, &bool> = collapsed.iter().collect();
        object.insert(COLLAPSED_KEY.into(), serde_json::json!(collapsed));
        tx.prepare_cached(
            "INSERT INTO slide_state (slide_id, state) VALUES (?1, ?2)
             ON CONFLICT(slide_id) DO UPDATE SET state = excluded.state",
        )?
        .execute(params![slide_id, Value::Object(object).to_string()])?;
        tx.commit()?;
        Ok(())
    }

    fn load_layers(&self, slide_id: SlideId) -> DbResult<Vec<Layer>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, slide_id, name, notes, color, visible, opacity, locked, role, sort_order,
                    created_at, updated_at
             FROM layers WHERE slide_id = ?1 AND deleted = 0
             ORDER BY sort_order, name COLLATE NOCASE, created_at",
        )?;
        let rows = stmt.query_map([slide_id], layer_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Ids of the live layers of a slide.
    pub fn layer_ids(&self, slide_id: SlideId) -> DbResult<Vec<String>> {
        Ok(self
            .load_layers(slide_id)?
            .into_iter()
            .map(|l| l.id)
            .collect())
    }

    /// Every slide with its contents (for archives).
    pub fn all_slides(&mut self) -> DbResult<Vec<(SlideRecord, LoadedSlide)>> {
        let sql = format!("SELECT {} FROM slides ORDER BY id", Self::SLIDE_COLUMNS);
        let records: Vec<SlideRecord> = self
            .conn
            .prepare(&sql)?
            .query_map([], Self::slide_from_row)?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        for record in records {
            let loaded = self.load_slide(record.id)?;
            if !loaded.layers.is_empty() {
                out.push((record, loaded));
            }
        }
        Ok(out)
    }

    /// Remembers the layer new annotations go to (ANN-18).
    pub fn set_current_layer(&mut self, slide_id: SlideId, layer_id: Option<&str>) -> DbResult<()> {
        self.writable()?;
        self.conn
            .prepare_cached(
                "INSERT INTO slide_state (slide_id, current_layer) VALUES (?1, ?2)
                 ON CONFLICT(slide_id) DO UPDATE SET current_layer = excluded.current_layer",
            )?
            .execute(params![slide_id, layer_id])?;
        Ok(())
    }

    // -- changes -------------------------------------------------------------------

    /// Applies changes atomically (all or nothing), checking that each one
    /// still fits the stored data, and records them in the audit trail.
    pub fn apply(&mut self, changes: &[Change], audit: Option<&Audit>) -> DbResult<Restored> {
        self.writable()?;
        let now = crate::model::now_ms();
        let tx = self.conn.transaction()?;
        let mut moved = Vec::new();
        for change in changes {
            apply_change(&tx, change, now, &mut moved)?;
        }
        if let Some(audit) = audit {
            tx.prepare_cached(
                "INSERT INTO audit_log (at, author, slide_id, action, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![now, audit.author, audit.slide_id, audit.action, audit.detail])?;
        }
        tx.commit()?;
        Ok(moved)
    }

    /// Permanently removes rows soft-deleted before `older_than` (ms).
    pub fn purge_deleted(&mut self, older_than: i64) -> DbResult<usize> {
        if self.read_only {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut removed = tx.execute(
            "DELETE FROM annotations WHERE deleted != 0 AND deleted < ?1",
            [older_than],
        )?;
        removed += tx.execute(
            "DELETE FROM layers WHERE deleted != 0 AND deleted < ?1",
            [older_than],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    /// Writes a consistent copy of the database to `target`.
    pub fn backup_to(&self, target: &Path) -> DbResult<()> {
        if target.exists() {
            std::fs::remove_file(target).map_err(|e| DbError::Failed(e.to_string()))?;
        }
        self.conn
            .execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// Merges another annotation database (a backup, or a pre-1.0
    /// `annotations.db`) into this one. Existing rows win; returns
    /// `(slides, layers, annotations)` added.
    pub fn merge_from(
        &mut self,
        source: &Path,
        scratch_dir: &Path,
    ) -> DbResult<(usize, usize, usize)> {
        self.writable()?;
        // Work on a migrated copy so the source is never modified.
        std::fs::create_dir_all(scratch_dir).map_err(|e| DbError::Failed(e.to_string()))?;
        let copy = scratch_dir.join("merge-source.db");
        let _ = std::fs::remove_file(&copy);
        {
            let src = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| DbError::Failed(format!("cannot open {}: {e}", source.display())))?;
            schema::detect_version(&src)
                .map_err(|e| DbError::Failed(format!("not an annotation database: {e}")))?;
            src.execute("VACUUM INTO ?1", [copy.to_string_lossy().as_ref()])?;
        }
        {
            let mut conn = Connection::open(&copy)?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            let mut report = OpenReport::default();
            schema::migrate(&mut conn, &mut report)?;
        }
        let result = (|| -> DbResult<(usize, usize, usize)> {
            self.conn.execute(
                "ATTACH DATABASE ?1 AS src",
                [copy.to_string_lossy().as_ref()],
            )?;
            let tx = self.conn.transaction()?;
            // Slides: match by identity or fingerprint; otherwise copy.
            let before_slides: i64 =
                tx.query_row("SELECT COUNT(*) FROM slides", [], |r| r.get(0))?;
            tx.execute_batch(
                "CREATE TEMP TABLE slide_map (src_id INTEGER PRIMARY KEY, dst_id INTEGER);
                 INSERT INTO slide_map (src_id, dst_id)
                   SELECT s.id, (SELECT d.id FROM main.slides d
                                 WHERE (s.identity IS NOT NULL AND d.identity = s.identity)
                                    OR (s.legacy_fingerprint IS NOT NULL AND d.legacy_fingerprint = s.legacy_fingerprint)
                                    OR (s.fallback_key IS NOT NULL AND d.fallback_key = s.fallback_key)
                                 LIMIT 1)
                   FROM src.slides s;",
            )?;
            {
                let mut unmatched = tx.prepare(
                    "SELECT s.id, s.identity, s.legacy_fingerprint, s.fallback_key, s.filename, s.width, s.height,
                            s.mpp_x, s.mpp_y, s.vendor, s.created_at, s.last_opened_at
                     FROM src.slides s JOIN slide_map m ON m.src_id = s.id WHERE m.dst_id IS NULL",
                )?;
                let rows: Vec<(i64, SlideRecord)> = unmatched
                    .query_map([], |r| Ok((r.get(0)?, Self::slide_from_row(r)?)))?
                    .collect::<Result<_, _>>()?;
                for (src_id, record) in rows {
                    tx.execute(
                        "INSERT INTO main.slides (identity, legacy_fingerprint, fallback_key, filename, width, height,
                                                  mpp_x, mpp_y, vendor, created_at, last_opened_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            record.identity,
                            record.legacy_fingerprint.map(|f| f.to_vec()),
                            record.fallback_key,
                            record.filename,
                            record.width as i64,
                            record.height as i64,
                            record.mpp_x,
                            record.mpp_y,
                            record.vendor,
                            record.created_at,
                            record.last_opened_at
                        ],
                    )?;
                    let dst = tx.last_insert_rowid();
                    tx.execute(
                        "UPDATE slide_map SET dst_id = ?2 WHERE src_id = ?1",
                        [src_id, dst],
                    )?;
                }
            }
            let after_slides: i64 =
                tx.query_row("SELECT COUNT(*) FROM slides", [], |r| r.get(0))?;
            tx.execute(
                "INSERT OR IGNORE INTO main.classes
                   SELECT c.* FROM src.classes c
                   WHERE c.deleted = 0 AND NOT EXISTS (
                       SELECT 1 FROM main.classes d WHERE d.deleted = 0 AND d.name = c.name COLLATE NOCASE)",
                [],
            )?;
            let layers = tx.execute(
                "INSERT OR IGNORE INTO main.layers
                   (id, slide_id, name, notes, color, visible, opacity, locked, role, sort_order, created_at, updated_at, deleted)
                 SELECT l.id, m.dst_id, l.name, l.notes, l.color, l.visible, l.opacity, l.locked, l.role, l.sort_order,
                        l.created_at, l.updated_at, 0
                 FROM src.layers l JOIN slide_map m ON m.src_id = l.slide_id WHERE l.deleted = 0",
                [],
            )?;
            let annotations = tx.execute(
                "INSERT OR IGNORE INTO main.annotations
                 SELECT a.id, a.layer_id,
                        CASE WHEN a.class_id IS NULL THEN NULL
                             ELSE COALESCE((SELECT d.id FROM main.classes d WHERE d.id = a.class_id),
                                           (SELECT d.id FROM main.classes d JOIN src.classes c ON c.id = a.class_id
                                            WHERE d.deleted = 0 AND d.name = c.name COLLATE NOCASE LIMIT 1)) END,
                        a.kind, a.geometry, a.min_x, a.min_y, a.max_x, a.max_y, a.geometry_ok, a.name, a.description,
                        a.status, a.author, a.locked, a.properties, a.created_at, a.updated_at, 0, a.comments
                 FROM src.annotations a JOIN main.layers l ON l.id = a.layer_id
                 WHERE a.deleted = 0",
                [],
            )?;
            tx.execute_batch("DROP TABLE slide_map;")?;
            tx.commit()?;
            Ok((
                ((after_slides - before_slides).max(0)) as usize,
                layers,
                annotations,
            ))
        })();
        let _ = self.conn.execute("DETACH DATABASE src", []);
        let _ = std::fs::remove_file(&copy);
        result
    }
}

/// An audit-trail entry.
#[derive(Debug, Clone, Default)]
pub struct Audit {
    pub author: String,
    pub slide_id: Option<SlideId>,
    pub action: String,
    pub detail: String,
}

/// A slide's contents as loaded.
#[derive(Debug, Default)]
pub struct LoadedSlide {
    pub layers: Vec<Layer>,
    pub annotations: Vec<Arc<Annotation>>,
    /// Polygons whose outline crosses itself (from before validation).
    pub invalid: HashSet<String>,
    /// Rows that could not be read.
    pub skipped: usize,
    pub current_layer: Option<String>,
    /// Layers the user collapsed (`true`) or expanded (`false`).
    pub collapsed: HashMap<String, bool>,
}

/// The key of the collapsed layers in `slide_state.state`.
const COLLAPSED_KEY: &str = "collapsed";

fn collapsed_from_state(state: &str) -> HashMap<String, bool> {
    serde_json::from_str::<Value>(state)
        .ok()
        .and_then(|state| {
            let layers = state.get(COLLAPSED_KEY)?.as_object()?.clone();
            Some(
                layers
                    .into_iter()
                    .filter_map(|(id, collapsed)| Some((id, collapsed.as_bool()?)))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct SlideSummary {
    pub record: SlideRecord,
    pub annotations: usize,
    pub layers: usize,
}

fn query_data_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap_or(0)
}

fn layer_from_row(r: &Row<'_>) -> rusqlite::Result<Layer> {
    Ok(Layer {
        id: r.get(0)?,
        slide_id: r.get(1)?,
        name: r.get(2)?,
        notes: r.get(3)?,
        color: Color::parse(&r.get::<_, String>(4)?).unwrap_or(Color(0xCC, 0xCC, 0xCC)),
        visible: r.get(5)?,
        opacity: r.get::<_, f64>(6)?.clamp(0.0, 1.0) as f32,
        locked: r.get(7)?,
        role: LayerRole::parse(&r.get::<_, String>(8)?),
        sort_order: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// Reads an annotation row; a row whose geometry or properties are not
/// readable is an error (the caller skips it) rather than a panic (ANN-4).
fn annotation_from_row(r: &Row<'_>) -> Result<Annotation, String> {
    let geometry_json: String = r.get(3).map_err(|e| e.to_string())?;
    let geometry: Geometry = serde_json::from_str(&geometry_json).map_err(|e| e.to_string())?;
    if !geometry.is_finite() {
        return Err("non-finite coordinates".to_string());
    }
    let properties_json: String = r.get(9).map_err(|e| e.to_string())?;
    let properties: Vec<(String, String)> =
        serde_json::from_str(&properties_json).unwrap_or_default();
    // Unreadable comments are dropped rather than losing the annotation.
    let comments: Vec<Comment> = r
        .get::<_, String>(13)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    Ok(Annotation {
        id: r.get(0).map_err(|e| e.to_string())?,
        layer_id: r.get(1).map_err(|e| e.to_string())?,
        class_id: r.get(2).map_err(|e| e.to_string())?,
        geometry,
        name: r.get(4).map_err(|e| e.to_string())?,
        description: r.get(5).map_err(|e| e.to_string())?,
        status: Status::parse(&r.get::<_, String>(6).map_err(|e| e.to_string())?)
            .unwrap_or_default(),
        author: r.get(7).map_err(|e| e.to_string())?,
        locked: r.get(8).map_err(|e| e.to_string())?,
        properties,
        comments,
        created_at: r.get(10).map_err(|e| e.to_string())?,
        updated_at: r.get(11).map_err(|e| e.to_string())?,
    })
}

/// The stored version of a row: `(updated_at, deleted)`.
fn row_version(tx: &Transaction<'_>, table: &str, id: &str) -> DbResult<Option<(i64, i64)>> {
    let sql = format!("SELECT updated_at, deleted FROM {table} WHERE id = ?1");
    Ok(tx
        .prepare_cached(&sql)?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

/// Checks that the stored row matches what a change expects.
fn expect(
    tx: &Transaction<'_>,
    table: &str,
    id: &str,
    what: &str,
    live_version: Option<i64>,
) -> DbResult<()> {
    let stored = row_version(tx, table, id)?;
    let ok = match (live_version, stored) {
        // Expect a live row with this version.
        (Some(version), Some((updated_at, 0))) => updated_at == version,
        (Some(_), _) => false,
        // Expect no live row.
        (None, Some((_, deleted))) => deleted != 0,
        (None, None) => true,
    };
    if ok {
        Ok(())
    } else {
        Err(DbError::Conflict(what.to_string()))
    }
}

fn write_annotation(tx: &Transaction<'_>, a: &Annotation) -> DbResult<()> {
    let geometry =
        serde_json::to_string(&a.geometry).map_err(|e| DbError::Failed(e.to_string()))?;
    let properties =
        serde_json::to_string(&a.properties).map_err(|e| DbError::Failed(e.to_string()))?;
    let comments =
        serde_json::to_string(&a.comments).map_err(|e| DbError::Failed(e.to_string()))?;
    let Bounds {
        min_x,
        min_y,
        max_x,
        max_y,
    } = a.geometry.bounds();
    tx.prepare_cached(
        "INSERT INTO annotations (id, layer_id, class_id, kind, geometry, min_x, min_y, max_x, max_y, geometry_ok,
                                  name, description, status, author, locked, properties, created_at, updated_at, deleted,
                                  comments)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 0, ?18)
         ON CONFLICT(id) DO UPDATE SET
            layer_id = excluded.layer_id, class_id = excluded.class_id, kind = excluded.kind,
            geometry = excluded.geometry, min_x = excluded.min_x, min_y = excluded.min_y,
            max_x = excluded.max_x, max_y = excluded.max_y, geometry_ok = 1, name = excluded.name,
            description = excluded.description, status = excluded.status, author = excluded.author,
            locked = excluded.locked, properties = excluded.properties, created_at = excluded.created_at,
            updated_at = excluded.updated_at, deleted = 0, comments = excluded.comments",
    )?
    .execute(params![
        a.id,
        a.layer_id,
        a.class_id,
        a.geometry.kind().as_str(),
        geometry,
        min_x,
        min_y,
        max_x,
        max_y,
        a.name,
        a.description,
        a.status.as_str(),
        a.author,
        a.locked,
        properties,
        a.created_at,
        a.updated_at,
        comments,
    ])?;
    Ok(())
}

fn write_layer(tx: &Transaction<'_>, l: &Layer) -> DbResult<()> {
    tx.prepare_cached(
        "INSERT INTO layers (id, slide_id, name, notes, color, visible, opacity, locked, role, sort_order,
                             created_at, updated_at, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0)
         ON CONFLICT(id) DO UPDATE SET
            slide_id = excluded.slide_id, name = excluded.name, notes = excluded.notes,
            color = excluded.color, visible = excluded.visible, opacity = excluded.opacity,
            locked = excluded.locked, role = excluded.role, sort_order = excluded.sort_order,
            created_at = excluded.created_at, updated_at = excluded.updated_at, deleted = 0",
    )?
    .execute(params![
        l.id,
        l.slide_id,
        l.name,
        l.notes,
        l.color.to_hex(),
        l.visible,
        f64::from(l.opacity),
        l.locked,
        l.role.as_str(),
        l.sort_order,
        l.created_at,
        l.updated_at,
    ])?;
    Ok(())
}

fn write_class(tx: &Transaction<'_>, c: &Class) -> DbResult<()> {
    tx.prepare_cached(
        "INSERT INTO classes (id, name, color, hotkey, visible, opacity, sort_order, created_at, updated_at, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name, color = excluded.color, hotkey = excluded.hotkey,
            visible = excluded.visible, opacity = excluded.opacity, sort_order = excluded.sort_order,
            created_at = excluded.created_at, updated_at = excluded.updated_at, deleted = 0",
    )?
    .execute(params![
        c.id,
        c.name,
        c.color.to_hex(),
        c.hotkey,
        c.visible,
        f64::from(c.opacity),
        c.sort_order,
        c.created_at,
        c.updated_at,
    ])?;
    Ok(())
}

fn soft_delete(tx: &Transaction<'_>, table: &str, id: &str, now: i64) -> DbResult<()> {
    let sql = format!("UPDATE {table} SET deleted = ?2 WHERE id = ?1");
    tx.prepare_cached(&sql)?.execute(params![id, now.max(1)])?;
    Ok(())
}

fn apply_change(
    tx: &Transaction<'_>,
    change: &Change,
    now: i64,
    moved: &mut Vec<(Layer, Vec<Arc<Annotation>>)>,
) -> DbResult<()> {
    match change {
        Change::Annotation { before, after } => {
            let id = after
                .as_ref()
                .or(before.as_ref())
                .map(|a| a.id.clone())
                .unwrap_or_default();
            expect(
                tx,
                "annotations",
                &id,
                "the annotation",
                before.as_ref().map(|b| b.updated_at),
            )?;
            match after {
                Some(after) => write_annotation(tx, after)?,
                None => soft_delete(tx, "annotations", &id, now)?,
            }
        }
        Change::Layer { before, after } => {
            let id = after
                .as_ref()
                .or(before.as_ref())
                .map(|l| l.id.clone())
                .unwrap_or_default();
            expect(
                tx,
                "layers",
                &id,
                "the layer",
                before.as_ref().map(|b| b.updated_at),
            )?;
            match after {
                Some(after) => write_layer(tx, after)?,
                None => soft_delete(tx, "layers", &id, now)?,
            }
        }
        Change::RemoveLayer { layer, .. } => {
            expect(tx, "layers", &layer.id, "the layer", Some(layer.updated_at))?;
            soft_delete(tx, "layers", &layer.id, now)?;
        }
        Change::RestoreLayer { layer, contents } => {
            expect(tx, "layers", &layer.id, "the layer", None)?;
            write_layer(tx, layer)?;
            for annotation in contents {
                write_annotation(tx, annotation)?;
            }
        }
        Change::Class { before, after } => {
            let id = after
                .as_ref()
                .or(before.as_ref())
                .map(|c| c.id.clone())
                .unwrap_or_default();
            expect(
                tx,
                "classes",
                &id,
                "the class",
                before.as_ref().map(|b| b.updated_at),
            )?;
            match after {
                Some(after) => write_class(tx, after)?,
                None => soft_delete(tx, "classes", &id, now)?,
            }
        }
        Change::MoveLayers {
            from,
            to,
            layer_ids,
        } => {
            for id in layer_ids {
                let updated = tx
                    .prepare_cached(
                        "UPDATE layers SET slide_id = ?3 WHERE id = ?1 AND slide_id = ?2 AND deleted = 0",
                    )?
                    .execute(params![id, from, to])?;
                if updated != 1 {
                    return Err(DbError::Conflict("a layer being moved".to_string()));
                }
                let layer = tx
                    .prepare_cached(
                        "SELECT id, slide_id, name, notes, color, visible, opacity, locked, role, sort_order,
                                created_at, updated_at FROM layers WHERE id = ?1",
                    )?
                    .query_row([id], layer_from_row)?;
                let mut contents = Vec::new();
                let mut stmt = tx.prepare_cached(
                    "SELECT id, layer_id, class_id, geometry, name, description, status, author, locked,
                            properties, created_at, updated_at, geometry_ok, comments
                     FROM annotations WHERE layer_id = ?1 AND deleted = 0 ORDER BY created_at, id",
                )?;
                let mut rows = stmt.query([id])?;
                while let Some(row) = rows.next()? {
                    if let Ok(a) = annotation_from_row(row) {
                        contents.push(Arc::new(a));
                    }
                }
                moved.push((layer, contents));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
