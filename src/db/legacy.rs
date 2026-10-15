//! Migration from the unversioned pre-1.0 schema.
//!
//! The old tables are kept, renamed with a `legacy_v1_` prefix, so nothing
//! is lost if the migration ever needs to be redone.

use super::schema::SCHEMA_V2;
use super::{DbResult, OpenReport};
use crate::model::{Bounds, Color, Geometry, PALETTE, clean_name};
use rusqlite::{Connection, params};
use std::collections::HashMap;

const LEGACY_TABLES: &[&str] = &[
    "annotation_layers",
    "annotations",
    "annotation_points",
    "annotation_ellipses",
    "annotation_polygons",
    "annotation_polygon_vertices",
    "annotation_bitmasks",
    "annotation_bitmask_strokes",
    "annotation_bitmask_stroke_points",
    "annotation_metadata",
    "export_metadata_settings",
];

fn exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |r| r.get(0),
    )
}

/// Legacy timestamps are seconds; new ones are milliseconds.
fn ms(seconds: i64) -> i64 {
    seconds.saturating_mul(1000)
}

/// id, fingerprint, name, notes, colour, created, updated.
type LegacyLayer = (String, Vec<u8>, String, Option<String>, String, i64, i64);
/// id, layer, created, updated, vertices.
type LegacyPolygon = (String, String, i64, i64, Vec<[f64; 2]>);

pub(super) fn migrate_v1(tx: &rusqlite::Transaction<'_>, report: &mut OpenReport) -> DbResult<()> {
    for table in LEGACY_TABLES {
        if exists(tx, table)? {
            tx.execute_batch(&format!("ALTER TABLE {table} RENAME TO legacy_v1_{table};"))?;
        }
    }
    tx.execute_batch(SCHEMA_V2)?;

    // Slides: one per legacy fingerprint.
    let mut slide_ids: HashMap<Vec<u8>, i64> = HashMap::new();
    if exists(tx, "legacy_v1_annotation_layers")? {
        let mut stmt = tx.prepare(
            "SELECT fingerprint, MIN(created_at), MAX(updated_at)
             FROM legacy_v1_annotation_layers GROUP BY fingerprint",
        )?;
        let rows: Vec<(Vec<u8>, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for (fingerprint, created, updated) in rows {
            let (fp, fallback) = if fingerprint.len() == 32 {
                (Some(fingerprint.clone()), None)
            } else {
                let hex: String = fingerprint.iter().map(|b| format!("{b:02x}")).collect();
                (None, Some(format!("legacy:{hex}")))
            };
            tx.execute(
                "INSERT INTO slides (legacy_fingerprint, fallback_key, created_at, last_opened_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![fp, fallback, ms(created), ms(updated)],
            )?;
            slide_ids.insert(fingerprint, tx.last_insert_rowid());
        }

        // Layers.
        let mut stmt = tx.prepare(
            "SELECT id, fingerprint, name, notes, color, created_at, updated_at
             FROM legacy_v1_annotation_layers ORDER BY lower(name), created_at",
        )?;
        let layers: Vec<LegacyLayer> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        let mut repaired_colors = 0usize;
        for (index, (id, fingerprint, name, notes, color, created, updated)) in
            layers.iter().enumerate()
        {
            let Some(slide_id) = slide_ids.get(fingerprint) else {
                continue;
            };
            let name = clean_name(name, "layer name", false).unwrap_or_else(|_| {
                let trimmed: String = name.chars().take(200).collect();
                if trimmed.trim().is_empty() {
                    "Untitled".to_string()
                } else {
                    trimmed.trim().to_string()
                }
            });
            // ANN-4: colours such as "##aébcd" passed the old CHECK.
            let color = Color::parse(color).unwrap_or_else(|| {
                repaired_colors += 1;
                Color::parse(PALETTE[index % PALETTE.len()]).unwrap_or_default()
            });
            tx.execute(
                "INSERT INTO layers (id, slide_id, name, notes, color, sort_order, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    slide_id,
                    name,
                    notes.clone().unwrap_or_default(),
                    color.to_hex(),
                    index as i64,
                    ms(*created),
                    ms(*updated)
                ],
            )?;
        }
        if repaired_colors > 0 {
            report.notes.push(format!(
                "{repaired_colors} layer colour(s) were unreadable and were replaced."
            ));
        }
    }

    // Metadata rows per annotation.
    let mut metadata: HashMap<String, Vec<(String, String)>> = HashMap::new();
    if exists(tx, "legacy_v1_annotation_metadata")? {
        let mut stmt = tx.prepare(
            "SELECT annotation_id, key, value FROM legacy_v1_annotation_metadata
             ORDER BY annotation_id, row_index",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (id, key, value) in rows {
            metadata.entry(id).or_default().push((key, value));
        }
    }

    let mut migrated = 0usize;
    let mut skipped = 0usize;
    let insert = |tx: &rusqlite::Transaction<'_>,
                  id: &str,
                  layer_id: &str,
                  geometry: &Geometry,
                  checked: bool,
                  created: i64,
                  updated: i64,
                  properties: &[(String, String)]|
     -> DbResult<()> {
        let Bounds {
            min_x,
            min_y,
            max_x,
            max_y,
        } = geometry.bounds();
        let json = serde_json::to_string(geometry).unwrap_or_default();
        let props = serde_json::to_string(properties).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT OR IGNORE INTO annotations (id, layer_id, kind, geometry, min_x, min_y, max_x, max_y,
                                                geometry_ok, properties, created_at, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
             WHERE EXISTS (SELECT 1 FROM layers WHERE id = ?2)",
            params![
                id,
                layer_id,
                geometry.kind().as_str(),
                json,
                min_x,
                min_y,
                max_x,
                max_y,
                checked,
                props,
                ms(created),
                ms(updated)
            ],
        )?;
        Ok(())
    };

    if exists(tx, "legacy_v1_annotation_points")? {
        let mut stmt = tx.prepare(
            "SELECT a.id, a.annotation_layer_id, a.created_at, a.updated_at, p.x_level0, p.y_level0
             FROM legacy_v1_annotations a JOIN legacy_v1_annotation_points p ON p.annotation_id = a.id
             WHERE a.type = 'point'",
        )?;
        let rows: Vec<(String, String, i64, i64, f64, f64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for (id, layer, created, updated, x, y) in rows {
            if !(x.is_finite() && y.is_finite()) {
                skipped += 1;
                continue;
            }
            let props = metadata.get(&id).cloned().unwrap_or_default();
            insert(
                tx,
                &id,
                &layer,
                &Geometry::Point { x, y },
                true,
                created,
                updated,
                &props,
            )?;
            migrated += 1;
        }
    }

    if exists(tx, "legacy_v1_annotation_polygon_vertices")? {
        let mut stmt = tx.prepare(
            "SELECT a.id, a.annotation_layer_id, a.created_at, a.updated_at, v.x_level0, v.y_level0
             FROM legacy_v1_annotations a
             JOIN legacy_v1_annotation_polygon_vertices v ON v.annotation_id = a.id
             WHERE a.type = 'polygon'
             ORDER BY a.id, v.vertex_index",
        )?;
        let rows: Vec<(String, String, i64, i64, f64, f64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        let mut current: Option<LegacyPolygon> = None;
        let mut flush =
            |polygon: Option<LegacyPolygon>, tx: &rusqlite::Transaction<'_>| -> DbResult<()> {
                let Some((id, layer, created, updated, vertices)) = polygon else {
                    return Ok(());
                };
                let finite = vertices
                    .iter()
                    .all(|p| p[0].is_finite() && p[1].is_finite());
                if vertices.len() < 3 || !finite {
                    skipped += 1;
                    return Ok(());
                }
                let props = metadata.get(&id).cloned().unwrap_or_default();
                let geometry = Geometry::Polygon {
                    exterior: vertices,
                    holes: Vec::new(),
                };
                // Checked for self-intersections the first time it is loaded.
                insert(tx, &id, &layer, &geometry, false, created, updated, &props)?;
                migrated += 1;
                Ok(())
            };
        for (id, layer, created, updated, x, y) in rows {
            match &mut current {
                Some((current_id, _, _, _, vertices)) if *current_id == id => vertices.push([x, y]),
                _ => {
                    flush(current.take(), tx)?;
                    current = Some((id, layer, created, updated, vec![[x, y]]));
                }
            }
        }
        flush(current.take(), tx)?;
    }

    if exists(tx, "legacy_v1_export_metadata_settings")? {
        let row: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT author, organization, project_name, license
                 FROM legacy_v1_export_metadata_settings WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        if let Some((author, organization, project, license)) = row {
            for (key, value) in [
                ("export.author", author),
                ("export.organization", organization),
                ("export.project", project),
                ("export.license", license),
            ] {
                if !value.is_empty() {
                    tx.execute(
                        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                        params![key, value],
                    )?;
                }
            }
        }
    }

    report.notes.push(format!(
        "Upgraded the annotation database from the pre-1.0 format: {} slide(s), {migrated} annotation(s){}.",
        slide_ids.len(),
        if skipped > 0 {
            format!(", {skipped} unreadable annotation(s) skipped")
        } else {
            String::new()
        }
    ));
    Ok(())
}
