use plugin_api::ffi::PluginUndoRedoStateFFI;
use rusqlite::params;
use std::collections::VecDeque;

use crate::db::open_database;
use crate::model::{
    Annotation, AnnotationLayer, LoadedFileAnnotations, PointAnnotation, PolygonAnnotation,
    PolygonVertex, sort_annotation_layers,
};
use crate::operations::ensure_loaded_for_file;
use crate::state::{host_api, plugin_state};

pub(crate) const MAX_UNDO_BUFFER_SIZE: usize = 256;
pub(crate) const MAX_REDO_BUFFER_SIZE: usize = 256;

#[derive(Clone)]
pub(crate) struct FileActionContext {
    pub(crate) file_path: String,
    pub(crate) filename: String,
}

#[derive(Clone)]
pub(crate) struct CreatePointAnnotation {
    pub(crate) file: FileActionContext,
    pub(crate) annotation_layer_id: String,
    pub(crate) annotation: PointAnnotation,
    pub(crate) layer_updated_at_before: i64,
    pub(crate) layer_updated_at_after: i64,
}

#[derive(Clone)]
pub(crate) struct DeletePointAnnotation {
    pub(crate) file: FileActionContext,
    pub(crate) annotation_layer_id: String,
    pub(crate) annotation: PointAnnotation,
    pub(crate) layer_updated_at_before: i64,
    pub(crate) layer_updated_at_after: i64,
}

#[derive(Clone)]
pub(crate) struct CreatePolygonAnnotation {
    pub(crate) file: FileActionContext,
    pub(crate) annotation_layer_id: String,
    pub(crate) annotation: PolygonAnnotation,
    pub(crate) layer_updated_at_before: i64,
    pub(crate) layer_updated_at_after: i64,
}

#[derive(Clone)]
pub(crate) struct DeletePolygonAnnotation {
    pub(crate) file: FileActionContext,
    pub(crate) annotation_layer_id: String,
    pub(crate) annotation: PolygonAnnotation,
    pub(crate) layer_updated_at_before: i64,
    pub(crate) layer_updated_at_after: i64,
}

#[derive(Clone)]
pub(crate) struct CreateAnnotationLayer {
    pub(crate) file: FileActionContext,
    pub(crate) layer: AnnotationLayer,
}

#[derive(Clone)]
pub(crate) struct DeleteAnnotationLayer {
    pub(crate) file: FileActionContext,
    pub(crate) layer: AnnotationLayer,
}

#[derive(Clone)]
pub(crate) struct UpdateAnnotationLayer {
    pub(crate) file: FileActionContext,
    pub(crate) layer_id: String,
    pub(crate) old_name: String,
    pub(crate) old_color_hex: String,
    pub(crate) old_updated_at: i64,
    pub(crate) new_name: String,
    pub(crate) new_color_hex: String,
    pub(crate) new_updated_at: i64,
}

#[derive(Clone)]
pub(crate) struct MultiAction {
    pub(crate) title: String,
    pub(crate) actions: Vec<Action>,
}

#[derive(Clone)]
pub(crate) enum Action {
    CreatePointAnnotation(CreatePointAnnotation),
    DeletePointAnnotation(DeletePointAnnotation),
    CreatePolygonAnnotation(CreatePolygonAnnotation),
    DeletePolygonAnnotation(DeletePolygonAnnotation),
    CreateAnnotationLayer(CreateAnnotationLayer),
    DeleteAnnotationLayer(DeleteAnnotationLayer),
    UpdateAnnotationLayer(UpdateAnnotationLayer),
    MultiAction(MultiAction),
}

pub(crate) trait InvertAction {
    fn inverse(&self) -> Action;
}

impl InvertAction for CreatePointAnnotation {
    fn inverse(&self) -> Action {
        Action::DeletePointAnnotation(DeletePointAnnotation {
            file: self.file.clone(),
            annotation_layer_id: self.annotation_layer_id.clone(),
            annotation: self.annotation.clone(),
            layer_updated_at_before: self.layer_updated_at_after,
            layer_updated_at_after: self.layer_updated_at_before,
        })
    }
}

impl InvertAction for DeletePointAnnotation {
    fn inverse(&self) -> Action {
        Action::CreatePointAnnotation(CreatePointAnnotation {
            file: self.file.clone(),
            annotation_layer_id: self.annotation_layer_id.clone(),
            annotation: self.annotation.clone(),
            layer_updated_at_before: self.layer_updated_at_after,
            layer_updated_at_after: self.layer_updated_at_before,
        })
    }
}

impl InvertAction for CreatePolygonAnnotation {
    fn inverse(&self) -> Action {
        Action::DeletePolygonAnnotation(DeletePolygonAnnotation {
            file: self.file.clone(),
            annotation_layer_id: self.annotation_layer_id.clone(),
            annotation: self.annotation.clone(),
            layer_updated_at_before: self.layer_updated_at_after,
            layer_updated_at_after: self.layer_updated_at_before,
        })
    }
}

impl InvertAction for DeletePolygonAnnotation {
    fn inverse(&self) -> Action {
        Action::CreatePolygonAnnotation(CreatePolygonAnnotation {
            file: self.file.clone(),
            annotation_layer_id: self.annotation_layer_id.clone(),
            annotation: self.annotation.clone(),
            layer_updated_at_before: self.layer_updated_at_after,
            layer_updated_at_after: self.layer_updated_at_before,
        })
    }
}

impl InvertAction for CreateAnnotationLayer {
    fn inverse(&self) -> Action {
        Action::DeleteAnnotationLayer(DeleteAnnotationLayer {
            file: self.file.clone(),
            layer: self.layer.clone(),
        })
    }
}

impl InvertAction for DeleteAnnotationLayer {
    fn inverse(&self) -> Action {
        Action::CreateAnnotationLayer(CreateAnnotationLayer {
            file: self.file.clone(),
            layer: self.layer.clone(),
        })
    }
}

impl InvertAction for UpdateAnnotationLayer {
    fn inverse(&self) -> Action {
        Action::UpdateAnnotationLayer(UpdateAnnotationLayer {
            file: self.file.clone(),
            layer_id: self.layer_id.clone(),
            old_name: self.new_name.clone(),
            old_color_hex: self.new_color_hex.clone(),
            old_updated_at: self.new_updated_at,
            new_name: self.old_name.clone(),
            new_color_hex: self.old_color_hex.clone(),
            new_updated_at: self.old_updated_at,
        })
    }
}

impl InvertAction for MultiAction {
    fn inverse(&self) -> Action {
        Action::MultiAction(MultiAction {
            title: self.title.clone(),
            actions: self.actions.iter().rev().map(Action::inverse).collect(),
        })
    }
}

impl InvertAction for Action {
    fn inverse(&self) -> Action {
        match self {
            Action::CreatePointAnnotation(action) => action.inverse(),
            Action::DeletePointAnnotation(action) => action.inverse(),
            Action::CreatePolygonAnnotation(action) => action.inverse(),
            Action::DeletePolygonAnnotation(action) => action.inverse(),
            Action::CreateAnnotationLayer(action) => action.inverse(),
            Action::DeleteAnnotationLayer(action) => action.inverse(),
            Action::UpdateAnnotationLayer(action) => action.inverse(),
            Action::MultiAction(action) => action.inverse(),
        }
    }
}

fn push_bounded(buffer: &mut VecDeque<Action>, action: Action, max_size: usize) {
    if buffer.len() >= max_size {
        buffer.pop_front();
    }
    buffer.push_back(action);
}

pub(crate) fn publish_undo_redo_state() {
    let (undo_available, redo_available) = {
        let state = plugin_state().lock().unwrap();
        (!state.undo_buffer.is_empty(), !state.redo_buffer.is_empty())
    };

    if let Some(host_api) = host_api() {
        let _ = (host_api.set_undo_redo_state)(
            host_api.context,
            PluginUndoRedoStateFFI {
                enabled: true,
                undo_available,
                redo_available,
            },
        )
        .into_result();
    }
}

pub(crate) fn push_undo_action(action: Action) {
    {
        let mut state = plugin_state().lock().unwrap();
        push_bounded(&mut state.undo_buffer, action, MAX_UNDO_BUFFER_SIZE);
        state.redo_buffer.clear();
    }
    publish_undo_redo_state();
}

pub(crate) fn perform_undo() -> Result<(), String> {
    let action = {
        let mut state = plugin_state().lock().unwrap();
        state.undo_buffer.pop_back()
    };
    let Some(action) = action else {
        publish_undo_redo_state();
        return Ok(());
    };

    if let Err(err) = apply_action(&action.inverse()) {
        let mut state = plugin_state().lock().unwrap();
        push_bounded(&mut state.undo_buffer, action, MAX_UNDO_BUFFER_SIZE);
        publish_undo_redo_state();
        return Err(err);
    }

    {
        let mut state = plugin_state().lock().unwrap();
        push_bounded(&mut state.redo_buffer, action, MAX_REDO_BUFFER_SIZE);
    }
    publish_undo_redo_state();
    Ok(())
}

pub(crate) fn perform_redo() -> Result<(), String> {
    let action = {
        let mut state = plugin_state().lock().unwrap();
        state.redo_buffer.pop_back()
    };
    let Some(action) = action else {
        publish_undo_redo_state();
        return Ok(());
    };

    if let Err(err) = apply_action(&action) {
        let mut state = plugin_state().lock().unwrap();
        push_bounded(&mut state.redo_buffer, action, MAX_REDO_BUFFER_SIZE);
        publish_undo_redo_state();
        return Err(err);
    }

    {
        let mut state = plugin_state().lock().unwrap();
        push_bounded(&mut state.undo_buffer, action, MAX_UNDO_BUFFER_SIZE);
    }
    publish_undo_redo_state();
    Ok(())
}

fn ensure_loaded(action_file: &FileActionContext) -> Result<(), String> {
    let mut state = plugin_state().lock().unwrap();
    ensure_loaded_for_file(&mut state, &action_file.file_path, &action_file.filename)
}

fn active_loaded_file(action_file: &FileActionContext) -> Result<LoadedFileAnnotations, String> {
    let mut state = plugin_state().lock().unwrap();
    ensure_loaded_for_file(&mut state, &action_file.file_path, &action_file.filename)?;
    state
        .files
        .get(&action_file.file_path)
        .cloned()
        .ok_or_else(|| format!("file '{}' is not loaded", action_file.file_path))
}

fn insert_point(
    connection: &rusqlite::Connection,
    annotation_layer_id: &str,
    annotation: &PointAnnotation,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO annotations (id, annotation_layer_id, type, created_at, updated_at) VALUES (?1, ?2, 'point', ?3, ?4)",
            params![
                &annotation.id,
                annotation_layer_id,
                annotation.created_at,
                annotation.updated_at,
            ],
        )
        .map_err(|err| format!("failed to insert point annotation '{}': {err}", annotation.id))?;
    connection
        .execute(
            "INSERT INTO annotation_points (annotation_id, x_level0, y_level0) VALUES (?1, ?2, ?3)",
            params![&annotation.id, annotation.x_level0, annotation.y_level0],
        )
        .map_err(|err| {
            format!(
                "failed to insert point annotation geometry '{}': {err}",
                annotation.id
            )
        })?;
    Ok(())
}

fn insert_polygon(
    connection: &rusqlite::Connection,
    annotation_layer_id: &str,
    annotation: &PolygonAnnotation,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO annotations (id, annotation_layer_id, type, created_at, updated_at) VALUES (?1, ?2, 'polygon', ?3, ?4)",
            params![
                &annotation.id,
                annotation_layer_id,
                annotation.created_at,
                annotation.updated_at,
            ],
        )
        .map_err(|err| {
            format!("failed to insert polygon annotation '{}': {err}", annotation.id)
        })?;
    connection
        .execute(
            "INSERT INTO annotation_polygons (annotation_id) VALUES (?1)",
            params![&annotation.id],
        )
        .map_err(|err| {
            format!(
                "failed to insert polygon annotation shell '{}': {err}",
                annotation.id
            )
        })?;
    for (index, vertex) in annotation.vertices.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO annotation_polygon_vertices (annotation_id, vertex_index, x_level0, y_level0) VALUES (?1, ?2, ?3, ?4)",
                params![&annotation.id, index as i64, vertex.x_level0, vertex.y_level0],
            )
            .map_err(|err| {
                format!(
                    "failed to insert polygon vertex {index} for '{}': {err}",
                    annotation.id
                )
            })?;
    }
    Ok(())
}

fn apply_create_point(action: &CreatePointAnnotation) -> Result<(), String> {
    ensure_loaded(&action.file)?;
    let connection = open_database()?;
    insert_point(&connection, &action.annotation_layer_id, &action.annotation)?;
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&action.annotation_layer_id, action.layer_updated_at_after],
        )
        .map_err(|err| format!("failed to update annotation layer timestamp: {err}"))?;

    let mut state = plugin_state().lock().unwrap();
    let loaded = state
        .files
        .get_mut(&action.file.file_path)
        .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
    let layer = loaded
        .annotation_layers
        .iter_mut()
        .find(|layer| layer.id == action.annotation_layer_id)
        .ok_or_else(|| {
            format!(
                "annotation layer '{}' is not loaded",
                action.annotation_layer_id
            )
        })?;
    layer.updated_at = action.layer_updated_at_after;
    layer
        .annotations
        .insert(0, Annotation::Point(action.annotation.clone()));
    Ok(())
}

fn apply_delete_point(action: &DeletePointAnnotation) -> Result<(), String> {
    ensure_loaded(&action.file)?;
    let connection = open_database()?;
    connection
        .execute(
            "DELETE FROM annotations WHERE id = ?1",
            params![&action.annotation.id],
        )
        .map_err(|err| {
            format!(
                "failed to delete annotation '{}': {err}",
                action.annotation.id
            )
        })?;
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&action.annotation_layer_id, action.layer_updated_at_after],
        )
        .map_err(|err| format!("failed to update annotation layer timestamp: {err}"))?;

    let mut state = plugin_state().lock().unwrap();
    let loaded = state
        .files
        .get_mut(&action.file.file_path)
        .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
    let layer = loaded
        .annotation_layers
        .iter_mut()
        .find(|layer| layer.id == action.annotation_layer_id)
        .ok_or_else(|| {
            format!(
                "annotation layer '{}' is not loaded",
                action.annotation_layer_id
            )
        })?;
    layer.updated_at = action.layer_updated_at_after;
    layer.annotations.retain(|annotation| match annotation {
        Annotation::Point(point) => point.id != action.annotation.id,
        Annotation::Polygon(_) => true,
    });
    Ok(())
}

fn apply_create_polygon(action: &CreatePolygonAnnotation) -> Result<(), String> {
    ensure_loaded(&action.file)?;
    let connection = open_database()?;
    insert_polygon(&connection, &action.annotation_layer_id, &action.annotation)?;
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&action.annotation_layer_id, action.layer_updated_at_after],
        )
        .map_err(|err| format!("failed to update annotation layer timestamp: {err}"))?;

    let mut state = plugin_state().lock().unwrap();
    let loaded = state
        .files
        .get_mut(&action.file.file_path)
        .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
    let layer = loaded
        .annotation_layers
        .iter_mut()
        .find(|layer| layer.id == action.annotation_layer_id)
        .ok_or_else(|| {
            format!(
                "annotation layer '{}' is not loaded",
                action.annotation_layer_id
            )
        })?;
    layer.updated_at = action.layer_updated_at_after;
    layer
        .annotations
        .insert(0, Annotation::Polygon(action.annotation.clone()));
    Ok(())
}

fn apply_delete_polygon(action: &DeletePolygonAnnotation) -> Result<(), String> {
    ensure_loaded(&action.file)?;
    let connection = open_database()?;
    connection
        .execute(
            "DELETE FROM annotations WHERE id = ?1",
            params![&action.annotation.id],
        )
        .map_err(|err| {
            format!(
                "failed to delete annotation '{}': {err}",
                action.annotation.id
            )
        })?;
    connection
        .execute(
            "UPDATE annotation_layers SET updated_at = ?2 WHERE id = ?1",
            params![&action.annotation_layer_id, action.layer_updated_at_after],
        )
        .map_err(|err| format!("failed to update annotation layer timestamp: {err}"))?;

    let mut state = plugin_state().lock().unwrap();
    let loaded = state
        .files
        .get_mut(&action.file.file_path)
        .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
    let layer = loaded
        .annotation_layers
        .iter_mut()
        .find(|layer| layer.id == action.annotation_layer_id)
        .ok_or_else(|| {
            format!(
                "annotation layer '{}' is not loaded",
                action.annotation_layer_id
            )
        })?;
    layer.updated_at = action.layer_updated_at_after;
    layer.annotations.retain(|annotation| match annotation {
        Annotation::Point(_) => true,
        Annotation::Polygon(polygon) => polygon.id != action.annotation.id,
    });
    Ok(())
}

fn apply_create_layer(action: &CreateAnnotationLayer) -> Result<(), String> {
    let loaded_file = active_loaded_file(&action.file)?;
    let connection = open_database()?;
    connection
        .execute(
            "INSERT INTO annotation_layers (id, fingerprint, name, notes, color, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &action.layer.id,
                loaded_file.fingerprint.as_slice(),
                &action.layer.name,
                action.layer.notes.as_deref(),
                &action.layer.color_hex,
                action.layer.created_at,
                action.layer.updated_at,
            ],
        )
        .map_err(|err| format!("failed to insert annotation layer '{}': {err}", action.layer.id))?;
    for annotation in &action.layer.annotations {
        match annotation {
            Annotation::Point(point) => insert_point(&connection, &action.layer.id, point)?,
            Annotation::Polygon(polygon) => insert_polygon(&connection, &action.layer.id, polygon)?,
        }
    }

    let mut state = plugin_state().lock().unwrap();
    let loaded = state
        .files
        .get_mut(&action.file.file_path)
        .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
    loaded.annotation_layers.push(action.layer.clone());
    sort_annotation_layers(&mut loaded.annotation_layers);
    state
        .selected_layer_by_file
        .insert(action.file.file_path.clone(), action.layer.id.clone());
    Ok(())
}

fn apply_delete_layer(action: &DeleteAnnotationLayer) -> Result<(), String> {
    ensure_loaded(&action.file)?;
    let connection = open_database()?;
    connection
        .execute(
            "DELETE FROM annotation_layers WHERE id = ?1",
            params![&action.layer.id],
        )
        .map_err(|err| {
            format!(
                "failed to delete annotation layer '{}': {err}",
                action.layer.id
            )
        })?;

    let mut state = plugin_state().lock().unwrap();
    let next_selected = {
        let loaded = state
            .files
            .get_mut(&action.file.file_path)
            .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
        loaded
            .annotation_layers
            .retain(|layer| layer.id != action.layer.id);
        loaded
            .annotation_layers
            .first()
            .map(|layer| layer.id.clone())
    };
    if let Some(collapsed) = state
        .collapsed_layers_by_file
        .get_mut(&action.file.file_path)
    {
        collapsed.remove(&action.layer.id);
    }
    if let Some(hidden) = state.hidden_layers_by_file.get_mut(&action.file.file_path) {
        hidden.remove(&action.layer.id);
    }
    if state
        .editing_layer_by_file
        .get(&action.file.file_path)
        .is_some_and(|editing| editing == &action.layer.id)
    {
        state.editing_layer_by_file.remove(&action.file.file_path);
    }
    if state
        .selected_layer_by_file
        .get(&action.file.file_path)
        .is_some_and(|selected| selected == &action.layer.id)
    {
        if let Some(next_id) = next_selected {
            state
                .selected_layer_by_file
                .insert(action.file.file_path.clone(), next_id);
        } else {
            state.selected_layer_by_file.remove(&action.file.file_path);
        }
    }
    Ok(())
}

fn apply_update_layer(action: &UpdateAnnotationLayer) -> Result<(), String> {
    ensure_loaded(&action.file)?;
    let connection = open_database()?;
    connection
        .execute(
            "UPDATE annotation_layers SET name = ?2, color = ?3, updated_at = ?4 WHERE id = ?1",
            params![
                &action.layer_id,
                &action.new_name,
                &action.new_color_hex,
                action.new_updated_at,
            ],
        )
        .map_err(|err| {
            format!(
                "failed to update annotation layer '{}': {err}",
                action.layer_id
            )
        })?;

    let mut state = plugin_state().lock().unwrap();
    let loaded = state
        .files
        .get_mut(&action.file.file_path)
        .ok_or_else(|| format!("file '{}' is not loaded", action.file.file_path))?;
    let layer = loaded
        .annotation_layers
        .iter_mut()
        .find(|layer| layer.id == action.layer_id)
        .ok_or_else(|| format!("annotation layer '{}' is not loaded", action.layer_id))?;
    layer.name = action.new_name.clone();
    layer.color_hex = action.new_color_hex.clone();
    layer.updated_at = action.new_updated_at;
    sort_annotation_layers(&mut loaded.annotation_layers);
    Ok(())
}

fn apply_action(action: &Action) -> Result<(), String> {
    match action {
        Action::CreatePointAnnotation(action) => apply_create_point(action),
        Action::DeletePointAnnotation(action) => apply_delete_point(action),
        Action::CreatePolygonAnnotation(action) => apply_create_polygon(action),
        Action::DeletePolygonAnnotation(action) => apply_delete_polygon(action),
        Action::CreateAnnotationLayer(action) => apply_create_layer(action),
        Action::DeleteAnnotationLayer(action) => apply_delete_layer(action),
        Action::UpdateAnnotationLayer(action) => apply_update_layer(action),
        Action::MultiAction(action) => {
            for subaction in &action.actions {
                apply_action(subaction)?;
            }
            Ok(())
        }
    }
}

pub(crate) fn file_action_context(file_path: &str, filename: &str) -> FileActionContext {
    FileActionContext {
        file_path: file_path.to_string(),
        filename: filename.to_string(),
    }
}

pub(crate) fn point_annotation_as_polygon_vertices(
    vertices: &[plugin_api::ffi::ViewportOverlayVertexFFI],
) -> Vec<PolygonVertex> {
    vertices
        .iter()
        .map(|vertex| PolygonVertex {
            x_level0: vertex.x_level0,
            y_level0: vertex.y_level0,
        })
        .collect()
}
