//! Changes: the unit of persistence, memory updates and undo.
//!
//! Every edit is a list of [`Change`]s applied in one database transaction
//! (ANN-1), then to memory, then to the host overlays. Undo applies the
//! inverses in reverse order. Each change carries the state it expects to
//! find (`before`), so an undo or redo that no longer fits — because
//! another eov window changed the same data — fails cleanly instead of
//! overwriting it.

use crate::model::{Annotation, Class, Layer, SlideId};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Change {
    /// Insert (`before: None`), update, or delete (`after: None`) one
    /// annotation.
    Annotation {
        before: Option<Arc<Annotation>>,
        after: Option<Arc<Annotation>>,
    },
    /// Insert, update or delete the metadata of a layer (deleting is only
    /// used for empty layers; see [`Change::RemoveLayer`]).
    Layer {
        before: Option<Layer>,
        after: Option<Layer>,
    },
    /// Soft-deletes a layer with everything in it.
    RemoveLayer {
        layer: Layer,
        contents: Vec<Arc<Annotation>>,
    },
    /// Restores (or creates) a layer with its contents.
    RestoreLayer {
        layer: Layer,
        contents: Vec<Arc<Annotation>>,
    },
    /// Insert, update or delete (soft) a class.
    Class {
        before: Option<Class>,
        after: Option<Class>,
    },
    /// Moves layers from one slide record to another (relinking orphaned
    /// annotations, ANN-8).
    MoveLayers {
        from: SlideId,
        to: SlideId,
        layer_ids: Vec<String>,
    },
}

impl Change {
    pub fn inverse(&self) -> Change {
        match self {
            Change::Annotation { before, after } => Change::Annotation {
                before: after.clone(),
                after: before.clone(),
            },
            Change::Layer { before, after } => Change::Layer {
                before: after.clone(),
                after: before.clone(),
            },
            Change::RemoveLayer { layer, contents } => Change::RestoreLayer {
                layer: layer.clone(),
                contents: contents.clone(),
            },
            Change::RestoreLayer { layer, contents } => Change::RemoveLayer {
                layer: layer.clone(),
                contents: contents.clone(),
            },
            Change::Class { before, after } => Change::Class {
                before: after.clone(),
                after: before.clone(),
            },
            Change::MoveLayers {
                from,
                to,
                layer_ids,
            } => Change::MoveLayers {
                from: *to,
                to: *from,
                layer_ids: layer_ids.clone(),
            },
        }
    }

    /// The slide a change belongs to, when it is slide-specific.
    pub fn slide(&self) -> Option<SlideId> {
        match self {
            Change::Layer { before, after } => {
                after.as_ref().or(before.as_ref()).map(|l| l.slide_id)
            }
            Change::RemoveLayer { layer, .. } | Change::RestoreLayer { layer, .. } => {
                Some(layer.slide_id)
            }
            Change::MoveLayers { to, .. } => Some(*to),
            _ => None,
        }
    }
}

/// Inverts a list of changes (reverse order).
pub fn invert(changes: &[Change]) -> Vec<Change> {
    changes.iter().rev().map(Change::inverse).collect()
}

/// A short count of what a list of changes does, for logs and the audit
/// trail.
pub fn summary(changes: &[Change]) -> String {
    let (mut added, mut edited, mut deleted, mut layers, mut classes) = (0, 0, 0, 0, 0);
    for change in changes {
        match change {
            Change::Annotation {
                before: None,
                after: Some(_),
            } => added += 1,
            Change::Annotation {
                before: Some(_),
                after: None,
            } => deleted += 1,
            Change::Annotation { .. } => edited += 1,
            Change::Layer { .. } | Change::RemoveLayer { .. } | Change::RestoreLayer { .. } => {
                layers += 1
            }
            Change::MoveLayers { layer_ids, .. } => layers += layer_ids.len(),
            Change::Class { .. } => classes += 1,
        }
    }
    let mut parts = Vec::new();
    for (n, what) in [
        (added, "added"),
        (edited, "edited"),
        (deleted, "deleted"),
        (layers, "layer changes"),
        (classes, "class changes"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    parts.join(", ")
}
