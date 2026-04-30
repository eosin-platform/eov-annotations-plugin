use abi_stable::std_types::RVec;
use plugin_api::ffi::{HostLogLevelFFI, UiPropertyFFI};

use crate::model::{
    Annotation, AnnotationMetadataEntry, SidebarTreeRow, annotation_label, hex_color_to_rgb,
};
use crate::operations::{
    add_selected_annotation_metadata_row,
    cancel_annotation_layer_rename_for_active_file, cancel_pending_delete_annotation_layer,
    confirm_pending_delete_annotation_layer, create_annotation_layer_for_active_file,
    delete_annotation_for_active_file, ensure_export_metadata_loaded,
    export_active_file_annotations, hide_metadata_settings_dialog, import_active_file_annotations,
    refresh_sidebar_if_available, rename_annotation_layer_for_active_file,
    request_delete_annotation_layer, request_render_if_available, respond_to_import_layer_conflict,
    respond_to_import_sha_mismatch, remove_selected_annotation_metadata_row,
    set_annotation_layer_color_for_active_file, set_annotation_layer_visibility_for_active_file,
    show_metadata_settings_dialog, sync_active_file, update_export_metadata_settings,
    update_selected_annotation_metadata_row,
};
use crate::state::{
    PendingImportDialog, PluginState, active_file_key, active_viewport_from_snapshot, host_api,
    host_snapshot, log_message, plugin_state,
};

fn sidebar_rows(state: &PluginState) -> Vec<SidebarTreeRow> {
    let Some(active_path) = active_file_key(state) else {
        return Vec::new();
    };
    let Some(loaded) = state.files.get(active_path) else {
        return Vec::new();
    };
    let selected_layer_id = state.selected_layer_by_file.get(active_path);
    let selected_annotation_id = state.selected_annotation_by_file.get(active_path);
    let collapsed_layers = state.collapsed_layers_by_file.get(active_path);
    let hidden_layers = state.hidden_layers_by_file.get(active_path);

    let mut rows = Vec::new();
    for layer in &loaded.annotation_layers {
        let is_collapsed = collapsed_layers.is_some_and(|collapsed| collapsed.contains(&layer.id));
        let is_visible = !hidden_layers.is_some_and(|hidden| hidden.contains(&layer.id));
        let (color_r, color_g, color_b) = hex_color_to_rgb(&layer.color_hex);
        rows.push(SidebarTreeRow {
            row_id: layer.id.clone(),
            parent_layer_id: layer.id.clone(),
            label: layer.name.clone(),
            annotation_count: layer.annotations.len() as i32,
            indent: 0,
            is_layer: true,
            is_collapsed,
            is_selected: selected_annotation_id.is_none()
                && selected_layer_id.is_some_and(|selected| selected == &layer.id),
            visible: is_visible,
            color_r: color_r as i32,
            color_g: color_g as i32,
            color_b: color_b as i32,
        });

        if !is_collapsed {
            for annotation in &layer.annotations {
                let annotation_id = match annotation {
                    Annotation::Point(point) => point.id.clone(),
                    Annotation::Polygon(polygon) => polygon.id.clone(),
                };
                let is_selected = selected_annotation_id
                    .is_some_and(|selected| selected == &annotation_id);
                rows.push(SidebarTreeRow {
                    row_id: annotation_id,
                    parent_layer_id: layer.id.clone(),
                    label: annotation_label(annotation),
                    annotation_count: 0,
                    indent: 1,
                    is_layer: false,
                    is_collapsed: false,
                    is_selected,
                    visible: is_visible,
                    color_r: color_r as i32,
                    color_g: color_g as i32,
                    color_b: color_b as i32,
                });
            }
        }
    }
    rows
}

fn parse_callback_args(args_json: &str) -> Vec<serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(args_json) {
        Ok(serde_json::Value::Array(values)) => values,
        _ => Vec::new(),
    }
}

fn selected_layer_name(state: &PluginState) -> String {
    state
        .active_file_path
        .as_deref()
        .and_then(|path| {
            let selected_id = state.selected_layer_by_file.get(path)?;
            state.files.get(path).and_then(|loaded| {
                loaded
                    .annotation_layers
                    .iter()
                    .find(|set| &set.id == selected_id)
                    .map(|set| set.name.clone())
            })
        })
        .unwrap_or_default()
}

fn selected_annotation_name(state: &PluginState) -> String {
    state
        .active_file_path
        .as_deref()
        .and_then(|path| {
            let selected_id = state.selected_annotation_by_file.get(path)?;
            state.files.get(path).and_then(|loaded| {
                loaded.annotation_layers.iter().find_map(|layer| {
                    layer.annotations.iter().find_map(|annotation| match annotation {
                        Annotation::Point(point) if &point.id == selected_id => {
                            Some(annotation_label(annotation))
                        }
                        Annotation::Polygon(polygon) if &polygon.id == selected_id => {
                            Some(annotation_label(annotation))
                        }
                        _ => None,
                    })
                })
            })
        })
        .unwrap_or_default()
}

fn selected_annotation_metadata_rows(state: &PluginState) -> Vec<AnnotationMetadataEntry> {
    state
        .active_file_path
        .as_deref()
        .and_then(|path| {
            let selected_id = state.selected_annotation_by_file.get(path)?;
            state.files.get(path).and_then(|loaded| {
                loaded.annotation_layers.iter().find_map(|layer| {
                    layer.annotations.iter().find_map(|annotation| match annotation {
                        Annotation::Point(point) if &point.id == selected_id => {
                            Some(point.metadata.clone())
                        }
                        Annotation::Polygon(polygon) if &polygon.id == selected_id => {
                            Some(polygon.metadata.clone())
                        }
                        _ => None,
                    })
                })
            })
        })
        .unwrap_or_default()
}

fn select_sidebar_row(row_id: &str) -> Result<(), String> {
    sync_active_file()?;

    {
        let mut state = plugin_state().lock().unwrap();
        let Some(active_path) = active_file_key(&state).map(str::to_string) else {
            return Ok(());
        };

        let is_layer = state.files.get(&active_path).is_some_and(|loaded| {
            loaded
                .annotation_layers
                .iter()
                .any(|layer| layer.id == row_id)
        });
        if is_layer {
            state
                .selected_layer_by_file
                .insert(active_path.clone(), row_id.to_string());
            state.selected_annotation_by_file.remove(&active_path);
            return Ok(());
        }

        let target = state.files.get(&active_path).and_then(|loaded| {
            loaded.annotation_layers.iter().find_map(|set| {
                set.annotations
                    .iter()
                    .find_map(|annotation| match annotation {
                        Annotation::Point(point) if point.id == row_id => {
                            Some((set.id.clone(), point.x_level0, point.y_level0))
                        }
                        Annotation::Polygon(polygon) if polygon.id == row_id => {
                            let min_x = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.x_level0)
                                .fold(f64::INFINITY, f64::min);
                            let min_y = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.y_level0)
                                .fold(f64::INFINITY, f64::min);
                            let max_x = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.x_level0)
                                .fold(f64::NEG_INFINITY, f64::max);
                            let max_y = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.y_level0)
                                .fold(f64::NEG_INFINITY, f64::max);
                            Some((set.id.clone(), (min_x + max_x) * 0.5, (min_y + max_y) * 0.5))
                        }
                        _ => None,
                    })
            })
        });

        if let Some((layer_id, _, _)) = target.as_ref() {
            state
                .selected_layer_by_file
                .insert(active_path.clone(), layer_id.clone());
            state
                .selected_annotation_by_file
                .insert(active_path.clone(), row_id.to_string());
        }
    }

    Ok(())
}

fn frame_sidebar_annotation(row_id: &str) -> Result<(), String> {
    sync_active_file()?;

    let annotation_bounds = {
        let state = plugin_state().lock().unwrap();
        let Some(active_path) = active_file_key(&state).map(str::to_string) else {
            return Ok(());
        };

        state.files.get(&active_path).and_then(|loaded| {
            loaded.annotation_layers.iter().find_map(|set| {
                set.annotations
                    .iter()
                    .find_map(|annotation| match annotation {
                        Annotation::Point(point) if point.id == row_id => {
                            Some((point.x_level0, point.y_level0, 0.0, 0.0, true))
                        }
                        Annotation::Polygon(polygon) if polygon.id == row_id => {
                            let min_x = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.x_level0)
                                .fold(f64::INFINITY, f64::min);
                            let min_y = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.y_level0)
                                .fold(f64::INFINITY, f64::min);
                            let max_x = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.x_level0)
                                .fold(f64::NEG_INFINITY, f64::max);
                            let max_y = polygon
                                .vertices
                                .iter()
                                .map(|vertex| vertex.y_level0)
                                .fold(f64::NEG_INFINITY, f64::max);
                            Some((min_x, min_y, max_x - min_x, max_y - min_y, false))
                        }
                        _ => None,
                    })
            })
        })
    };

    let Some((x, y, width, height, is_point)) = annotation_bounds else {
        return Ok(());
    };

    let snapshot = host_snapshot()?;
    let Some(active_viewport) = active_viewport_from_snapshot(&snapshot) else {
        return Ok(());
    };
    let target_zoom = if is_point { 1.9 } else { 1.2 };
    let padding_factor = 1.1;
    let target_visible_width = (active_viewport.width / (target_zoom * padding_factor)).max(1.0);
    let target_visible_height =
        (active_viewport.height / (target_zoom * padding_factor)).max(1.0);
    let frame_width = if is_point {
        target_visible_width
    } else {
        width.max(target_visible_width)
    };
    let frame_height = if is_point {
        target_visible_height
    } else {
        height.max(target_visible_height)
    };
    let center_x = if is_point { x } else { x + width * 0.5 };
    let center_y = if is_point { y } else { y + height * 0.5 };
    let frame_x = center_x - frame_width * 0.5;
    let frame_y = center_y - frame_height * 0.5;
    let Some(host_api) = host_api() else {
        return Ok(());
    };
    (host_api.frame_active_rect)(
        host_api.context,
        frame_x,
        frame_y,
        frame_width,
        frame_height,
    )
    .into_result()
    .map_err(|err| format!("failed to frame annotation '{row_id}': {err}"))
}

fn toggle_set_for_active_file(set_id: &str) -> Result<(), String> {
    sync_active_file()?;

    let mut state = plugin_state().lock().unwrap();
    let Some(active_path) = active_file_key(&state).map(str::to_string) else {
        return Ok(());
    };
    let collapsed = state
        .collapsed_layers_by_file
        .entry(active_path)
        .or_default();
    if !collapsed.insert(set_id.to_string()) {
        collapsed.remove(set_id);
    }
    Ok(())
}

pub(crate) fn on_sidebar_callback(callback_name: &str, args_json: &str) {
    let args = parse_callback_args(args_json);

    let result = match callback_name {
        "export-clicked" => export_active_file_annotations(),
        "import-clicked" => import_active_file_annotations(),
        "create-layer-clicked" => create_annotation_layer_for_active_file().map(|_| {
            refresh_sidebar_if_available();
        }),
        "metadata-settings-requested" => show_metadata_settings_dialog(),
        "metadata-settings-confirmed" => {
            let Some(serde_json::Value::String(author)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::String(organization)) = args.get(1) else {
                return;
            };
            let Some(serde_json::Value::String(project_name)) = args.get(2) else {
                return;
            };
            let Some(serde_json::Value::String(license)) = args.get(3) else {
                return;
            };
            update_export_metadata_settings(author, organization, project_name, license).and_then(
                |_| {
                    hide_metadata_settings_dialog()?;
                    refresh_sidebar_if_available();
                    Ok(())
                },
            )
        }
        "metadata-settings-cancelled" => hide_metadata_settings_dialog(),
        "import-sha-warning-decided" => {
            let Some(serde_json::Value::Bool(should_import)) = args.first() else {
                return;
            };
            respond_to_import_sha_mismatch(*should_import)
        }
        "import-conflict-decided" => {
            let Some(serde_json::Value::String(action)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::Bool(apply_to_all)) = args.get(1) else {
                return;
            };
            respond_to_import_layer_conflict(action, *apply_to_all)
        }
        "rename-layer-committed" => {
            let Some(serde_json::Value::String(set_id)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::String(new_name)) = args.get(1) else {
                return;
            };
            rename_annotation_layer_for_active_file(set_id, new_name).map(|_| {
                refresh_sidebar_if_available();
            })
        }
        "rename-layer-cancelled" => cancel_annotation_layer_rename_for_active_file().map(|_| {
            refresh_sidebar_if_available();
        }),
        "delete-layer-confirmed" => confirm_pending_delete_annotation_layer(),
        "delete-layer-cancelled" => cancel_pending_delete_annotation_layer(),
        "request-delete-layer" => {
            let Some(serde_json::Value::String(set_id)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::String(set_name)) = args.get(1) else {
                return;
            };
            request_delete_annotation_layer(set_id, set_name)
        }
        "delete-annotation-clicked" => {
            let Some(serde_json::Value::String(annotation_id)) = args.first() else {
                return;
            };
            delete_annotation_for_active_file(annotation_id).map(|_| {
                refresh_sidebar_if_available();
                request_render_if_available();
            })
        }
        "source-selected" => Ok(()),
        "row-clicked" => {
            let Some(serde_json::Value::String(row_id)) = args.first() else {
                return;
            };
            select_sidebar_row(row_id).map(|_| {
                refresh_sidebar_if_available();
                request_render_if_available();
            })
        }
        "row-double-clicked" => {
            let Some(serde_json::Value::String(row_id)) = args.first() else {
                return;
            };
            select_sidebar_row(row_id).and_then(|_| frame_sidebar_annotation(row_id)).map(|_| {
                refresh_sidebar_if_available();
                request_render_if_available();
            })
        }
        "metadata-row-added" => add_selected_annotation_metadata_row().map(|_| {
            refresh_sidebar_if_available();
        }),
        "metadata-row-removed" => {
            let Some(serde_json::Value::Number(row_index)) = args.first() else {
                return;
            };
            let Some(row_index) = row_index.as_u64() else {
                return;
            };
            remove_selected_annotation_metadata_row(row_index as usize).map(|_| {
                refresh_sidebar_if_available();
            })
        }
        "metadata-row-key-changed" => {
            let Some(serde_json::Value::Number(row_index)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::String(key)) = args.get(1) else {
                return;
            };
            let Some(row_index) = row_index.as_u64() else {
                return;
            };
            update_selected_annotation_metadata_row(row_index as usize, Some(key), None)
        }
        "metadata-row-value-changed" => {
            let Some(serde_json::Value::Number(row_index)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::String(value)) = args.get(1) else {
                return;
            };
            let Some(row_index) = row_index.as_u64() else {
                return;
            };
            update_selected_annotation_metadata_row(row_index as usize, None, Some(value))
        }
        "toggle-layer" => {
            let Some(serde_json::Value::String(set_id)) = args.first() else {
                return;
            };
            toggle_set_for_active_file(set_id).map(|_| {
                refresh_sidebar_if_available();
            })
        }
        "toggle-layer-visibility" => {
            let Some(serde_json::Value::String(set_id)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::Bool(visible)) = args.get(1) else {
                return;
            };
            set_annotation_layer_visibility_for_active_file(set_id, *visible).map(|_| {
                refresh_sidebar_if_available();
                request_render_if_available();
            })
        }
        "set-layer-color" => {
            let Some(serde_json::Value::String(set_id)) = args.first() else {
                return;
            };
            let Some(serde_json::Value::String(color_hex)) = args.get(1) else {
                return;
            };
            set_annotation_layer_color_for_active_file(set_id, color_hex).map(|_| {
                refresh_sidebar_if_available();
                request_render_if_available();
            })
        }
        _ => Ok(()),
    };

    if let Err(err) = result {
        log_message(HostLogLevelFFI::Error, err);
    }
}

pub(crate) fn get_sidebar_properties() -> RVec<UiPropertyFFI> {
    if let Err(err) = sync_active_file() {
        log_message(HostLogLevelFFI::Error, err);
    }

    {
        let mut state = plugin_state().lock().unwrap();
        if let Err(err) = ensure_export_metadata_loaded(&mut state) {
            log_message(HostLogLevelFFI::Error, err);
        }
    }

    let state = plugin_state().lock().unwrap();
    let rows = sidebar_rows(&state);
    let editing_layer_id = state
        .active_file_path
        .as_deref()
        .and_then(|path| state.editing_layer_by_file.get(path).cloned())
        .unwrap_or_default();
    let empty_state = if state.active_file_path.is_none() {
        "Open a slide to view its annotation layers.".to_string()
    } else if rows.is_empty() {
        "No annotation layers for this slide yet.".to_string()
    } else {
        String::new()
    };
    let (show_sha_mismatch_warning, show_import_conflict_dialog, import_conflict_layer_name) =
        match &state.pending_import_dialog {
            PendingImportDialog::None => (false, false, String::new()),
            PendingImportDialog::ShaMismatchWarning => (true, false, String::new()),
            PendingImportDialog::LayerConflict { layer_name } => (false, true, layer_name.clone()),
        };
    let selected_annotation_name = selected_annotation_name(&state);
    let selected_annotation_metadata = selected_annotation_metadata_rows(&state);

    RVec::from(vec![
        UiPropertyFFI {
            name: "source-options".into(),
            json_value: "[\"Local\"]".into(),
        },
        UiPropertyFFI {
            name: "source-index".into(),
            json_value: "0".into(),
        },
        UiPropertyFFI {
            name: "tree-items".into(),
            json_value: serde_json::to_string(&rows)
                .unwrap_or_else(|_| "[]".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "empty-state-text".into(),
            json_value: serde_json::to_string(&empty_state)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "can-export".into(),
            json_value: (state.active_file_path.is_some()).to_string().into(),
        },
        UiPropertyFFI {
            name: "can-import".into(),
            json_value: (state.active_file_path.is_some()).to_string().into(),
        },
        UiPropertyFFI {
            name: "can-delete-set".into(),
            json_value: state
                .active_file_path
                .as_deref()
                .and_then(|path| state.selected_layer_by_file.get(path))
                .is_some()
                .to_string()
                .into(),
        },
        UiPropertyFFI {
            name: "selected-layer-id".into(),
            json_value: serde_json::to_string(
                &state
                    .active_file_path
                    .as_deref()
                    .and_then(|path| state.selected_layer_by_file.get(path).cloned())
                    .unwrap_or_default(),
            )
            .unwrap_or_else(|_| "\"\"".to_string())
            .into(),
        },
        UiPropertyFFI {
            name: "editing-layer-id".into(),
            json_value: serde_json::to_string(&editing_layer_id)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "selected-layer-name".into(),
            json_value: serde_json::to_string(&selected_layer_name(&state))
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "has-selected-annotation".into(),
            json_value: state
                .active_file_path
                .as_deref()
                .and_then(|path| state.selected_annotation_by_file.get(path))
                .is_some()
                .to_string()
                .into(),
        },
        UiPropertyFFI {
            name: "selected-annotation-name".into(),
            json_value: serde_json::to_string(&selected_annotation_name)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "annotation-metadata-items".into(),
            json_value: serde_json::to_string(&selected_annotation_metadata)
                .unwrap_or_else(|_| "[]".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "show-sha-mismatch-warning".into(),
            json_value: show_sha_mismatch_warning.to_string().into(),
        },
        UiPropertyFFI {
            name: "show-import-conflict-dialog".into(),
            json_value: show_import_conflict_dialog.to_string().into(),
        },
        UiPropertyFFI {
            name: "import-conflict-layer-name".into(),
            json_value: serde_json::to_string(&import_conflict_layer_name)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "metadata-author".into(),
            json_value: serde_json::to_string(&state.export_metadata.author)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "metadata-organization".into(),
            json_value: serde_json::to_string(&state.export_metadata.organization)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "metadata-project-name".into(),
            json_value: serde_json::to_string(&state.export_metadata.project_name)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "metadata-license".into(),
            json_value: serde_json::to_string(&state.export_metadata.license)
                .unwrap_or_else(|_| "\"\"".to_string())
                .into(),
        },
        UiPropertyFFI {
            name: "pending-delete-layer-name".into(),
            json_value: serde_json::to_string(
                &state
                    .pending_delete_layer
                    .as_ref()
                    .map(|pending| pending.layer_name.clone())
                    .unwrap_or_default(),
            )
            .unwrap_or_else(|_| "\"\"".to_string())
            .into(),
        },
    ])
}
