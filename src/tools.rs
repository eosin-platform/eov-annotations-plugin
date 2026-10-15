//! The plugin's tools, toolbar buttons and their icons.
//!
//! Hotkeys avoid the host's shortcuts (f, m, n, r, d, + = -, digits,
//! arrows, Esc, Enter, Delete and the Ctrl/⌘ combinations).

use crate::model::Class;
use eov_plugin_api::prelude::*;

pub const SELECT: &str = "select";
pub const POINT: &str = "point";
pub const COUNT: &str = "count";
pub const POLYGON: &str = "polygon";
pub const FREEHAND: &str = "freehand";
pub const RECTANGLE: &str = "rectangle";
pub const ELLIPSE: &str = "ellipse";
pub const POLYLINE: &str = "polyline";
pub const TEXT: &str = "text";
pub const FIELD: &str = "field";

pub const SIDEBAR_BUTTON: &str = "sidebar";
pub const VISIBILITY_HUD: &str = "visibility";
pub const CLASS_BUTTON_PREFIX: &str = "class-";

/// Edit layers that do not exist: with these, clicks never grab existing
/// objects, so every count click adds a point.
pub const NO_EDITING: &str = "__no_editing__";

fn svg(body: &str) -> String {
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='white' \
         stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'>{body}</svg>"
    )
}

pub fn sidebar_icon() -> String {
    svg(
        "<path d='M12 21s-6-5.3-6-10a6 6 0 0 1 12 0c0 4.7-6 10-6 10z'/><circle cx='12' cy='11' r='2.2'/>",
    )
}

pub fn visible_icon() -> String {
    svg(
        "<path d='M2 12s3.6-6 10-6 10 6 10 6-3.6 6-10 6S2 12 2 12z'/><circle cx='12' cy='12' r='2.8'/>",
    )
}

pub fn hidden_icon() -> String {
    svg("<path d='M2 12s3.6-6 10-6 10 6 10 6-3.6 6-10 6S2 12 2 12z'/><path d='M4 20 20 4'/>")
}

struct Def {
    id: &'static str,
    label: &'static str,
    key: &'static str,
    kind: ToolKind,
    icon: &'static str,
}

const TOOLS: &[Def] = &[
    Def {
        id: SELECT,
        label: "Select and edit annotations",
        key: "v",
        kind: ToolKind::Select,
        icon: "<path d='M5 3l12 8-5 1 3 6-2 1-3-6-4 4z'/>",
    },
    Def {
        id: POINT,
        label: "Point annotation",
        key: ".",
        kind: ToolKind::Point,
        icon: "<circle cx='12' cy='12' r='3.5' fill='white'/>",
    },
    Def {
        id: COUNT,
        label: "Count cells (Shift+click: second class)",
        key: "c",
        kind: ToolKind::Point,
        icon: "<circle cx='7' cy='8' r='2' fill='white'/><circle cx='16' cy='7' r='2' fill='white'/><circle cx='11' cy='16' r='2' fill='white'/><path d='M17 14v6M14 17h6'/>",
    },
    Def {
        id: POLYGON,
        label: "Polygon (Shift: add to selection, Alt: cut from it)",
        key: "p",
        kind: ToolKind::Polygon,
        icon: "<path d='M5 7l7-4 7 5-2 10H7z'/>",
    },
    Def {
        id: FREEHAND,
        label: "Freehand outline (Shift: add, Alt: cut)",
        key: "g",
        kind: ToolKind::Freehand,
        icon: "<path d='M4 15c2-6 6-9 9-7s-3 6 0 8 6-2 7-5'/>",
    },
    Def {
        id: RECTANGLE,
        label: "Rectangle",
        key: "b",
        kind: ToolKind::Rectangle,
        icon: "<rect x='4' y='6' width='16' height='12' rx='1'/>",
    },
    Def {
        id: ELLIPSE,
        label: "Ellipse",
        key: "o",
        kind: ToolKind::Ellipse,
        icon: "<ellipse cx='12' cy='12' rx='8' ry='6'/>",
    },
    Def {
        id: POLYLINE,
        label: "Line / path",
        key: "l",
        kind: ToolKind::Polyline,
        icon: "<path d='M4 18l5-8 5 4 6-9'/>",
    },
    Def {
        id: TEXT,
        label: "Text label",
        key: "t",
        kind: ToolKind::Point,
        icon: "<path d='M6 6h12M12 6v13'/>",
    },
    Def {
        id: FIELD,
        label: "HPF field (click to place a field of the configured size)",
        key: "h",
        kind: ToolKind::Point,
        icon: "<circle cx='12' cy='12' r='8' stroke-dasharray='3 2'/><path d='M12 9v6M9 12h6'/>",
    },
];

/// Hotkeys of the tools (classes may not use them).
pub fn tool_hotkeys() -> Vec<&'static str> {
    TOOLS.iter().map(|t| t.key).collect()
}

pub fn is_tool(id: &str) -> bool {
    TOOLS.iter().any(|t| t.id == id)
}

pub fn tool_specs() -> Vec<ToolSpec> {
    TOOLS
        .iter()
        .enumerate()
        .map(|(order, def)| {
            let mut spec = ToolSpec::new(def.id, def.label, svg(def.icon), def.kind)
                .with_hotkey(Hotkey::key(def.key));
            spec.order = order as i32;
            spec.cursor = match def.kind {
                ToolKind::Select => Cursor::Default,
                _ => Cursor::Crosshair,
            };
            if def.id == COUNT {
                spec.edit_layers = vec![NO_EDITING.to_string()];
            }
            spec.preview_style = Some(Style {
                fill: Rgba(0xFF, 0xD6, 0x0A, 0x22),
                stroke: Rgba(0xFF, 0xD6, 0x0A, 0xFF),
                stroke_width: 1.5,
                dash: Dash::Dashed,
                ..Style::default()
            });
            // Hand-drawn outlines keep detail down to about a screen pixel.
            spec.freehand_tolerance_px = 1.0;
            spec
        })
        .collect()
}

/// A coloured dot with the hotkey letter, for class buttons.
pub fn class_icon(class: &Class) -> String {
    let key = class
        .hotkey
        .as_deref()
        .unwrap_or("")
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let key = match key.as_str() {
        "<" => "&lt;".to_string(),
        ">" => "&gt;".to_string(),
        "&" => "&amp;".to_string(),
        "'" => "&apos;".to_string(),
        "\"" => "&quot;".to_string(),
        other => other.to_string(),
    };
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'><circle cx='12' cy='12' r='8' fill='{}' \
         stroke='white' stroke-width='1.2'/><text x='12' y='16' font-size='11' text-anchor='middle' \
         font-family='sans-serif' fill='white' stroke='black' stroke-width='0.4'>{key}</text></svg>",
        class.color.to_hex()
    )
}

/// The toolbar button selecting a class (only for classes with a hotkey).
pub fn class_button(class: &Class, active: bool) -> Option<ToolbarButtonSpec> {
    let key = class.hotkey.clone()?;
    Some(ToolbarButtonSpec {
        button_id: format!("{CLASS_BUTTON_PREFIX}{}", class.id),
        tooltip: format!("Class: {}", class.name),
        icon_svg: class_icon(class),
        toggled_icon_svg: None,
        active,
        enabled: true,
        hotkey: Some(Hotkey::key(key)),
        order: 100 + class.sort_order.clamp(0, 1000) as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_hotkeys_avoid_host_shortcuts() {
        let reserved = ["f", "m", "n", "r", "d", "+", "=", "-", "0", "1", "9"];
        for key in tool_hotkeys() {
            assert!(!reserved.contains(&key), "{key}");
        }
        let specs = tool_specs();
        assert_eq!(specs.len(), TOOLS.len());
        assert!(specs.len() <= 16, "the host allows 16 tools per plugin");
        let mut keys = tool_hotkeys();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), TOOLS.len(), "hotkeys are distinct");
    }

    #[test]
    fn class_icons_escape_their_letter() {
        let class = Class {
            id: "c".into(),
            name: "A".into(),
            color: crate::model::Color(1, 2, 3),
            hotkey: Some("<".into()),
            visible: true,
            opacity: 1.0,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        assert!(class_icon(&class).contains("&lt;"));
        assert!(class_button(&class, false).is_some());
        let mut none = class;
        none.hotkey = None;
        assert!(class_button(&none, false).is_none());
    }
}
