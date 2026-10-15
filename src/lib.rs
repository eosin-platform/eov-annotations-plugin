//! The eov annotations plugin (plugin API 1.0).
//!
//! Point, region, line and text annotations in per-slide layers, reusable
//! classes, cell counting with per-class tallies and densities, HPF fields,
//! per-annotation measurements, and GeoJSON (QuPath), ASAP, ImageScope and
//! eov import/export. Data lives in a SQLite database in the plugin's data
//! directory, keyed by the host's slide identity.
//!
//! Coordinates are level-0 pixels of the slide (x right, y down, origin at
//! the top-left of the level-0 canvas).
//!
//! Module map: [`model`] and [`geometry`] (data and validation), [`db`]
//! (persistence), [`core`] (threads and the edit pipeline), [`actions`]
//! (what user actions do), [`overlay`] (host layers), [`sidebar`] (the
//! panel), [`transfer`] and [`formats`] (import/export), [`counting`],
//! [`measure`] and [`stats`] (quantification).

pub mod actions;
pub mod change;
pub mod core;
pub mod counting;
pub mod db;
pub mod fileio;
pub mod formats;
pub mod geometry;
pub mod measure;
pub mod model;
pub mod overlay;
pub mod sidebar;
pub mod state;
pub mod stats;
pub mod tools;
pub mod transfer;

use crate::core::{Core, DbJob, PALETTE_TOKEN, Paths, REFRESH_TOKEN};
use crate::model::{Geometry, LayerRole};
use crate::sidebar::{SidebarHost, SidebarView};
use crate::state::{Pending, Settings, State};
use eov_plugin_api::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

/// Host settings key of [`Settings`].
pub const SETTINGS_KEY: &str = "settings";
/// Optional host settings key overriding the database location (e.g. a
/// shared, read-only teaching database).
pub const DATABASE_PATH_KEY: &str = "database_path";

/// Signature of a registered class button: hotkey, active, icon, tooltip.
type PaletteEntry = (String, bool, String, String);

#[derive(Default)]
struct Ui {
    sidebar: Option<Surface>,
    /// Open import dialogs: surface id → import plan id.
    imports: BTreeMap<String, u64>,
    /// Class toolbar buttons registered, by button id.
    palette: BTreeMap<String, PaletteEntry>,
    /// HUD "visibility" state per pane.
    hud: BTreeMap<PaneId, bool>,
    /// The annotation highlighted from the list.
    highlighted: Option<(FileId, String)>,
}

pub struct AnnotationsPlugin {
    core: Arc<Core>,
    ui: Mutex<Ui>,
    view: Mutex<SidebarView>,
}

/// Where the database lives: `database_path` from the settings, or
/// `$EOV_ANNOTATIONS_DB`, or `annotations.db` in the plugin's data
/// directory (never the home directory).
fn database_path(init: &PluginInit) -> PathBuf {
    if let Some(path) = init
        .settings
        .get(DATABASE_PATH_KEY)
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
    {
        return PathBuf::from(path);
    }
    #[cfg(not(test))]
    if let Some(path) = std::env::var_os("EOV_ANNOTATIONS_DB").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    PathBuf::from(&init.data_dir).join("annotations.db")
}

impl AnnotationsPlugin {
    fn ui(&self) -> MutexGuard<'_, Ui> {
        self.ui.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds files that were open before the plugin started (no
    /// `FileOpened` is sent for them).
    fn adopt_open_files(&self) {
        let Ok(snapshot) = self.core.host.snapshot() else {
            return;
        };
        let known: Vec<FileId> = self.core.state().files.keys().copied().collect();
        for file in snapshot.files.iter().cloned() {
            if !known.contains(&file.file_id) {
                crate::core::file_opened(&self.core, file);
            }
        }
        let mut state = self.core.state();
        if state.active_file.is_none() {
            state.active_pane = snapshot.focused_pane;
            state.active_file = snapshot.focused_view().map(|v| v.file_id);
        }
    }

    fn toggle_sidebar(&self) -> Result<(), String> {
        let existing = self.ui().sidebar.take();
        if let Some(sidebar) = existing
            && sidebar.is_open()
        {
            sidebar.close().map_err(String::from)?;
            let _ = self
                .core
                .host
                .set_toolbar_button_active(tools::SIDEBAR_BUTTON, false);
            return Ok(());
        }
        self.open_sidebar()
    }

    fn open_sidebar(&self) -> Result<(), String> {
        if self.ui().sidebar.as_ref().is_some_and(Surface::is_open) {
            return Ok(());
        }
        let properties = {
            let state = self.core.state();
            sidebar::properties(&state, &SidebarHost::view(self))
        };
        let mut spec = SurfaceSpec::new(
            SurfaceKind::Sidebar,
            sidebar::SIDEBAR_UI,
            sidebar::SIDEBAR_COMPONENT,
        )
        .with_title("Annotations")
        .with_size(340.0, None);
        spec.properties = properties;
        let surface = self.core.host.open_surface(spec).map_err(String::from)?;
        self.ui().sidebar = Some(surface);
        let _ = self
            .core
            .host
            .set_toolbar_button_active(tools::SIDEBAR_BUTTON, true);
        Ok(())
    }

    /// Pushes the sidebar's properties (only changed values are applied by
    /// the host).
    fn refresh_sidebar(&self) {
        let Some(sidebar) = self.ui().sidebar.clone() else {
            return;
        };
        let properties = {
            let state = self.core.state();
            sidebar::properties(&state, &SidebarHost::view(self))
        };
        if sidebar.set_many(properties).is_err() {
            // Closed behind our back.
            self.ui().sidebar = None;
        }
    }

    /// Keeps one toolbar button per class with a hotkey; pressing the key
    /// (or clicking) makes the class active.
    fn sync_palette(&self) {
        let desired: BTreeMap<String, ToolbarButtonSpec> = {
            let state = self.core.state();
            state
                .classes
                .iter()
                .filter_map(|c| {
                    let active = state.settings.active_class.as_deref() == Some(c.id.as_str());
                    tools::class_button(c, active)
                })
                .map(|spec| (spec.button_id.clone(), spec))
                .collect()
        };
        let mut ui = self.ui();
        let stale: Vec<String> = ui
            .palette
            .keys()
            .filter(|id| !desired.contains_key(*id))
            .cloned()
            .collect();
        for id in stale {
            let _ = self.core.host.remove_toolbar_button(&id);
            ui.palette.remove(&id);
        }
        for (id, spec) in desired {
            let signature: PaletteEntry = (
                spec.hotkey
                    .as_ref()
                    .map(|h| h.key.clone())
                    .unwrap_or_default(),
                spec.active,
                spec.icon_svg.clone(),
                spec.tooltip.clone(),
            );
            if ui.palette.get(&id) == Some(&signature) {
                continue;
            }
            let only_active_changed = ui.palette.get(&id).is_some_and(|old| {
                old.0 == signature.0 && old.2 == signature.2 && old.3 == signature.3
            });
            let result = if only_active_changed {
                self.core.host.set_toolbar_button_active(&id, signature.1)
            } else {
                self.core.host.add_toolbar_button(spec)
            };
            if result.is_ok() {
                ui.palette.insert(id, signature);
            }
        }
    }

    fn on_tool_activated(&self, tool_id: String) {
        let field = tool_id == tools::FIELD;
        {
            let mut state = self.core.state();
            state.active_tool = Some(tool_id.clone());
            let class = state
                .settings
                .active_class
                .as_deref()
                .and_then(|id| state.class(id))
                .map_or("Unclassified".to_string(), |c| c.name.clone());
            let status = match tool_id.as_str() {
                tools::COUNT => {
                    let alternate = state
                        .settings
                        .alternate_class
                        .as_deref()
                        .and_then(|id| state.class(id))
                        .map(|c| format!(", Shift+click: {}", c.name))
                        .unwrap_or_default();
                    format!("Counting {class}{alternate}")
                }
                tools::FIELD => {
                    "Click to place an HPF field; the dashed circle shows its size".to_string()
                }
                tools::SELECT => {
                    "Click to select, drag to select several; drag handles to edit".to_string()
                }
                _ => format!("Drawing {class}"),
            };
            self.core.host.set_status(Some(&status));
        }
        if field {
            if let Ok(snapshot) = self.core.host.snapshot() {
                for pane in snapshot.panes {
                    if let Some(view) = pane.view {
                        actions::view_changed(&self.core, &view);
                    }
                }
            }
            actions::refresh_reticles(&self.core);
        }
    }

    fn on_tool_deactivated(&self, tool_id: String) {
        {
            let mut state = self.core.state();
            if state.active_tool.as_deref() == Some(tool_id.as_str()) {
                state.active_tool = None;
            }
        }
        self.core.host.set_status(None);
        if tool_id == tools::FIELD {
            actions::refresh_reticles(&self.core);
        }
    }

    fn toggle_visibility(&self, pane: PaneId, file_id: FileId) {
        let (ops, hidden) = {
            let mut state = self.core.state();
            let Some(file) = state.files.get_mut(&file_id) else {
                return;
            };
            file.hidden = !file.hidden;
            let hidden = file.hidden;
            (overlay::file_visibility(&state, file_id), hidden)
        };
        self.core.exec(ops);
        self.set_hud(pane, hidden);
    }

    fn set_hud(&self, pane: PaneId, hidden: bool) {
        let mut ui = self.ui();
        if ui.hud.get(&pane) == Some(&hidden) {
            return;
        }
        ui.hud.insert(pane, hidden);
        let _ = self.core.host.update_hud_button(ButtonUpdate {
            button_id: tools::VISIBILITY_HUD.into(),
            pane: Some(pane),
            active: Some(hidden),
            ..Default::default()
        });
    }

    /// Highlights the annotation hovered in the list.
    fn highlight(&self, target: Option<(FileId, String)>) {
        let previous = {
            let mut ui = self.ui();
            if ui.highlighted == target {
                return;
            }
            std::mem::replace(&mut ui.highlighted, target.clone())
        };
        let mut ops = Vec::new();
        {
            let state = self.core.state();
            if let Some((file_id, id)) = previous
                && let Some(slide) = state.slide_of(file_id)
            {
                ops.extend(overlay::for_annotations(&state, slide.id(), &[id]).build());
            }
            if let Some((file_id, id)) = &target
                && let Some(slide) = state.slide_of(*file_id)
                && let Some(a) = slide.annotations.get(id)
                && let Some(layer) = slide.layer(&a.layer_id)
                && let Some(object) = overlay::Styler::new(&state).highlighted(a, layer)
            {
                ops.push(overlay::HostOp::Upsert(
                    overlay::host_layer_id(*file_id, &a.layer_id),
                    vec![object],
                ));
            }
        }
        self.core.exec(ops);
    }

    fn open_import_dialog(&self, plan_id: u64) {
        let properties = {
            let mut state = self.core.state();
            let Some(plan) = state.imports.get(&plan_id) else {
                return;
            };
            if !state.files.contains_key(&plan.file_id) {
                state.imports.remove(&plan_id);
                return;
            }
            let mut p = Map::new();
            p.insert("source".into(), json!(plan.source));
            p.insert(
                "summary".into(),
                json!(format!(
                    "{} · {} annotation(s) in {} layer(s){}",
                    plan.format,
                    plan.annotation_count(),
                    plan.layers.len(),
                    if plan.classes.is_empty() {
                        String::new()
                    } else {
                        format!(" · {} class(es)", plan.classes.len())
                    }
                )),
            );
            p.insert("match-text".into(), json!(plan.match_text));
            p.insert("mismatch".into(), json!(plan.mismatch));
            p.insert("notes".into(), json!(plan.notes.join("\n")));
            p.insert(
                "conflicts".into(),
                json!(if plan.conflicts.is_empty() {
                    String::new()
                } else {
                    format!(
                        "Layers with the same name already exist: {}",
                        plan.conflicts.join(", ")
                    )
                }),
            );
            p
        };
        let mut spec = SurfaceSpec::new(
            SurfaceKind::Modal,
            sidebar::IMPORT_UI,
            sidebar::IMPORT_COMPONENT,
        )
        .with_title("Import annotations")
        .with_size(520.0, Some(400.0));
        spec.properties = properties;
        match self.core.host.open_surface(spec) {
            Ok(surface) => {
                self.ui().imports.insert(surface.id().to_string(), plan_id);
            }
            Err(e) => {
                self.core.state().imports.remove(&plan_id);
                self.core.error("Cannot show the import dialog", e.message);
            }
        }
    }

    fn import_callback(
        &self,
        surface_id: &str,
        name: &str,
        args: &[Value],
    ) -> Result<Value, String> {
        let Some(plan_id) = self.ui().imports.remove(surface_id) else {
            return Ok(Value::Null);
        };
        let plan = self.core.state().imports.remove(&plan_id);
        let _ = self.core.host.surface(surface_id).close();
        if let (Some(plan), "import-confirm") = (plan, name) {
            let strategy = transfer::Strategy::from_index(
                args.first().and_then(Value::as_f64).unwrap_or(0.0) as i64,
            );
            transfer::apply_import(&self.core, plan, strategy);
        }
        Ok(Value::Null)
    }

    fn dialog_result(&self, request_id: &str, paths: Vec<String>) {
        let Some(pending) = self.core.state().pending.remove(request_id) else {
            return;
        };
        let Some(path) = paths.into_iter().next().map(PathBuf::from) else {
            return;
        };
        match pending {
            Pending::Import { file_id } => transfer::import_file(&self.core, file_id, path),
            Pending::Export { file_id, format } => {
                transfer::export_file(&self.core, file_id, format, path)
            }
            Pending::ExportAll => transfer::export_all(&self.core, path),
            Pending::Backup => transfer::backup(&self.core, path),
            Pending::MergeDb => transfer::merge_database(&self.core, path),
            _ => {}
        }
    }

    fn confirm_result(&self, request_id: &str, confirmed: bool) {
        let Some(pending) = self.core.state().pending.remove(request_id) else {
            return;
        };
        if !confirmed {
            return;
        }
        match pending {
            Pending::DeleteLayer { file_id, layer_id } => {
                actions::layer_delete(&self.core, file_id, layer_id)
            }
            Pending::ClearCounts { file_id } => actions::clear_counts(&self.core, file_id),
            Pending::DeleteClass { class_id } => actions::class_delete(&self.core, class_id),
            Pending::Relink { file_id, from } => actions::relink(&self.core, file_id, from),
            _ => {}
        }
    }

    fn menu_item(&self, item_id: &str, context: MenuContext) -> Result<(), String> {
        let file_id = context.view.file_id;
        let ours = |r: &ObjectRef| {
            overlay::parse_host_layer_id(&r.layer_id).is_some_and(|(f, _)| f == file_id)
        };
        let hit = context
            .hit
            .as_ref()
            .filter(|h| ours(h))
            .map(|h| h.object_id.clone());
        // Act on the selection when the clicked object is part of it.
        let selection: Vec<String> = context
            .selection
            .iter()
            .filter(|r| ours(r))
            .map(|r| r.object_id.clone())
            .collect();
        let targets = match &hit {
            Some(id) if selection.contains(id) => selection,
            Some(id) => vec![id.clone()],
            None => selection,
        };
        match item_id {
            "add-point" => {
                let class = self.core.state().settings.active_class.clone();
                actions::add(
                    &self.core,
                    file_id,
                    vec![Geometry::Point {
                        x: context.image_x,
                        y: context.image_y,
                    }],
                    LayerRole::Normal,
                    class,
                    "",
                    true,
                    "Add point",
                );
            }
            "add-field" => actions::shape_drawn(
                &self.core,
                tools::FIELD,
                file_id,
                Shape::Point {
                    x: context.image_x,
                    y: context.image_y,
                },
                Modifiers::default(),
            ),
            "show" => {
                if let Some(id) = hit {
                    actions::select(&self.core, file_id, vec![id]);
                    self.core.state().settings.tab = sidebar::TAB_LAYERS;
                    self.open_sidebar()?;
                    self.refresh_sidebar();
                }
            }
            "lock" | "unlock" => {
                let locked = item_id == "lock";
                actions::edit(
                    &self.core,
                    file_id,
                    targets,
                    if locked { "Lock" } else { "Unlock" },
                    move |a| {
                        a.locked = locked;
                        Ok(())
                    },
                );
            }
            "delete" => actions::delete(&self.core, file_id, targets),
            "copy" => {
                if let Some(id) = hit {
                    actions::select(&self.core, file_id, vec![id]);
                    let text = sidebar::measurement_text(&self.core.state(), file_id);
                    self.core.host.copy_text(text).map_err(String::from)?;
                }
            }
            "panel" => self.toggle_sidebar()?,
            _ => {}
        }
        Ok(())
    }

    fn file_closed(&self, file_id: FileId) {
        let core = &self.core;
        {
            let mut state = core.state();
            state.files.remove(&file_id);
            if state.active_file == Some(file_id) {
                state.active_file = None;
            }
            if state.hovered.as_ref().is_some_and(|(f, _)| *f == file_id) {
                state.hovered = None;
            }
            state.evict_unused_slides();
            state.pending.retain(|_, p| match p {
                Pending::Import { file_id: f }
                | Pending::Export { file_id: f, .. }
                | Pending::DeleteLayer { file_id: f, .. }
                | Pending::ClearCounts { file_id: f }
                | Pending::Relink { file_id: f, .. } => *f != file_id,
                _ => true,
            });
            state.imports.retain(|_, plan| plan.file_id != file_id);
        }
        let mut ui = self.ui();
        if ui.highlighted.as_ref().is_some_and(|(f, _)| *f == file_id) {
            ui.highlighted = None;
        }
        // Imports bound to the closed slide are cancelled (ANN-9).
        let stale: Vec<String> = {
            let state = core.state();
            ui.imports
                .iter()
                .filter(|(_, plan)| !state.imports.contains_key(plan))
                .map(|(surface, _)| surface.clone())
                .collect()
        };
        for surface in stale {
            ui.imports.remove(&surface);
            let _ = core.host.surface(surface).close();
        }
        drop(ui);
        core.request_refresh();
    }
}

impl SidebarHost for AnnotationsPlugin {
    fn view(&self) -> MutexGuard<'_, SidebarView> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn refresh(&self) {
        self.refresh_sidebar();
    }

    fn save_settings(&self) {
        let settings = self.core.state().settings.clone();
        if let Err(e) = self.core.host.set_setting(SETTINGS_KEY, &settings) {
            self.core
                .host
                .warn(format!("cannot save the annotation settings: {e}"));
        }
    }
}

impl Plugin for AnnotationsPlugin {
    fn create(host: Host, init: PluginInit) -> Result<Self, String> {
        let settings: Settings = init
            .settings
            .get(SETTINGS_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let paths = Paths {
            data_dir: PathBuf::from(&init.data_dir),
            cache_dir: PathBuf::from(&init.cache_dir),
            db_path: database_path(&init),
        };
        for spec in tools::tool_specs() {
            host.register_tool(spec).map_err(String::from)?;
        }
        host.add_toolbar_button(ToolbarButtonSpec {
            button_id: tools::SIDEBAR_BUTTON.into(),
            tooltip: "Annotations".into(),
            icon_svg: tools::sidebar_icon(),
            order: 50,
            ..Default::default()
        })
        .map_err(String::from)?;
        let _ = host.add_hud_button(HudButtonSpec {
            button_id: tools::VISIBILITY_HUD.into(),
            tooltip: "Show or hide annotations on this slide".into(),
            icon_svg: tools::visible_icon(),
            toggled_icon_svg: Some(tools::hidden_icon()),
            ..Default::default()
        });
        let core = Core::start(
            host,
            paths,
            init.plugin_version,
            State::new(settings.sanitized()),
        );
        let plugin = Self {
            core,
            ui: Mutex::new(Ui::default()),
            view: Mutex::new(SidebarView::default()),
        };
        plugin.adopt_open_files();
        Ok(plugin)
    }

    fn on_event(&self, event: HostEvent) -> Result<(), String> {
        let core = &self.core;
        match event {
            HostEvent::Ready { restore_sidebar } => {
                self.adopt_open_files();
                if restore_sidebar {
                    self.open_sidebar()?;
                }
                self.sync_palette();
            }
            HostEvent::Shutdown => core.sync(),
            HostEvent::FileOpened { file } => crate::core::file_opened(core, file),
            HostEvent::FileClosed { file_id } => self.file_closed(file_id),
            HostEvent::ActiveChanged { pane, file_id } => {
                {
                    let mut state = core.state();
                    state.active_pane = pane;
                    state.active_file = file_id;
                }
                if let Some(file_id) = file_id {
                    crate::core::retry_if_due(core, file_id, false);
                    actions::check_external(core);
                    let hidden = core.state().files.get(&file_id).is_some_and(|f| f.hidden);
                    if let Some(pane) = pane {
                        self.set_hud(pane, hidden);
                    }
                }
                core.request_refresh();
            }
            HostEvent::ViewChanged { view } => {
                actions::view_changed(core, &view);
                let hidden = core
                    .state()
                    .files
                    .get(&view.file_id)
                    .is_some_and(|f| f.hidden);
                self.set_hud(view.pane, hidden);
            }
            HostEvent::WindowFocusChanged { focused: true } => actions::check_external(core),
            HostEvent::ToolbarButtonClicked { button_id } => {
                if button_id == tools::SIDEBAR_BUTTON {
                    self.toggle_sidebar()?;
                } else if let Some(class_id) = button_id.strip_prefix(tools::CLASS_BUTTON_PREFIX) {
                    actions::set_active_class(core, Some(class_id.to_string()));
                }
            }
            HostEvent::HudButtonClicked {
                button_id,
                pane,
                view: Some(view),
                ..
            } if button_id == tools::VISIBILITY_HUD => self.toggle_visibility(pane, view.file_id),
            HostEvent::ContextMenuItemSelected { item_id, context } => {
                self.menu_item(&item_id, context)?
            }
            HostEvent::ToolActivated { tool_id } if tools::is_tool(&tool_id) => {
                self.on_tool_activated(tool_id)
            }
            HostEvent::ToolDeactivated { tool_id } if tools::is_tool(&tool_id) => {
                self.on_tool_deactivated(tool_id)
            }
            HostEvent::ShapeDrawn {
                tool_id,
                file_id,
                shape,
                modifiers,
                ..
            } => actions::shape_drawn(core, &tool_id, file_id, shape, modifiers),
            HostEvent::ObjectClicked {
                object,
                click_count,
                ..
            } => {
                if click_count >= 2
                    && let Some((file_id, _)) = overlay::parse_host_layer_id(&object.layer_id)
                {
                    actions::select(core, file_id, vec![object.object_id]);
                    core.state().settings.tab = sidebar::TAB_LAYERS;
                    self.open_sidebar()?;
                    self.refresh_sidebar();
                }
            }
            HostEvent::ObjectHovered { object, .. } => {
                let hovered = object.and_then(|o| {
                    overlay::parse_host_layer_id(&o.layer_id)
                        .map(|(file_id, _)| (file_id, o.object_id))
                });
                let changed = {
                    let mut state = core.state();
                    let changed = state.hovered != hovered;
                    state.hovered = hovered;
                    changed
                };
                if changed && self.ui().sidebar.is_some() {
                    core.request_refresh();
                }
            }
            HostEvent::ObjectEdited { object, shape } => {
                actions::object_edited(core, object, shape)
            }
            HostEvent::ObjectsDeleteRequested { objects } => actions::delete_objects(core, objects),
            HostEvent::SelectionChanged { objects } => actions::selection_changed(core, objects),
            HostEvent::Undo { entry_id } => core.db(DbJob::History {
                entry_id,
                redo: false,
            }),
            HostEvent::Redo { entry_id } => core.db(DbJob::History {
                entry_id,
                redo: true,
            }),
            HostEvent::SurfaceClosed { surface_id } => {
                let mut ui = self.ui();
                if ui.sidebar.as_ref().is_some_and(|s| s.id() == surface_id) {
                    ui.sidebar = None;
                    let _ = core
                        .host
                        .set_toolbar_button_active(tools::SIDEBAR_BUTTON, false);
                }
                if let Some(plan) = ui.imports.remove(&surface_id) {
                    core.state().imports.remove(&plan);
                }
            }
            HostEvent::DialogResult { request_id, paths } => self.dialog_result(&request_id, paths),
            HostEvent::ConfirmResult {
                request_id,
                confirmed,
            } => self.confirm_result(&request_id, confirmed),
            HostEvent::JobCancelRequested { job_id } => {
                if let Some(flag) = core.state().jobs.get(&job_id) {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            HostEvent::Posted { token } => match token.as_str() {
                REFRESH_TOKEN => {
                    core.refresh_delivered();
                    self.refresh_sidebar();
                }
                PALETTE_TOKEN => self.sync_palette(),
                other => {
                    if let Some(id) = other
                        .strip_prefix(transfer::IMPORT_TOKEN_PREFIX)
                        .and_then(|id| id.parse().ok())
                    {
                        self.open_import_dialog(id);
                    }
                }
            },
            _ => {}
        }
        Ok(())
    }

    fn context_menu(&self, context: &MenuContext) -> Vec<MenuItem> {
        let state = self.core.state();
        let file_id = context.view.file_id;
        let Some(slide) = state.slide_of(file_id) else {
            return Vec::new();
        };
        let editable = state.writable().is_ok();
        let item = |id: &str, label: &str, icon: &str, enabled: bool| MenuItem {
            item_id: id.into(),
            label: label.into(),
            icon: icon.into(),
            enabled,
            ..Default::default()
        };
        let mut items = Vec::new();
        let hit = context
            .hit
            .as_ref()
            .filter(|h| {
                overlay::parse_host_layer_id(&h.layer_id).is_some_and(|(f, _)| f == file_id)
            })
            .and_then(|h| slide.annotations.get(&h.object_id));
        if let Some(a) = hit {
            items.push(item("show", "Show in annotations panel", "", true));
            items.push(item("copy", "Copy measurements", "copy", true));
            if a.locked {
                items.push(item("unlock", "Unlock annotation", "", editable));
            } else {
                items.push(item("lock", "Lock annotation", "", editable));
            }
            items.push(MenuItem {
                separator_after: true,
                ..item("delete", "Delete annotation", "delete", editable)
            });
        }
        items.push(item("add-point", "Add point here", "", editable));
        items.push(item("add-field", "Place HPF field here", "", editable));
        items.push(item("panel", "Annotations panel", "", true));
        items
    }

    fn ui_callback(&self, surface_id: &str, name: &str, args: &[Value]) -> Result<Value, String> {
        if self.ui().imports.contains_key(surface_id) {
            return self.import_callback(surface_id, name, args);
        }
        match name {
            // Test hooks (no surface declares these callbacks): wait for the
            // workers; list the dialog requests the plugin waits for.
            "__sync" => {
                self.core.sync();
                Ok(Value::Null)
            }
            "__pending" => {
                let mut ids: Vec<String> = self.core.state().pending.keys().cloned().collect();
                ids.sort();
                Ok(json!(ids))
            }
            "row-hovered" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let file = self.core.state().active_file;
                self.highlight(file.filter(|_| !id.is_empty()).map(|f| (f, id)));
                sidebar::callback(&self.core, self, name, args)
            }
            _ => sidebar::callback(&self.core, self, name, args),
        }
    }

    fn shutdown(&self) {
        self.core.stop();
    }
}

eov_plugin_api::export_plugin!(AnnotationsPlugin);

/// The plugin's root module, for hosts that link it in-process (tests).
pub fn module() -> eov_plugin_api::abi::PluginModuleRef {
    eov_plugin_api::sdk::root_module::<AnnotationsPlugin>()
}

#[cfg(test)]
mod tests;
