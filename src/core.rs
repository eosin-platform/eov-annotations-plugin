//! The plugin core: state, worker threads and the edit pipeline.
//!
//! Threads:
//!
//! - the host's UI thread delivers events; handlers only read state and
//!   queue work, so they stay short (ANN-1, ANN-14);
//! - the database thread owns the SQLite connection and executes edits in
//!   order: plan (against the current state) → one transaction → memory →
//!   host overlays → undo entry (memory changes only after a successful
//!   commit);
//! - the I/O thread hashes slides, reads imports and writes exports.
//!
//! Nothing here holds the state lock while calling the host with large
//! payloads, and no handler waits for a worker.

use crate::change::{self, Change};
use crate::db::{Audit, Db, DbError, PURGE_AFTER_MS};
use crate::fileio;
use crate::model::{Class, Color, SlideId, SlideRecord, new_id, now_ms};
use crate::overlay;
use crate::state::{DbStatus, Load, OpenFile, Record, SlideData, State};
use eov_plugin_api::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Token of the coalesced sidebar refresh.
pub const REFRESH_TOKEN: &str = "refresh";
/// Token asking the UI thread to sync the class toolbar buttons.
pub const PALETTE_TOKEN: &str = "palette";

/// A change to make, planned on the database thread against the state as
/// it is after every earlier edit (so rapid edits never work from stale
/// data).
pub struct Op {
    pub label: String,
    /// The file the edit came from (scopes Ctrl+Z to its slide).
    pub file_id: Option<FileId>,
    /// Push an undo entry.
    pub record: bool,
    pub plan: Plan,
}

/// Turns the current state into the changes to apply (on the database
/// thread).
pub type Plan = Box<dyn FnOnce(&State) -> Result<Planned, String> + Send>;

impl Op {
    pub fn new(
        label: impl Into<String>,
        file_id: Option<FileId>,
        plan: impl FnOnce(&State) -> Result<Planned, String> + Send + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            file_id,
            record: true,
            plan: Box::new(plan),
        }
    }

    pub fn without_history(mut self) -> Self {
        self.record = false;
        self
    }
}

/// The result of planning an edit.
#[derive(Default)]
pub struct Planned {
    pub changes: Vec<Change>,
    /// Replace the selection of a file after the edit.
    pub select: Option<(FileId, Vec<String>)>,
    /// Remember the target layer of a slide.
    pub current_layer: Option<(SlideId, String)>,
    /// Shown as an info toast after success.
    pub notice: Option<String>,
    /// Overrides the undo label.
    pub label: Option<String>,
    /// Annotation ids whose host objects must be restored if nothing is
    /// saved (the host already showed an edit that was refused or failed).
    pub revert: Option<(SlideId, Vec<String>)>,
    /// Shown as a warning (e.g. why an edit was refused).
    pub warning: Option<String>,
}

impl Planned {
    pub fn changes(changes: Vec<Change>) -> Self {
        Self {
            changes,
            ..Default::default()
        }
    }
}

/// Whether the slide fingerprint was computed yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fingerprint {
    NotComputed,
    Computed(Option<[u8; 32]>),
}

type DbTask = Box<dyn FnOnce(&Arc<Core>, &mut DbCtx) + Send>;
type IoTask = Box<dyn FnOnce(&Arc<Core>) + Send>;

pub enum DbJob {
    Open,
    Load {
        file_id: FileId,
        fingerprint: Fingerprint,
    },
    Op(Op),
    History {
        entry_id: String,
        redo: bool,
    },
    Run(DbTask),
    Barrier(Sender<()>),
    Stop,
}

enum IoJob {
    Run(IoTask),
    Barrier(Sender<()>),
    Stop,
}

/// What the database thread owns.
pub struct DbCtx {
    pub db: Option<Db>,
}

/// Where things live.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub db_path: PathBuf,
}

pub struct Core {
    pub host: Host,
    pub paths: Paths,
    pub plugin_version: String,
    state: Mutex<State>,
    db_tx: Mutex<Option<Sender<DbJob>>>,
    io_tx: Mutex<Option<Sender<IoJob>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    refresh_pending: AtomicBool,
}

impl Core {
    pub fn start(host: Host, paths: Paths, plugin_version: String, state: State) -> Arc<Core> {
        let (db_tx, db_rx) = mpsc::channel();
        let (io_tx, io_rx) = mpsc::channel();
        let core = Arc::new(Core {
            host,
            paths,
            plugin_version,
            state: Mutex::new(state),
            db_tx: Mutex::new(Some(db_tx)),
            io_tx: Mutex::new(Some(io_tx)),
            threads: Mutex::new(Vec::new()),
            refresh_pending: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&core);
        let db_thread = std::thread::Builder::new()
            .name("annotations-db".into())
            .spawn(move || db_loop(weak, db_rx));
        let weak = Arc::downgrade(&core);
        let io_thread = std::thread::Builder::new()
            .name("annotations-io".into())
            .spawn(move || io_loop(weak, io_rx));
        let mut threads = core.threads.lock().unwrap_or_else(|e| e.into_inner());
        for thread in [db_thread, io_thread] {
            match thread {
                Ok(handle) => threads.push(handle),
                Err(e) => core
                    .host
                    .error(format!("cannot start a worker thread: {e}")),
            }
        }
        drop(threads);
        core.db(DbJob::Open);
        core
    }

    /// The state (a panic elsewhere never wedges it).
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn db(&self, job: DbJob) {
        let sender = self.db_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(sender) = sender {
            let _ = sender.send(job);
        }
    }

    pub fn db_run(&self, task: impl FnOnce(&Arc<Core>, &mut DbCtx) + Send + 'static) {
        self.db(DbJob::Run(Box::new(task)));
    }

    pub fn io(&self, task: impl FnOnce(&Arc<Core>) + Send + 'static) {
        let sender = self.io_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(sender) = sender {
            let _ = sender.send(IoJob::Run(Box::new(task)));
        }
    }

    pub fn submit(&self, op: Op) {
        self.db(DbJob::Op(op));
    }

    /// Asks for one sidebar refresh on the UI thread (coalesced).
    pub fn request_refresh(&self) {
        if !self.refresh_pending.swap(true, Ordering::AcqRel) {
            let _ = self.host.post(REFRESH_TOKEN);
        }
    }

    /// Called when the refresh token arrives.
    pub fn refresh_delivered(&self) {
        self.refresh_pending.store(false, Ordering::Release);
    }

    /// Waits until both workers are idle (tests; never on the UI thread of
    /// a real host).
    pub fn sync(&self) {
        for _ in 0..3 {
            let (tx, rx) = mpsc::channel();
            self.db(DbJob::Barrier(tx));
            let _ = rx.recv_timeout(Duration::from_secs(30));
            let (tx, rx) = mpsc::channel();
            let sender = self.io_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(sender) = sender {
                let _ = sender.send(IoJob::Barrier(tx));
                let _ = rx.recv_timeout(Duration::from_secs(30));
            }
        }
    }

    /// Stops the workers after they finished queued work (pending writes
    /// are flushed).
    pub fn stop(&self) {
        if let Some(sender) = self.db_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = sender.send(DbJob::Stop);
        }
        if let Some(sender) = self.io_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = sender.send(IoJob::Stop);
        }
        let threads: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.threads.lock().unwrap_or_else(|e| e.into_inner()));
        for thread in threads {
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }

    /// Performs host overlay calls; failures are logged.
    pub fn exec(&self, ops: Vec<overlay::HostOp>) {
        for error in overlay::exec(&self.host, ops) {
            self.host.warn(format!("overlay update failed: {error}"));
        }
    }

    pub fn notify(&self, level: NotifyLevel, message: impl Into<String>) {
        self.host.notify(level, message);
    }

    pub fn error(&self, message: impl Into<String>, detail: impl Into<String>) {
        self.host.notify_error(message, detail);
    }
}

fn guarded(core: &Arc<Core>, what: &str, f: impl FnOnce()) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".to_string());
        core.error(format!("Internal error while {what}"), message);
    }
}

fn db_loop(core: Weak<Core>, rx: Receiver<DbJob>) {
    let mut ctx = DbCtx { db: None };
    while let Ok(job) = rx.recv() {
        let Some(core) = core.upgrade() else {
            break;
        };
        match job {
            DbJob::Stop => break,
            DbJob::Barrier(done) => {
                let _ = done.send(());
            }
            DbJob::Open => guarded(&core, "opening the database", || open_db(&core, &mut ctx)),
            DbJob::Load {
                file_id,
                fingerprint,
            } => guarded(&core, "loading annotations", || {
                load_file(&core, &mut ctx, file_id, fingerprint)
            }),
            DbJob::Op(op) => guarded(&core, "saving annotations", || run_op(&core, &mut ctx, op)),
            DbJob::History { entry_id, redo } => guarded(&core, "undoing", || {
                run_history(&core, &mut ctx, &entry_id, redo)
            }),
            DbJob::Run(task) => guarded(&core, "working on the database", || task(&core, &mut ctx)),
        }
    }
}

fn io_loop(core: Weak<Core>, rx: Receiver<IoJob>) {
    while let Ok(job) = rx.recv() {
        let Some(core) = core.upgrade() else {
            break;
        };
        match job {
            IoJob::Stop => break,
            IoJob::Barrier(done) => {
                let _ = done.send(());
            }
            IoJob::Run(task) => guarded(&core, "reading or writing a file", || task(&core)),
        }
    }
}

/// Shown in the Data tab with a new, empty database.
pub const LEGACY_DATABASE_HINT: &str = "New database. If you annotated with eov before 1.0, those annotations are \
     in annotations.db in eov's old settings folder (~/.config/eov on Linux, ~/Library/Application Support/eov \
     on macOS, %APPDATA%\\eov on Windows): bring them in with Merge… above.";

/// The classes a new database starts with.
const DEFAULT_CLASSES: &[(&str, &str)] = &[
    ("Tumour", "#D55E00"),
    ("Stroma", "#56B4E9"),
    ("Necrosis", "#9B9B9B"),
    ("Immune cells", "#009E73"),
    ("Mitosis", "#CC79A7"),
    ("Positive", "#8B4513"),
    ("Negative", "#0072B2"),
    ("Other", "#F0E442"),
];

fn open_db(core: &Arc<Core>, ctx: &mut DbCtx) {
    ctx.db = None;
    let scratch = core.paths.cache_dir.join("db");
    match Db::open(&core.paths.db_path, &scratch) {
        Ok((mut db, report)) => {
            let read_only = db.is_read_only();
            let mut classes = db.load_classes().unwrap_or_default();
            let fresh = classes.is_empty() && !read_only;
            if fresh {
                let now = now_ms();
                let defaults: Vec<Change> = DEFAULT_CLASSES
                    .iter()
                    .enumerate()
                    .map(|(i, (name, color))| Change::Class {
                        before: None,
                        after: Some(Class {
                            id: new_id(),
                            name: name.to_string(),
                            color: Color::parse(color).unwrap_or_default(),
                            hotkey: None,
                            visible: true,
                            opacity: 1.0,
                            sort_order: i as i64,
                            created_at: now,
                            updated_at: now,
                        }),
                    })
                    .collect();
                if db.apply(&defaults, None).is_ok() {
                    classes = db.load_classes().unwrap_or_default();
                }
            }
            if !read_only && let Err(e) = db.purge_deleted(now_ms() - PURGE_AFTER_MS) {
                core.host
                    .warn(format!("cannot purge deleted annotations: {e}"));
            }
            let legacy_export: Vec<(String, Option<String>)> =
                ["author", "organization", "project", "license"]
                    .iter()
                    .map(|k| {
                        (
                            k.to_string(),
                            db.setting(&format!("export.{k}")).ok().flatten(),
                        )
                    })
                    .collect();
            let path = db.path().to_string_lossy().into_owned();
            ctx.db = Some(db);
            let pending: Vec<FileId> = {
                let mut state = core.state();
                state.db = DbStatus::Ready { read_only, path };
                state.db_notes = report.notes.clone();
                if fresh {
                    // The plugin never looks outside its data directory, so
                    // say where the pre-1.0 database usually is.
                    state.db_notes.push(LEGACY_DATABASE_HINT.to_string());
                }
                state.classes = classes;
                state.sort_classes();
                // Settings from the pre-1.0 export metadata.
                let mut adopted = false;
                for (key, value) in legacy_export {
                    let Some(value) = value else { continue };
                    let field = match key.as_str() {
                        "author" => &mut state.settings.author,
                        "organization" => &mut state.settings.organization,
                        "project" => &mut state.settings.project,
                        _ => &mut state.settings.license,
                    };
                    if field.is_empty() {
                        *field = value;
                        adopted = true;
                    }
                }
                if adopted {
                    let _ = core.host.set_setting("settings", &state.settings);
                }
                state
                    .files
                    .iter()
                    .filter(|(_, f)| f.load == Load::Pending)
                    .map(|(id, _)| *id)
                    .collect()
            };
            for note in &report.notes {
                core.notify(NotifyLevel::Info, note.clone());
            }
            for file_id in pending {
                core.db(DbJob::Load {
                    file_id,
                    fingerprint: Fingerprint::NotComputed,
                });
            }
            let _ = core.host.post(PALETTE_TOKEN);
        }
        Err(error) => {
            {
                let mut state = core.state();
                state.db = DbStatus::Failed {
                    error: error.clone(),
                };
                for file in state.files.values_mut() {
                    if file.load == Load::Pending {
                        file.load = Load::Failed {
                            error: error.clone(),
                            attempts: 1,
                            retry_at: Instant::now() + Duration::from_secs(5),
                        };
                    }
                }
            }
            core.error("The annotation database could not be opened", error);
        }
    }
    core.request_refresh();
}

/// The key used when the host provides no identity and no fingerprint can
/// be computed.
fn fallback_key(info: &FileInfo) -> String {
    format!("file:{}:{}x{}", info.filename, info.width, info.height)
}

fn load_failed(core: &Arc<Core>, file_id: FileId, error: String) {
    {
        let mut state = core.state();
        if let Some(file) = state.files.get_mut(&file_id) {
            let attempts = match &file.load {
                Load::Failed { attempts, .. } => attempts + 1,
                _ => 1,
            };
            let backoff = Duration::from_secs((1u64 << attempts.min(6)).min(60));
            file.load = Load::Failed {
                error: error.clone(),
                attempts,
                retry_at: Instant::now() + backoff,
            };
        }
    }
    core.host.warn(format!(
        "cannot load the annotations of this slide: {error}"
    ));
    core.request_refresh();
}

fn load_file(core: &Arc<Core>, ctx: &mut DbCtx, file_id: FileId, fingerprint: Fingerprint) {
    let (info, existing) = {
        let state = core.state();
        let Some(file) = state.files.get(&file_id) else {
            return;
        };
        if matches!(file.load, Load::Loaded(_)) {
            return;
        }
        // The same slide may already be loaded for another tab.
        let existing = file.info.identity.as_ref().and_then(|identity| {
            state
                .slides
                .values()
                .find(|s| s.record.identity.as_deref() == Some(identity.as_str()))
                .map(SlideData::id)
        });
        (file.info.clone(), existing)
    };
    if let Some(slide_id) = existing {
        attach(core, file_id, slide_id);
        return;
    }
    let Some(db) = ctx.db.as_mut() else {
        let error = match &core.state().db {
            DbStatus::Failed { error } => error.clone(),
            _ => "the database is not open".to_string(),
        };
        load_failed(core, file_id, error);
        return;
    };
    let need_fingerprint =
        info.identity.is_none() || db.has_unmatched_legacy_slides().unwrap_or(false);
    if need_fingerprint && fingerprint == Fingerprint::NotComputed {
        // Sample the slide file on the I/O thread (ANN-14), then come back.
        let path = PathBuf::from(&info.path);
        core.io(move |core| {
            let fingerprint = fileio::legacy_fingerprint(&path).ok();
            core.db(DbJob::Load {
                file_id,
                fingerprint: Fingerprint::Computed(fingerprint),
            });
        });
        return;
    }
    let fingerprint = match fingerprint {
        Fingerprint::Computed(f) => f,
        Fingerprint::NotComputed => None,
    };
    let fallback = (info.identity.is_none() && fingerprint.is_none()).then(|| fallback_key(&info));
    let found = match db.find_slide(
        info.identity.as_deref(),
        fingerprint.as_ref(),
        fallback.as_deref(),
    ) {
        Ok(found) => found,
        Err(e) => {
            load_failed(core, file_id, e.to_string());
            return;
        }
    };
    let now = now_ms();
    let record = SlideRecord {
        id: found.as_ref().map_or(0, |r| r.id),
        identity: info.identity.clone(),
        legacy_fingerprint: fingerprint.or(found.as_ref().and_then(|r| r.legacy_fingerprint)),
        fallback_key: fallback,
        filename: info.filename.clone(),
        width: info.width,
        height: info.height,
        mpp_x: info.mpp_x,
        mpp_y: info.mpp_y,
        vendor: info.vendor.clone(),
        created_at: found.as_ref().map_or(now, |r| r.created_at),
        last_opened_at: now,
    };
    let mut record = match db.upsert_slide(&record) {
        Ok(record) => record,
        Err(e) => {
            load_failed(core, file_id, e.to_string());
            return;
        }
    };
    if record.id == 0 {
        // Read-only database without a record for this slide.
        let mut state = core.state();
        record.id = state.next_virtual_slide;
        state.next_virtual_slide -= 1;
    }
    let loaded = if record.id > 0 {
        match db.load_slide(record.id) {
            Ok(loaded) => loaded,
            Err(e) => {
                load_failed(core, file_id, e.to_string());
                return;
            }
        }
    } else {
        Default::default()
    };
    if loaded.skipped > 0 {
        core.notify(
            NotifyLevel::Warning,
            format!(
                "{} annotation(s) of {} could not be read and were skipped.",
                loaded.skipped, info.filename
            ),
        );
    }
    let slide = SlideData::new(
        record,
        loaded.layers,
        loaded.annotations,
        loaded.invalid,
        loaded.current_layer,
    );
    let slide_id = slide.id();
    {
        let mut state = core.state();
        if !state.files.contains_key(&file_id) {
            return;
        }
        if !state.slides.contains_key(&slide_id) {
            state.insert_slide(slide);
        }
    }
    attach(core, file_id, slide_id);
}

/// Shows a loaded slide's annotations in a file.
fn attach(core: &Arc<Core>, file_id: FileId, slide_id: SlideId) {
    let batch = {
        let mut state = core.state();
        let Some(file) = state.files.get_mut(&file_id) else {
            return;
        };
        file.load = Load::Loaded(slide_id);
        overlay::sync_file(&mut state, file_id)
    };
    let ops = batch.build();
    core.exec(ops);
    let reticle = crate::actions::reticle_ops(&core.state(), file_id);
    core.exec(reticle);
    core.request_refresh();
}

/// Re-reads every loaded slide (another eov window wrote to the database).
pub fn reload_all(core: &Arc<Core>, ctx: &mut DbCtx) {
    let Some(db) = ctx.db.as_mut() else {
        return;
    };
    let classes = db.load_classes().unwrap_or_default();
    let slides: Vec<(SlideId, SlideRecord)> = core
        .state()
        .slides
        .values()
        .map(|s| (s.id(), s.record.clone()))
        .collect();
    let mut reloaded = Vec::new();
    for (id, record) in slides {
        if id <= 0 {
            continue;
        }
        match db.load_slide(id) {
            Ok(loaded) => reloaded.push(SlideData::new(
                record,
                loaded.layers,
                loaded.annotations,
                loaded.invalid,
                loaded.current_layer,
            )),
            Err(e) => core.host.warn(format!("cannot reload annotations: {e}")),
        }
    }
    let batch = {
        let mut state = core.state();
        state.classes = classes;
        state.sort_classes();
        for slide in reloaded {
            let id = slide.id();
            let current = state.slides.get(&id).and_then(|s| s.current_layer.clone());
            let mut slide = slide;
            slide.current_layer = slide.current_layer.or(current);
            state.slides.remove(&id);
            state.layer_slide.retain(|_, s| *s != id);
            state.insert_slide(slide);
        }
        overlay::sync_all(&mut state)
    };
    core.exec(batch.build());
    let _ = core.host.post(PALETTE_TOKEN);
    core.request_refresh();
}

/// Checks whether another process changed the database and reloads.
pub fn check_external(core: &Arc<Core>, ctx: &mut DbCtx) {
    let changed = ctx.db.as_mut().is_some_and(Db::changed_externally);
    if changed {
        reload_all(core, ctx);
        core.notify(
            NotifyLevel::Info,
            "Annotations were changed by another eov window and have been reloaded.",
        );
    }
}

fn is_busy(error: &DbError) -> bool {
    matches!(error, DbError::Failed(m) if m.contains("locked") || m.contains("busy"))
}

/// Applies changes with a couple of retries when another process holds
/// the database (ANN-13).
fn apply_with_retry(
    db: &mut Db,
    changes: &[Change],
    audit: &Audit,
) -> Result<crate::db::Restored, DbError> {
    let mut delay = Duration::from_millis(500);
    for attempt in 0..3 {
        match db.apply(changes, Some(audit)) {
            Err(e) if is_busy(&e) && attempt < 2 => {
                std::thread::sleep(delay);
                delay *= 4;
            }
            other => return other,
        }
    }
    Err(DbError::Failed("the database stayed locked".to_string()))
}

fn friendly(error: &DbError) -> String {
    match error {
        DbError::Failed(m) if m.contains("UNIQUE") && m.contains("classes") => {
            "a class with that name already exists".to_string()
        }
        other => other.to_string(),
    }
}

pub fn run_op(core: &Arc<Core>, ctx: &mut DbCtx, op: Op) {
    let planned = {
        let state = core.state();
        (op.plan)(&state)
    };
    let planned = match planned {
        Ok(planned) => planned,
        Err(message) => {
            if !message.is_empty() {
                core.notify(NotifyLevel::Warning, message);
            }
            core.request_refresh();
            return;
        }
    };
    let label = planned.label.clone().unwrap_or(op.label);
    if planned.changes.is_empty() {
        finish(core, &planned, None);
        return;
    }
    let author = core.state().settings.author.clone();
    let slide = planned.changes.iter().find_map(Change::slide).or_else(|| {
        let state = core.state();
        planned.changes.iter().find_map(|c| match c {
            Change::Annotation { before, after } => after
                .as_ref()
                .or(before.as_ref())
                .and_then(|a| state.slide_of_layer(&a.layer_id)),
            _ => None,
        })
    });
    let audit = Audit {
        author,
        slide_id: slide,
        action: label.clone(),
        detail: change::summary(&planned.changes),
    };
    let result = match ctx.db.as_mut() {
        Some(db) => apply_with_retry(db, &planned.changes, &audit),
        None => Err(DbError::Failed("the database is not open".to_string())),
    };
    match result {
        Ok(moved) => {
            let batch = {
                let mut state = core.state();
                state.apply(&planned.changes, &moved);
                let batch = overlay::for_changes(&mut state, &planned.changes);
                if op.record {
                    let entry_id = state.history.next_id();
                    state.history.insert(
                        entry_id.clone(),
                        Record {
                            slide,
                            file_id: op.file_id,
                            label: label.clone(),
                            changes: planned.changes.clone(),
                            undone: false,
                        },
                    );
                    // Only for files still open (the host forgets closed ones).
                    let file_id = op.file_id.filter(|id| state.files.contains_key(id));
                    let _ = core.host.push_undo(entry_id, file_id, label.clone());
                }
                batch
            };
            core.exec(batch.build());
            finish(core, &planned, Some(&label));
        }
        Err(error) => {
            if let Some((slide, ids)) = &planned.revert {
                let batch = overlay::for_annotations(&core.state(), *slide, ids);
                core.exec(batch.build());
            }
            match &error {
                DbError::Conflict(_) => {
                    core.error(format!("Could not save \"{label}\""), friendly(&error));
                    check_external(core, ctx);
                }
                _ => core.error(format!("Could not save \"{label}\""), friendly(&error)),
            }
            core.request_refresh();
        }
    }
}

/// Follow-ups of a successful (or empty) edit.
fn finish(core: &Arc<Core>, planned: &Planned, _label: Option<&str>) {
    if planned.changes.is_empty()
        && let Some((slide, ids)) = &planned.revert
    {
        let batch = overlay::for_annotations(&core.state(), *slide, ids);
        core.exec(batch.build());
    }
    if let Some(warning) = &planned.warning {
        core.notify(NotifyLevel::Warning, warning.clone());
    }
    if let Some((slide, layer)) = &planned.current_layer {
        let layer = layer.clone();
        let slide = *slide;
        let changed = {
            let mut state = core.state();
            match state.slides.get_mut(&slide) {
                Some(s) if s.current_layer.as_deref() != Some(layer.as_str()) => {
                    s.current_layer = Some(layer.clone());
                    true
                }
                _ => false,
            }
        };
        if changed && slide > 0 {
            core.db_run(move |core, ctx| {
                if let Some(db) = ctx.db.as_mut()
                    && let Err(e) = db.set_current_layer(slide, Some(&layer))
                    && e != DbError::ReadOnly
                {
                    core.host
                        .warn(format!("cannot remember the current layer: {e}"));
                }
            });
        }
    }
    if let Some((file_id, ids)) = &planned.select {
        let ops = {
            let mut state = core.state();
            if let Some(file) = state.files.get_mut(file_id) {
                file.selection = ids.clone();
            }
            overlay::select(&state, *file_id)
        };
        core.exec(ops);
    }
    if let Some(notice) = &planned.notice {
        core.notify(NotifyLevel::Info, notice.clone());
    }
    let _ = core.host.post(PALETTE_TOKEN);
    core.request_refresh();
}

fn run_history(core: &Arc<Core>, ctx: &mut DbCtx, entry_id: &str, redo: bool) {
    let (changes, label, slide) = {
        let state = core.state();
        let Some(record) = state.history.get(entry_id) else {
            drop(state);
            core.notify(
                NotifyLevel::Warning,
                "This annotation change can no longer be undone.",
            );
            return;
        };
        if record.undone != redo {
            // Already in the requested state (duplicate request).
            return;
        }
        if let Some(slide) = record.slide
            && !state.slides.contains_key(&slide)
        {
            drop(state);
            core.notify(
                NotifyLevel::Warning,
                "The slide of this change is not open any more.",
            );
            return;
        }
        let changes = if redo {
            record.changes.clone()
        } else {
            change::invert(&record.changes)
        };
        (changes, record.label.clone(), record.slide)
    };
    let audit = Audit {
        author: core.state().settings.author.clone(),
        slide_id: slide,
        action: format!("{} {label}", if redo { "Redo" } else { "Undo" }),
        detail: change::summary(&changes),
    };
    let result = match ctx.db.as_mut() {
        Some(db) => apply_with_retry(db, &changes, &audit),
        None => Err(DbError::Failed("the database is not open".to_string())),
    };
    match result {
        Ok(moved) => {
            let batch = {
                let mut state = core.state();
                state.apply(&changes, &moved);
                if let Some(record) = state.history.get_mut(entry_id) {
                    record.undone = !redo;
                }
                overlay::for_changes(&mut state, &changes)
            };
            core.exec(batch.build());
            let _ = core.host.post(PALETTE_TOKEN);
            core.request_refresh();
        }
        Err(error) => {
            core.state().history.remove(entry_id);
            core.error(
                format!(
                    "Could not {} \"{label}\"",
                    if redo { "redo" } else { "undo" }
                ),
                friendly(&error),
            );
            if matches!(error, DbError::Conflict(_)) {
                check_external(core, ctx);
            }
        }
    }
}

/// Starts loading a newly opened file.
pub fn file_opened(core: &Arc<Core>, info: FileInfo) {
    let file_id = info.file_id;
    {
        let mut state = core.state();
        let mut file = OpenFile::new(info);
        if let Some(old) = state.files.get(&file_id) {
            file.hidden = old.hidden;
        }
        state.files.insert(file_id, file);
    }
    core.db(DbJob::Load {
        file_id,
        fingerprint: Fingerprint::NotComputed,
    });
    core.request_refresh();
}

/// Retries a failed load once its backoff passed.
pub fn retry_if_due(core: &Arc<Core>, file_id: FileId, force: bool) {
    let due = {
        let mut state = core.state();
        let failed_db = matches!(state.db, DbStatus::Failed { .. });
        match state.files.get_mut(&file_id) {
            Some(file) => match &file.load {
                Load::Failed { retry_at, .. } if force || Instant::now() >= *retry_at => {
                    if !failed_db {
                        file.load = Load::Pending;
                    }
                    true
                }
                _ => false,
            },
            None => false,
        }
    };
    if due {
        let failed_db = matches!(core.state().db, DbStatus::Failed { .. });
        if failed_db {
            core.db(DbJob::Open);
        }
        core.db(DbJob::Load {
            file_id,
            fingerprint: Fingerprint::NotComputed,
        });
    }
}
