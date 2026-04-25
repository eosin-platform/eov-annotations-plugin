use abi_stable::std_types::{ROption, RString};
use plugin_api::ffi::{
    HostApiVTable, HostLogLevelFFI, HostSnapshotFFI, OpenFileInfoFFI, ViewportSnapshotFFI,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::model::{AnnotationExportMetadata, ExportAnnotationLayer, LoadedFileAnnotations};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportConflictStrategy {
    Merge,
    Replace,
    Skip,
}

#[derive(Clone, Default)]
pub(crate) enum PendingImportDialog {
    #[default]
    None,
    ShaMismatchWarning,
    LayerConflict { layer_name: String },
}

#[derive(Clone, Default)]
pub(crate) struct PendingDeleteLayer {
    pub(crate) layer_id: String,
    pub(crate) layer_name: String,
}

#[derive(Clone, Default)]
pub(crate) struct PendingImport {
    pub(crate) layers: Vec<ExportAnnotationLayer>,
    pub(crate) next_index: usize,
    pub(crate) apply_to_all: Option<ImportConflictStrategy>,
    pub(crate) next_conflict_resolution: Option<ImportConflictStrategy>,
}

#[derive(Default)]
pub(crate) struct PluginState {
    pub(crate) files: HashMap<String, LoadedFileAnnotations>,
    pub(crate) active_file_path: Option<String>,
    pub(crate) active_filename: Option<String>,
    pub(crate) selected_layer_by_file: HashMap<String, String>,
    pub(crate) editing_layer_by_file: HashMap<String, String>,
    pub(crate) collapsed_layers_by_file: HashMap<String, HashSet<String>>,
    pub(crate) hidden_layers_by_file: HashMap<String, HashSet<String>>,
    pub(crate) export_metadata: AnnotationExportMetadata,
    pub(crate) export_metadata_loaded: bool,
    pub(crate) pending_delete_layer: Option<PendingDeleteLayer>,
    pub(crate) pending_import: Option<PendingImport>,
    pub(crate) pending_import_dialog: PendingImportDialog,
}

static HOST_API: Mutex<Option<HostApiVTable>> = Mutex::new(None);
static PLUGIN_STATE: OnceLock<Mutex<PluginState>> = OnceLock::new();

pub(crate) fn plugin_state() -> &'static Mutex<PluginState> {
    PLUGIN_STATE.get_or_init(|| Mutex::new(PluginState::default()))
}

pub(crate) fn host_api() -> Option<HostApiVTable> {
    *HOST_API.lock().unwrap()
}

pub(crate) fn set_host_api(host_api: HostApiVTable) {
    *HOST_API.lock().unwrap() = Some(host_api);
}

pub(crate) fn log_message(level: HostLogLevelFFI, message: impl Into<String>) {
    if let Some(host_api) = host_api() {
        (host_api.log_message)(host_api.context, level, RString::from(message.into()));
    }
}

pub(crate) fn host_snapshot() -> Result<HostSnapshotFFI, String> {
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };
    Ok((host_api.get_snapshot)(host_api.context))
}

pub(crate) fn active_file_from_snapshot(snapshot: &HostSnapshotFFI) -> Option<OpenFileInfoFFI> {
    match &snapshot.active_file {
        ROption::RSome(file) => Some(file.clone()),
        ROption::RNone => None,
    }
}

pub(crate) fn active_viewport_from_snapshot(
    snapshot: &HostSnapshotFFI,
) -> Option<ViewportSnapshotFFI> {
    match &snapshot.active_viewport {
        ROption::RSome(viewport) => Some(viewport.clone()),
        ROption::RNone => None,
    }
}

pub(crate) fn active_file_key(state: &PluginState) -> Option<&str> {
    state.active_file_path.as_deref()
}
