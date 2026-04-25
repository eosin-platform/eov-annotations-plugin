use common::file_id::hex_digest;
use plugin_api::ffi::{
    ConfirmationDialogRequestFFI, HostToolModeFFI, ModalDialogRequestFFI, ViewportSnapshotFFI,
};
use rusqlite::params;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use uuid::Uuid;

use crate::db::{
    fingerprint_for_file, full_sha256_for_file, load_annotation_layers, load_export_metadata,
    open_database, save_export_metadata,
};
use crate::model::{
    Annotation, AnnotationExportMetadata, AnnotationLayer, ExportAnnotation,
    ExportAnnotationLayer, ExportFile, ExportPolygonVertex, LoadedFileAnnotations,
    PointAnnotation, PolygonAnnotation, PolygonVertex, choose_annotation_layer_color,
    now_unix_secs, sort_annotation_layers, unique_untitled_set_name,
};
use crate::state::{
    ImportConflictStrategy, PendingImport, PendingImportDialog, PluginState,
    active_file_from_snapshot, active_file_key, host_api, host_snapshot, plugin_state,
};

enum ImportStep {
    WaitForShaMismatchConfirmation,
    WaitForLayerConflict,
    Apply {
        active_file_path: String,
        fingerprint: [u8; 32],
        imported_layer: ExportAnnotationLayer,
        existing_layer_id: Option<String>,
        conflict_strategy: Option<ImportConflictStrategy>,
    },
    Complete,
}

fn ensure_loaded_for_file(
    state: &mut PluginState,
    file_path: &str,
    filename: &str,
) -> Result<(), String> {
    if state.files.contains_key(file_path) {
        return Ok(());
    }

    let path = Path::new(file_path);
    let fingerprint = fingerprint_for_file(path)?;
    let connection = open_database()?;
    let annotation_layers = load_annotation_layers(&connection, &fingerprint)?;
    state.files.insert(
        file_path.to_string(),
        LoadedFileAnnotations {
            file_path: file_path.to_string(),
            filename: filename.to_string(),
            fingerprint,
            annotation_layers,
        },
    );
    Ok(())
}

pub(crate) fn sync_active_file() -> Result<(), String> {
    let snapshot = host_snapshot()?;
    let mut state = plugin_state().lock().unwrap();
    let Some(active_file) = active_file_from_snapshot(&snapshot) else {
        state.active_file_path = None;
        state.active_filename = None;
        return Ok(());
    };

    let file_path = active_file.path.to_string();
    let filename = active_file.filename.to_string();
    ensure_loaded_for_file(&mut state, &file_path, &filename)?;
    state.active_file_path = Some(file_path);
    state.active_filename = Some(filename);
    Ok(())
}

pub(crate) fn ensure_loaded_for_viewport(viewport: &ViewportSnapshotFFI) -> Result<(), String> {
    let file_path = viewport.file_path.to_string();
    if file_path.is_empty() {
        return Ok(());
    }
    let filename = viewport.filename.to_string();
    ensure_loaded_for_file(&mut plugin_state().lock().unwrap(), &file_path, &filename)
}

pub(crate) fn refresh_sidebar_if_available() {
    if let Some(host_api) = host_api() {
        let _ = (host_api.refresh_sidebar)(host_api.context).into_result();
    }
}

pub(crate) fn request_render_if_available() {
    if let Some(host_api) = host_api() {
        let _ = (host_api.request_render)(host_api.context).into_result();
    }
}

fn trim_metadata_field(value: &str) -> String {
    value.trim().chars().take(255).collect()
}

pub(crate) fn ensure_export_metadata_loaded(state: &mut PluginState) -> Result<(), String> {
    if state.export_metadata_loaded {
        return Ok(());
    }

    let connection = open_database()?;
    state.export_metadata = load_export_metadata(&connection)?;
    state.export_metadata_loaded = true;
    Ok(())
}

pub(crate) fn update_export_metadata_settings(
    author: &str,
    organization: &str,
    project_name: &str,
    license: &str,
) -> Result<(), String> {
    let metadata = AnnotationExportMetadata {
        author: trim_metadata_field(author),
        organization: trim_metadata_field(organization),
        project_name: trim_metadata_field(project_name),
        license: trim_metadata_field(license),
    };

    let connection = open_database()?;
    save_export_metadata(&connection, &metadata)?;

    let mut state = plugin_state().lock().unwrap();
    state.export_metadata = metadata;
    state.export_metadata_loaded = true;
    Ok(())
}

pub(crate) fn show_metadata_settings_dialog() -> Result<(), String> {
    sync_active_file()?;
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };

    {
        let mut state = plugin_state().lock().unwrap();
        ensure_export_metadata_loaded(&mut state)?;
    }

    (host_api.show_modal_dialog)(
        host_api.context,
        ModalDialogRequestFFI {
            ui_path: "ui/metadata-settings-dialog.slint".into(),
            component: "MetadataSettingsDialog".into(),
            width_px: 560,
            height_px: 432,
        },
    )
    .into_result()
    .map_err(|err| format!("failed to show metadata settings dialog: {err}"))
}

pub(crate) fn hide_metadata_settings_dialog() -> Result<(), String> {
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };
    (host_api.hide_modal_dialog)(host_api.context)
        .into_result()
        .map_err(|err| format!("failed to hide metadata settings dialog: {err}"))
}

fn clear_pending_import(state: &mut PluginState) {
    state.pending_import = None;
    state.pending_import_dialog = PendingImportDialog::None;
}

fn layer_id_exists(connection: &rusqlite::Connection, layer_id: &str) -> Result<bool, String> {
    let exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM annotation_layers WHERE id = ?1)",
            params![layer_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|err| format!("failed to check annotation layer id '{layer_id}': {err}"))?;
    Ok(exists != 0)
}

fn annotation_id_exists(
    connection: &rusqlite::Connection,
    annotation_id: &str,
) -> Result<bool, String> {
    let exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM annotations WHERE id = ?1)",
            params![annotation_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|err| format!("failed to check annotation id '{annotation_id}': {err}"))?;
    Ok(exists != 0)
}

fn next_available_layer_id(
    connection: &rusqlite::Connection,
    preferred_id: &str,
) -> Result<String, String> {
    if !layer_id_exists(connection, preferred_id)? {
        return Ok(preferred_id.to_string());
    }
    Ok(Uuid::new_v4().to_string())
}

fn next_available_annotation_id(
    connection: &rusqlite::Connection,
    preferred_id: &str,
) -> Result<String, String> {
    if !annotation_id_exists(connection, preferred_id)? {
        return Ok(preferred_id.to_string());
    }
    Ok(Uuid::new_v4().to_string())
}

fn insert_point_annotation(
    connection: &rusqlite::Connection,
    annotation_layer_id: &str,
    id: &str,
    created_at: i64,
    updated_at: i64,
    x_level0: f64,
    y_level0: f64,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO annotations (id, annotation_layer_id, type, created_at, updated_at) VALUES (?1, ?2, 'point', ?3, ?4)",
            params![id, annotation_layer_id, created_at, updated_at],
        )
        .map_err(|err| format!("failed to insert imported point annotation '{id}': {err}"))?;
    connection
        .execute(
            "INSERT INTO annotation_points (annotation_id, x_level0, y_level0) VALUES (?1, ?2, ?3)",
            params![id, x_level0, y_level0],
        )
        .map_err(|err| format!("failed to insert imported point geometry '{id}': {err}"))?;
    Ok(())
}

fn insert_polygon_annotation(
    connection: &rusqlite::Connection,
    annotation_layer_id: &str,
    id: &str,
    created_at: i64,
    updated_at: i64,
    vertices: &[ExportPolygonVertex],
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO annotations (id, annotation_layer_id, type, created_at, updated_at) VALUES (?1, ?2, 'polygon', ?3, ?4)",
            params![id, annotation_layer_id, created_at, updated_at],
        )
        .map_err(|err| format!("failed to insert imported polygon annotation '{id}': {err}"))?;
    connection
        .execute(
            "INSERT INTO annotation_polygons (annotation_id) VALUES (?1)",
            params![id],
        )
        .map_err(|err| format!("failed to insert imported polygon shell '{id}': {err}"))?;
    for (index, vertex) in vertices.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO annotation_polygon_vertices (annotation_id, vertex_index, x_level0, y_level0) VALUES (?1, ?2, ?3, ?4)",
                params![id, index as i64, vertex.x_level0, vertex.y_level0],
            )
            .map_err(|err| format!("failed to insert imported polygon vertex {index} for '{id}': {err}"))?;
    }
    Ok(())
}

fn insert_annotation_record(
    connection: &rusqlite::Connection,
    annotation_layer_id: &str,
    annotation: &ExportAnnotation,
    skip_existing_ids: &HashSet<String>,
) -> Result<Option<Annotation>, String> {
    match annotation {
        ExportAnnotation::Point {
            id,
            created_at,
            updated_at,
            x_level0,
            y_level0,
        } => {
            if skip_existing_ids.contains(id) {
                return Ok(None);
            }
            let annotation_id = next_available_annotation_id(connection, id)?;
            insert_point_annotation(
                connection,
                annotation_layer_id,
                &annotation_id,
                *created_at,
                *updated_at,
                *x_level0,
                *y_level0,
            )?;
            Ok(Some(Annotation::Point(PointAnnotation {
                id: annotation_id,
                created_at: *created_at,
                updated_at: *updated_at,
                x_level0: *x_level0,
                y_level0: *y_level0,
            })))
        }
        ExportAnnotation::Polygon {
            id,
            created_at,
            updated_at,
            vertices,
        } => {
            if skip_existing_ids.contains(id) {
                return Ok(None);
            }
            let annotation_id = next_available_annotation_id(connection, id)?;
            insert_polygon_annotation(
                connection,
                annotation_layer_id,
                &annotation_id,
                *created_at,
                *updated_at,
                vertices,
            )?;
            Ok(Some(Annotation::Polygon(PolygonAnnotation {
                id: annotation_id,
                created_at: *created_at,
                updated_at: *updated_at,
                vertices: vertices
                    .iter()
                    .map(|vertex| PolygonVertex {
                        x_level0: vertex.x_level0,
                        y_level0: vertex.y_level0,
                    })
                    .collect(),
            })))
        }
    }
}

fn import_new_layer(
    connection: &rusqlite::Connection,
    fingerprint: &[u8; 32],
    imported_layer: &ExportAnnotationLayer,
) -> Result<AnnotationLayer, String> {
    let layer_id = next_available_layer_id(connection, &imported_layer.id)?;
    connection
        .execute(
            "INSERT INTO annotation_layers (id, fingerprint, name, notes, color, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &layer_id,
                fingerprint.as_slice(),
                &imported_layer.name,
                imported_layer.notes.as_deref(),
                &imported_layer.color_hex,
                imported_layer.created_at,
                imported_layer.updated_at,
            ],
        )
        .map_err(|err| format!("failed to insert imported annotation layer '{}': {err}", imported_layer.name))?;

    let mut annotations = Vec::new();
    let skip_existing_ids = HashSet::new();
    for annotation in &imported_layer.annotations {
        if let Some(annotation) = insert_annotation_record(connection, &layer_id, annotation, &skip_existing_ids)? {
            annotations.push(annotation);
        }
    }

    Ok(AnnotationLayer {
        id: layer_id,
        name: imported_layer.name.clone(),
        notes: imported_layer.notes.clone(),
        color_hex: imported_layer.color_hex.clone(),
        created_at: imported_layer.created_at,
        updated_at: imported_layer.updated_at,
        annotations,
    })
}

fn merge_into_existing_layer(
    connection: &rusqlite::Connection,
    target_layer: &mut AnnotationLayer,
    imported_layer: &ExportAnnotationLayer,
) -> Result<(), String> {
    let existing_ids = target_layer
        .annotations
        .iter()
        .map(|annotation| match annotation {
            Annotation::Point(point) => point.id.clone(),
            Annotation::Polygon(polygon) => polygon.id.clone(),
        })
        .collect::<HashSet<_>>();

    for annotation in &imported_layer.annotations {
        if let Some(annotation) =
            insert_annotation_record(connection, &target_layer.id, annotation, &existing_ids)?
        {
            target_layer.annotations.push(annotation);
        }
    }

    let updated_at = target_layer.updated_at.max(imported_layer.updated_at);
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&target_layer.id, updated_at],
        )
        .map_err(|err| {
            format!(
                "failed to update merged annotation layer timestamp for '{}': {err}",
                target_layer.id
            )
        })?;
    target_layer.updated_at = updated_at;
    Ok(())
}

fn apply_import_step(step: ImportStep) -> Result<(), String> {
    match step {
        ImportStep::WaitForShaMismatchConfirmation
        | ImportStep::WaitForLayerConflict
        | ImportStep::Complete => Ok(()),
        ImportStep::Apply {
            active_file_path,
            fingerprint,
            imported_layer,
            existing_layer_id,
            conflict_strategy,
        } => {
            let connection = open_database()?;
            let mut state = plugin_state().lock().unwrap();
            if state.pending_import.is_none() {
                return Err("pending import state is missing".to_string());
            }

            if let Some(strategy) = conflict_strategy {
                match strategy {
                    ImportConflictStrategy::Skip => {
                        if let Some(pending) = state.pending_import.as_mut() {
                            pending.next_index += 1;
                            pending.next_conflict_resolution = None;
                        }
                        state.pending_import_dialog = PendingImportDialog::None;
                        return Ok(());
                    }
                    ImportConflictStrategy::Replace => {
                        if let Some(existing_layer_id) = existing_layer_id.as_deref() {
                            connection
                                .execute(
                                    "DELETE FROM annotation_layers WHERE id = ?1",
                                    params![existing_layer_id],
                                )
                                .map_err(|err| {
                                    format!(
                                        "failed to replace annotation layer '{}': {err}",
                                        imported_layer.name
                                    )
                                })?;
                        }
                    }
                    ImportConflictStrategy::Merge => {}
                }
            }

            match conflict_strategy {
                Some(ImportConflictStrategy::Merge) => {
                    let Some(existing_layer_id) = existing_layer_id else {
                        return Err("merge requested without an existing layer".to_string());
                    };
                    {
                        let loaded = state
                            .files
                            .get_mut(&active_file_path)
                            .ok_or_else(|| format!("active file '{}' is not loaded", active_file_path))?;
                        let target_layer = loaded
                            .annotation_layers
                            .iter_mut()
                            .find(|layer| layer.id == existing_layer_id)
                            .ok_or_else(|| {
                                format!(
                                    "target annotation layer '{}' was not found for merge",
                                    existing_layer_id
                                )
                            })?;
                        merge_into_existing_layer(&connection, target_layer, &imported_layer)?;
                    }
                }
                Some(ImportConflictStrategy::Replace) => {
                    if let Some(existing_layer_id) = existing_layer_id.as_deref() {
                        let loaded = state
                            .files
                            .get_mut(&active_file_path)
                            .ok_or_else(|| format!("active file '{}' is not loaded", active_file_path))?;
                        loaded.annotation_layers.retain(|layer| layer.id != existing_layer_id);
                    }
                    let imported_runtime_layer =
                        import_new_layer(&connection, &fingerprint, &imported_layer)?;
                    state.selected_layer_by_file.insert(
                        active_file_path.clone(),
                        imported_runtime_layer.id.clone(),
                    );
                    let loaded = state
                        .files
                        .get_mut(&active_file_path)
                        .ok_or_else(|| format!("active file '{}' is not loaded", active_file_path))?;
                    loaded.annotation_layers.push(imported_runtime_layer);
                    sort_annotation_layers(&mut loaded.annotation_layers);
                }
                Some(ImportConflictStrategy::Skip) => {}
                None => {
                    let imported_runtime_layer =
                        import_new_layer(&connection, &fingerprint, &imported_layer)?;
                    state.selected_layer_by_file.insert(
                        active_file_path.clone(),
                        imported_runtime_layer.id.clone(),
                    );
                    let loaded = state
                        .files
                        .get_mut(&active_file_path)
                        .ok_or_else(|| format!("active file '{}' is not loaded", active_file_path))?;
                    loaded.annotation_layers.push(imported_runtime_layer);
                    sort_annotation_layers(&mut loaded.annotation_layers);
                }
            }

            if let Some(pending) = state.pending_import.as_mut() {
                pending.next_index += 1;
                pending.next_conflict_resolution = None;
            }
            state.pending_import_dialog = PendingImportDialog::None;
            Ok(())
        }
    }
}

fn next_import_step() -> Result<ImportStep, String> {
    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        clear_pending_import(&mut state);
        return Ok(ImportStep::Complete);
    };
    let fingerprint = match state.files.get(&active_file_path) {
        Some(loaded) => loaded.fingerprint,
        None => {
            clear_pending_import(&mut state);
            return Ok(ImportStep::Complete);
        }
    };
    let dialog_state = state.pending_import_dialog.clone();
    let Some(pending) = state.pending_import.as_ref() else {
        return Ok(ImportStep::Complete);
    };

    match dialog_state {
        PendingImportDialog::ShaMismatchWarning => return Ok(ImportStep::WaitForShaMismatchConfirmation),
        PendingImportDialog::LayerConflict { .. } => return Ok(ImportStep::WaitForLayerConflict),
        PendingImportDialog::None => {}
    }

    if pending.next_index >= pending.layers.len() {
        clear_pending_import(&mut state);
        return Ok(ImportStep::Complete);
    }

    let imported_layer = pending.layers[pending.next_index].clone();
    let existing_layer = state
        .files
        .get(&active_file_path)
        .expect("active file must remain loaded")
        .annotation_layers
        .iter()
        .find(|layer| {
            layer.name.eq_ignore_ascii_case(&imported_layer.name) || layer.id == imported_layer.id
        })
        .map(|layer| (layer.id.clone(), layer.name.clone()));

    if let Some((existing_layer_id, existing_layer_name)) = existing_layer {
        if let Some(strategy) = pending.apply_to_all {
            return Ok(ImportStep::Apply {
                active_file_path,
                fingerprint,
                imported_layer,
                existing_layer_id: Some(existing_layer_id),
                conflict_strategy: Some(strategy),
            });
        }

        if let Some(strategy) = state
            .pending_import
            .as_mut()
            .and_then(|pending| pending.next_conflict_resolution.take())
        {
            return Ok(ImportStep::Apply {
                active_file_path,
                fingerprint,
                imported_layer,
                existing_layer_id: Some(existing_layer_id),
                conflict_strategy: Some(strategy),
            });
        }

        state.pending_import_dialog = PendingImportDialog::LayerConflict {
            layer_name: existing_layer_name,
        };
        return Ok(ImportStep::WaitForLayerConflict);
    }

    Ok(ImportStep::Apply {
        active_file_path,
        fingerprint,
        imported_layer,
        existing_layer_id: None,
        conflict_strategy: None,
    })
}

fn continue_pending_import() -> Result<(), String> {
    loop {
        match next_import_step()? {
            step @ ImportStep::Apply { .. } => apply_import_step(step)?,
            ImportStep::WaitForShaMismatchConfirmation
            | ImportStep::WaitForLayerConflict
            | ImportStep::Complete => return Ok(()),
        }
    }
}

pub(crate) fn import_active_file_annotations() -> Result<(), String> {
    sync_active_file()?;
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };

    let import_path = match (host_api.open_file_dialog)(host_api.context, "JSON".into(), "json".into())
        .into_result()
    {
        Ok(path) => path.to_string(),
        Err(_) => return Ok(()),
    };

    let json = fs::read_to_string(&import_path)
        .map_err(|err| format!("failed to read annotation import '{}': {err}", import_path))?;
    let import_payload: ExportFile = serde_json::from_str(&json)
        .map_err(|err| format!("failed to parse annotation import '{}': {err}", import_path))?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = active_file_key(&state).map(str::to_string) else {
        return Ok(());
    };
    let imported_layers = import_payload.annotation_layers;
    let sha_matches = {
        let full_sha = full_sha256_for_file(Path::new(&active_file_path))?;
        hex_digest(&full_sha).eq_ignore_ascii_case(&import_payload.file_sha256)
    };

    state.pending_import = Some(PendingImport {
        layers: imported_layers,
        next_index: 0,
        apply_to_all: None,
        next_conflict_resolution: None,
    });
    state.pending_import_dialog = if sha_matches {
        PendingImportDialog::None
    } else {
        PendingImportDialog::ShaMismatchWarning
    };
    drop(state);

    if sha_matches {
        continue_pending_import()?;
        request_render_if_available();
    }
    refresh_sidebar_if_available();
    Ok(())
}

pub(crate) fn respond_to_import_sha_mismatch(should_import: bool) -> Result<(), String> {
    let mut state = plugin_state().lock().unwrap();
    if !matches!(state.pending_import_dialog, PendingImportDialog::ShaMismatchWarning) {
        return Ok(());
    }
    if !should_import {
        clear_pending_import(&mut state);
        drop(state);
        refresh_sidebar_if_available();
        return Ok(());
    }
    state.pending_import_dialog = PendingImportDialog::None;
    drop(state);

    continue_pending_import()?;
    refresh_sidebar_if_available();
    request_render_if_available();
    Ok(())
}

pub(crate) fn respond_to_import_layer_conflict(
    action: &str,
    apply_to_all: bool,
) -> Result<(), String> {
    let mut state = plugin_state().lock().unwrap();
    let PendingImportDialog::LayerConflict { .. } = &state.pending_import_dialog else {
        return Ok(());
    };

    let strategy = match action {
        "merge" => Some(ImportConflictStrategy::Merge),
        "replace" => Some(ImportConflictStrategy::Replace),
        "skip" => Some(ImportConflictStrategy::Skip),
        "cancel" => None,
        _ => return Err(format!("unsupported import conflict action '{action}'")),
    };

    if let Some(strategy) = strategy {
        if let Some(pending) = state.pending_import.as_mut() {
            pending.next_conflict_resolution = Some(strategy);
            if apply_to_all {
                pending.apply_to_all = Some(strategy);
            }
        }
        state.pending_import_dialog = PendingImportDialog::None;
        drop(state);
        continue_pending_import()?;
        refresh_sidebar_if_available();
        request_render_if_available();
        return Ok(());
    }

    clear_pending_import(&mut state);
    drop(state);
    refresh_sidebar_if_available();
    Ok(())
}

fn ensure_selected_layer_for_active_file(
    state: &mut PluginState,
) -> Result<Option<String>, String> {
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(None);
    };
    let Some(loaded) = state.files.get(&active_file_path).cloned() else {
        return Ok(None);
    };

    if let Some(selected_id) = state.selected_layer_by_file.get(&active_file_path)
        && loaded
            .annotation_layers
            .iter()
            .any(|set| &set.id == selected_id)
    {
        return Ok(Some(selected_id.clone()));
    }

    if let Some(existing) = loaded
        .annotation_layers
        .iter()
        .find(|set| set.name == "Untitled")
    {
        state
            .selected_layer_by_file
            .insert(active_file_path, existing.id.clone());
        return Ok(Some(existing.id.clone()));
    }

    let connection = open_database()?;
    let id = Uuid::new_v4().to_string();
    let timestamp = now_unix_secs();
    let color_hex = choose_annotation_layer_color(&loaded.annotation_layers);
    connection
		.execute(
			"INSERT INTO annotation_layers (id, fingerprint, name, notes, color, created_at, updated_at) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
			params![&id, loaded.fingerprint.as_slice(), "Untitled", &color_hex, timestamp, timestamp],
		)
		.map_err(|err| format!("failed to create untitled annotation layer: {err}"))?;

    let loaded_entry = state
        .files
        .get_mut(&active_file_path)
        .expect("loaded file missing");
    loaded_entry.annotation_layers.insert(
        0,
        AnnotationLayer {
            id: id.clone(),
            name: "Untitled".to_string(),
            notes: None,
            color_hex,
            created_at: timestamp,
            updated_at: timestamp,
            annotations: Vec::new(),
        },
    );
    state
        .selected_layer_by_file
        .insert(active_file_path, id.clone());
    Ok(Some(id))
}

pub(crate) fn create_annotation_layer_for_active_file() -> Result<(), String> {
    sync_active_file()?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(());
    };
    let Some(loaded) = state.files.get(&active_file_path).cloned() else {
        return Ok(());
    };

    let name = unique_untitled_set_name(&loaded.annotation_layers);
    let id = Uuid::new_v4().to_string();
    let timestamp = now_unix_secs();
    let color_hex = choose_annotation_layer_color(&loaded.annotation_layers);
    let connection = open_database()?;
    connection
		.execute(
			"INSERT INTO annotation_layers (id, fingerprint, name, notes, color, created_at, updated_at) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
			params![&id, loaded.fingerprint.as_slice(), &name, &color_hex, timestamp, timestamp],
		)
		.map_err(|err| format!("failed to create annotation layer '{name}': {err}"))?;

    let loaded_entry = state
        .files
        .get_mut(&active_file_path)
        .ok_or_else(|| format!("active file '{}' is not loaded", active_file_path))?;
    loaded_entry.annotation_layers.push(AnnotationLayer {
        id: id.clone(),
        name,
        notes: None,
        color_hex,
        created_at: timestamp,
        updated_at: timestamp,
        annotations: Vec::new(),
    });
    sort_annotation_layers(&mut loaded_entry.annotation_layers);
    state
        .selected_layer_by_file
        .insert(active_file_path.clone(), id.clone());
    state.editing_layer_by_file.insert(active_file_path, id);
    Ok(())
}

pub(crate) fn set_annotation_layer_visibility_for_active_file(
    set_id: &str,
    visible: bool,
) -> Result<(), String> {
    sync_active_file()?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(());
    };
    let hidden_sets = state
        .hidden_layers_by_file
        .entry(active_file_path)
        .or_default();
    if visible {
        hidden_sets.remove(set_id);
    } else {
        hidden_sets.insert(set_id.to_string());
    }
    Ok(())
}

pub(crate) fn set_annotation_layer_color_for_active_file(
    set_id: &str,
    color_hex: &str,
) -> Result<(), String> {
    sync_active_file()?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(());
    };
    state.editing_layer_by_file.remove(&active_file_path);

    let timestamp = now_unix_secs();
    let connection = open_database()?;
    connection
        .execute(
            "UPDATE annotation_layers SET color = ?2, updated_at = ?3 WHERE id = ?1",
            params![set_id, color_hex, timestamp],
        )
        .map_err(|err| format!("failed to update annotation layer color for '{set_id}': {err}"))?;

    if let Some(loaded_entry) = state.files.get_mut(&active_file_path)
        && let Some(set) = loaded_entry
            .annotation_layers
            .iter_mut()
            .find(|set| set.id == set_id)
    {
        set.color_hex = color_hex.to_string();
        set.updated_at = timestamp;
    }

    Ok(())
}

pub(crate) fn rename_annotation_layer_for_active_file(
    set_id: &str,
    new_name: &str,
) -> Result<(), String> {
    sync_active_file()?;

    let trimmed_name = new_name.trim();
    if trimmed_name.is_empty() {
        return Ok(());
    }

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(());
    };

    let timestamp = now_unix_secs();
    let connection = open_database()?;
    connection
        .execute(
            "UPDATE annotation_layers SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![set_id, trimmed_name, timestamp],
        )
        .map_err(|err| format!("failed to rename annotation layer '{set_id}': {err}"))?;

    let loaded_entry = match state.files.get_mut(&active_file_path) {
        Some(entry) => entry,
        None => return Ok(()),
    };
    if let Some(set) = loaded_entry
        .annotation_layers
        .iter_mut()
        .find(|set| set.id == set_id)
    {
        set.name = trimmed_name.to_string();
        set.updated_at = timestamp;
        sort_annotation_layers(&mut loaded_entry.annotation_layers);
    }
    Ok(())
}

pub(crate) fn delete_annotation_layer_for_active_file(set_id: &str) -> Result<(), String> {
    sync_active_file()?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(());
    };
    let connection = open_database()?;
    connection
        .execute(
            "DELETE FROM annotation_layers WHERE id = ?1",
            params![set_id],
        )
        .map_err(|err| format!("failed to delete annotation layer '{set_id}': {err}"))?;

    let next_selected = {
        let loaded_entry = match state.files.get_mut(&active_file_path) {
            Some(entry) => entry,
            None => return Ok(()),
        };
        loaded_entry
            .annotation_layers
            .retain(|set| set.id != set_id);
        loaded_entry
            .annotation_layers
            .first()
            .map(|set| set.id.clone())
    };

    if let Some(collapsed) = state.collapsed_layers_by_file.get_mut(&active_file_path) {
        collapsed.remove(set_id);
    }
    if let Some(hidden) = state.hidden_layers_by_file.get_mut(&active_file_path) {
        hidden.remove(set_id);
    }
    if let Some(editing) = state.editing_layer_by_file.get(&active_file_path)
        && editing == set_id
    {
        state.editing_layer_by_file.remove(&active_file_path);
    }
    match next_selected {
        Some(next_id) => {
            state
                .selected_layer_by_file
                .insert(active_file_path, next_id);
        }
        None => {
            state.selected_layer_by_file.remove(&active_file_path);
        }
    }
    Ok(())
}

pub(crate) fn delete_annotation_for_active_file(annotation_id: &str) -> Result<(), String> {
    sync_active_file()?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_file_path) = state.active_file_path.clone() else {
        return Ok(());
    };
    let timestamp = now_unix_secs();
    let connection = open_database()?;

    let updated_set_id = state.files.get(&active_file_path).and_then(|loaded| {
        loaded.annotation_layers.iter().find_map(|set| {
            set.annotations
                .iter()
                .any(|annotation| match annotation {
                    Annotation::Point(point) => point.id == annotation_id,
                    Annotation::Polygon(polygon) => polygon.id == annotation_id,
                })
                .then(|| set.id.clone())
        })
    });

    connection
        .execute(
            "DELETE FROM annotations WHERE id = ?1",
            params![annotation_id],
        )
        .map_err(|err| format!("failed to delete annotation '{annotation_id}': {err}"))?;

    if let Some(set_id) = updated_set_id.as_deref() {
        connection
            .execute(
                "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
                params![set_id, timestamp],
            )
            .map_err(|err| {
                format!("failed to update annotation layer timestamp for '{set_id}': {err}")
            })?;
    }

    if let Some(loaded_entry) = state.files.get_mut(&active_file_path) {
        for set in &mut loaded_entry.annotation_layers {
            let before = set.annotations.len();
            set.annotations.retain(|annotation| match annotation {
                Annotation::Point(point) => point.id != annotation_id,
                Annotation::Polygon(polygon) => polygon.id != annotation_id,
            });
            if set.annotations.len() != before {
                set.updated_at = timestamp;
                break;
            }
        }
    }

    Ok(())
}

pub(crate) fn request_delete_annotation_layer(set_id: &str, set_name: &str) -> Result<(), String> {
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };
    let request = ConfirmationDialogRequestFFI {
		title: "Delete Annotation Layer".into(),
		message: format!(
			"Are you sure you want to delete annotation layer '{set_name}'? This action cannot be undone."
		)
		.into(),
		confirm_label: "Delete Permanently".into(),
		cancel_label: "Cancel".into(),
		confirm_callback: abi_stable::std_types::ROption::RSome("delete-layer-confirmed".into()),
		confirm_args_json: abi_stable::std_types::ROption::RSome(
			serde_json::to_string(&vec![set_id])
				.unwrap_or_else(|_| "[]".to_string())
				.into(),
		),
		cancel_callback: abi_stable::std_types::ROption::RNone,
		cancel_args_json: abi_stable::std_types::ROption::RNone,
	};
    (host_api.show_confirmation_dialog)(host_api.context, request)
        .into_result()
        .map_err(|err| format!("failed to show delete annotation layer confirmation: {err}"))
}

pub(crate) fn move_point_annotation(
    viewport: &ViewportSnapshotFFI,
    annotation_id: &str,
    x_level0: f64,
    y_level0: f64,
) -> Result<(), String> {
    ensure_loaded_for_viewport(viewport)?;
    let file_path = viewport.file_path.to_string();
    let mut state = plugin_state().lock().unwrap();
    state.active_file_path = Some(file_path.clone());
    state.active_filename = Some(viewport.filename.to_string());

    let timestamp = now_unix_secs();
    let connection = open_database()?;
    connection
        .execute(
            "UPDATE annotation_points SET x_level0 = ?2, y_level0 = ?3 WHERE annotation_id = ?1",
            params![annotation_id, x_level0, y_level0],
        )
        .map_err(|err| format!("failed to move point annotation '{annotation_id}': {err}"))?;

    let mut updated_set_id = None;
    if let Some(loaded) = state.files.get_mut(&file_path) {
        for set in &mut loaded.annotation_layers {
            for annotation in &mut set.annotations {
                if let Annotation::Point(point) = annotation
                    && point.id == annotation_id
                {
                    point.x_level0 = x_level0;
                    point.y_level0 = y_level0;
                    point.updated_at = timestamp;
                    set.updated_at = timestamp;
                    updated_set_id = Some(set.id.clone());
                    break;
                }
            }
            if updated_set_id.is_some() {
                break;
            }
        }
    }

    connection
        .execute(
            "UPDATE annotations SET updated_at = ?2 WHERE id = ?1",
            params![annotation_id, timestamp],
        )
        .map_err(|err| {
            format!("failed to update annotation timestamp for '{annotation_id}': {err}")
        })?;

    if let Some(set_id) = updated_set_id {
        connection
            .execute(
                "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
                params![set_id, timestamp],
            )
            .map_err(|err| {
                format!("failed to update annotation layer timestamp after move: {err}")
            })?;
    }

    Ok(())
}

pub(crate) fn move_polygon_annotation(
    viewport: &ViewportSnapshotFFI,
    annotation_id: &str,
    vertices: &[plugin_api::ffi::ViewportOverlayVertexFFI],
) -> Result<(), String> {
    if vertices.len() < 3 {
        return Ok(());
    }

    ensure_loaded_for_viewport(viewport)?;
    let file_path = viewport.file_path.to_string();
    let mut state = plugin_state().lock().unwrap();
    state.active_file_path = Some(file_path.clone());
    state.active_filename = Some(viewport.filename.to_string());

    let timestamp = now_unix_secs();
    let connection = open_database()?;
    connection
        .execute(
            "DELETE FROM annotation_polygon_vertices WHERE annotation_id = ?1",
            params![annotation_id],
        )
        .map_err(|err| format!("failed to clear polygon vertices for '{annotation_id}': {err}"))?;
    for (index, vertex) in vertices.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO annotation_polygon_vertices (annotation_id, vertex_index, x_level0, y_level0) VALUES (?1, ?2, ?3, ?4)",
                params![annotation_id, index as i64, vertex.x_level0, vertex.y_level0],
            )
            .map_err(|err| format!("failed to update polygon vertex {index} for '{annotation_id}': {err}"))?;
    }

    let mut updated_set_id = None;
    if let Some(loaded) = state.files.get_mut(&file_path) {
        for set in &mut loaded.annotation_layers {
            for annotation in &mut set.annotations {
                if let Annotation::Polygon(polygon) = annotation
                    && polygon.id == annotation_id
                {
                    polygon.vertices = vertices
                        .iter()
                        .map(|vertex| PolygonVertex {
                            x_level0: vertex.x_level0,
                            y_level0: vertex.y_level0,
                        })
                        .collect();
                    polygon.updated_at = timestamp;
                    set.updated_at = timestamp;
                    updated_set_id = Some(set.id.clone());
                    break;
                }
            }
            if updated_set_id.is_some() {
                break;
            }
        }
    }

    connection
        .execute(
            "UPDATE annotations SET updated_at = ?2 WHERE id = ?1",
            params![annotation_id, timestamp],
        )
        .map_err(|err| {
            format!("failed to update polygon annotation timestamp for '{annotation_id}': {err}")
        })?;

    if let Some(set_id) = updated_set_id {
        connection
            .execute(
                "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
                params![set_id, timestamp],
            )
            .map_err(|err| {
                format!("failed to update annotation layer timestamp after polygon move: {err}")
            })?;
    }

    Ok(())
}

pub(crate) fn start_point_annotation_flow() -> Result<(), String> {
    sync_active_file()?;
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };
    (host_api.set_active_tool)(host_api.context, HostToolModeFFI::PointAnnotation)
        .into_result()
        .map_err(|err| format!("failed to activate point annotation tool: {err}"))?;
    refresh_sidebar_if_available();
    request_render_if_available();
    Ok(())
}

pub(crate) fn start_polygon_annotation_flow() -> Result<(), String> {
    sync_active_file()?;
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };
    (host_api.set_active_tool)(host_api.context, HostToolModeFFI::PolygonAnnotation)
        .into_result()
        .map_err(|err| format!("failed to activate polygon annotation tool: {err}"))?;
    refresh_sidebar_if_available();
    request_render_if_available();
    Ok(())
}

pub(crate) fn export_active_file_annotations() -> Result<(), String> {
    sync_active_file()?;
    let Some(host_api) = host_api() else {
        return Err("host API is not available".to_string());
    };

    let export_payload = {
        let mut state = plugin_state().lock().unwrap();
        ensure_export_metadata_loaded(&mut state)?;
        let Some(active_path) = active_file_key(&state) else {
            return Ok(());
        };
        let loaded = state
            .files
            .get(active_path)
            .ok_or_else(|| format!("active file '{}' is not loaded", active_path))?;
        ExportFile {
            file_path: loaded.file_path.clone(),
            file_sha256: hex_digest(&full_sha256_for_file(Path::new(&loaded.file_path))?),
            metadata: state.export_metadata.clone(),
            annotation_layers: loaded
                .annotation_layers
                .iter()
                .map(|set| ExportAnnotationLayer {
                    id: set.id.clone(),
                    name: set.name.clone(),
                    notes: set.notes.clone(),
                    color_hex: set.color_hex.clone(),
                    created_at: set.created_at,
                    updated_at: set.updated_at,
                    annotations: set
                        .annotations
                        .iter()
                        .map(|annotation| match annotation {
                            Annotation::Point(point) => ExportAnnotation::Point {
                                id: point.id.clone(),
                                created_at: point.created_at,
                                updated_at: point.updated_at,
                                x_level0: point.x_level0,
                                y_level0: point.y_level0,
                            },
                            Annotation::Polygon(polygon) => ExportAnnotation::Polygon {
                                id: polygon.id.clone(),
                                created_at: polygon.created_at,
                                updated_at: polygon.updated_at,
                                vertices: polygon
                                    .vertices
                                    .iter()
                                    .map(|vertex| ExportPolygonVertex {
                                        x_level0: vertex.x_level0,
                                        y_level0: vertex.y_level0,
                                    })
                                    .collect(),
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    };

    let default_file_name = {
        let state = plugin_state().lock().unwrap();
        state
            .active_file_path
            .as_deref()
            .and_then(|path| state.files.get(path))
            .map(|loaded| format!("{}_annotations.json", loaded.filename))
            .unwrap_or_else(|| {
                let filename = export_payload
                    .file_path
                    .rsplit_once(std::path::MAIN_SEPARATOR)
                    .map(|(_, name)| name.to_string())
                    .unwrap_or_else(|| export_payload.file_path.clone());
                format!("{}_annotations.json", filename)
            })
    };

    let save_path = match (host_api.save_file_dialog)(
        host_api.context,
        default_file_name.into(),
        "JSON".into(),
        "json".into(),
    )
    .into_result()
    {
        Ok(path) => path.to_string(),
        Err(_) => return Ok(()),
    };

    let json = serde_json::to_string_pretty(&export_payload)
        .map_err(|err| format!("failed to serialize annotation export: {err}"))?;
    fs::write(&save_path, json)
        .map_err(|err| format!("failed to write annotation export '{}': {err}", save_path))?;
    Ok(())
}

pub(crate) fn persist_point_annotation(
    viewport: &ViewportSnapshotFFI,
    x_level0: f64,
    y_level0: f64,
) -> Result<(), String> {
    ensure_loaded_for_viewport(viewport)?;
    let file_path = viewport.file_path.to_string();
    let mut state = plugin_state().lock().unwrap();
    state.active_file_path = Some(file_path.clone());
    state.active_filename = Some(viewport.filename.to_string());
    let Some(annotation_layer_id) = ensure_selected_layer_for_active_file(&mut state)? else {
        return Ok(());
    };

    let annotation_id = Uuid::new_v4().to_string();
    let timestamp = now_unix_secs();
    let connection = open_database()?;
    connection
		.execute(
			"INSERT INTO annotations (id, annotation_layer_id, type, created_at, updated_at) VALUES (?1, ?2, 'point', ?3, ?4)",
			params![&annotation_id, &annotation_layer_id, timestamp, timestamp],
		)
		.map_err(|err| format!("failed to insert point annotation: {err}"))?;
    connection
        .execute(
            "INSERT INTO annotation_points (annotation_id, x_level0, y_level0) VALUES (?1, ?2, ?3)",
            params![&annotation_id, x_level0, y_level0],
        )
        .map_err(|err| format!("failed to insert point annotation geometry: {err}"))?;
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&annotation_layer_id, timestamp],
        )
        .map_err(|err| format!("failed to update annotation layer timestamp: {err}"))?;

    let loaded = state
        .files
        .get_mut(&file_path)
        .ok_or_else(|| format!("file '{}' is not loaded in plugin state", file_path))?;
    let set = loaded
        .annotation_layers
        .iter_mut()
        .find(|set| set.id == annotation_layer_id)
        .ok_or_else(|| format!("annotation layer '{}' is not loaded", annotation_layer_id))?;
    set.updated_at = timestamp;
    set.annotations.insert(
        0,
        Annotation::Point(PointAnnotation {
            id: annotation_id,
            created_at: timestamp,
            updated_at: timestamp,
            x_level0,
            y_level0,
        }),
    );
    Ok(())
}

pub(crate) fn persist_polygon_annotation(
    viewport: &ViewportSnapshotFFI,
    vertices: &[plugin_api::ffi::ViewportOverlayVertexFFI],
) -> Result<(), String> {
    if vertices.len() < 3 {
        return Ok(());
    }

    ensure_loaded_for_viewport(viewport)?;
    let file_path = viewport.file_path.to_string();
    let mut state = plugin_state().lock().unwrap();
    state.active_file_path = Some(file_path.clone());
    state.active_filename = Some(viewport.filename.to_string());
    let Some(annotation_layer_id) = ensure_selected_layer_for_active_file(&mut state)? else {
        return Ok(());
    };

    let annotation_id = Uuid::new_v4().to_string();
    let timestamp = now_unix_secs();
    let connection = open_database()?;
    connection
        .execute(
            "INSERT INTO annotations (id, annotation_layer_id, type, created_at, updated_at) VALUES (?1, ?2, 'polygon', ?3, ?4)",
            params![&annotation_id, &annotation_layer_id, timestamp, timestamp],
        )
        .map_err(|err| format!("failed to insert polygon annotation: {err}"))?;
    connection
        .execute(
            "INSERT INTO annotation_polygons (annotation_id) VALUES (?1)",
            params![&annotation_id],
        )
        .map_err(|err| format!("failed to insert polygon annotation shell: {err}"))?;
    for (index, vertex) in vertices.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO annotation_polygon_vertices (annotation_id, vertex_index, x_level0, y_level0) VALUES (?1, ?2, ?3, ?4)",
                params![&annotation_id, index as i64, vertex.x_level0, vertex.y_level0],
            )
            .map_err(|err| format!("failed to insert polygon vertex {index}: {err}"))?;
    }
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&annotation_layer_id, timestamp],
        )
        .map_err(|err| format!("failed to update annotation layer timestamp: {err}"))?;

    let loaded = state
        .files
        .get_mut(&file_path)
        .ok_or_else(|| format!("file '{}' is not loaded in plugin state", file_path))?;
    let set = loaded
        .annotation_layers
        .iter_mut()
        .find(|set| set.id == annotation_layer_id)
        .ok_or_else(|| format!("annotation layer '{}' is not loaded", annotation_layer_id))?;
    set.updated_at = timestamp;
    set.annotations.insert(
        0,
        Annotation::Polygon(PolygonAnnotation {
            id: annotation_id,
            created_at: timestamp,
            updated_at: timestamp,
            vertices: vertices
                .iter()
                .map(|vertex| PolygonVertex {
                    x_level0: vertex.x_level0,
                    y_level0: vertex.y_level0,
                })
                .collect(),
        }),
    );
    Ok(())
}
