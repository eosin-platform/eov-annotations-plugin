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
pub const ARROW: &str = "arrow";
pub const TEXT: &str = "text";
pub const FIELD: &str = "field";
pub const BRUSH: &str = "brush";

pub const SIDEBAR_BUTTON: &str = "sidebar";
pub const VISIBILITY_HUD: &str = "visibility";

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
        id: BRUSH,
        label: "Brush (paint a region, over a selected region to extend it; Alt or the eraser erases; [ and ] change the size)",
        key: "s",
        // Custom: the plugin paints the stroke from the pointer.
        kind: ToolKind::Custom,
        icon: "<path d='M14 4l6 6-8 8H6v-6z'/><path d='M4 20c2 0 3-1 3-3'/>",
    },
    Def {
        id: ARROW,
        label: "Arrow (click the tail, double-click the tip; more clicks bend it)",
        key: "a",
        kind: ToolKind::Polyline,
        icon: "<path d='M4 20 19 5'/><path d='M10 5h9v9'/>",
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
        label: "HPF field (a field of the configured size follows the pointer; click to place it, drag to pan)",
        key: "h",
        // Custom, so that the reticle can follow the pointer.
        kind: ToolKind::Custom,
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

/// The tool a key selects (`None` for the HPF field tool's own key).
pub fn tool_for_key(key: &str) -> Option<&'static str> {
    TOOLS
        .iter()
        .find(|t| t.key == key && t.id != FIELD)
        .map(|t| t.id)
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
            if def.kind == ToolKind::Custom {
                // The pointer tools claim only the keys they use: every
                // other key keeps its usual meaning (eov's shortcuts, other
                // tools, class hotkeys; Escape leaves the tool).
                spec.keys = Some(match def.id {
                    BRUSH => vec!["[".to_string(), "]".to_string()],
                    _ => Vec::new(),
                });
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

/// Prefix of the ids of class hotkeys (`class-<class id>`).
pub const CLASS_HOTKEY_PREFIX: &str = "class-";

/// The key binding that makes a class active (for classes with a hotkey).
pub fn class_hotkey(class: &Class) -> Option<HotkeySpec> {
    let key = class.hotkey.clone()?;
    Some(HotkeySpec {
        hotkey_id: format!("{CLASS_HOTKEY_PREFIX}{}", class.id),
        hotkey: Hotkey::key(key),
        description: format!("Annotation class: {}", class.name),
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
    fn classes_with_a_hotkey_get_a_key_binding() {
        let class = Class {
            id: "c".into(),
            name: "Blasts".into(),
            color: crate::model::Color(1, 2, 3),
            hotkey: Some("q".into()),
            visible: true,
            opacity: 1.0,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        let spec = class_hotkey(&class).unwrap();
        assert_eq!(spec.hotkey_id, "class-c");
        assert_eq!(spec.hotkey.key, "q");
        assert!(spec.description.contains("Blasts"));
        let mut none = class;
        none.hotkey = None;
        assert!(class_hotkey(&none).is_none());
    }

    #[test]
    fn keys_of_the_other_tools_are_known_to_the_field_tool() {
        assert_eq!(tool_for_key("p"), Some(POLYGON));
        assert_eq!(tool_for_key("v"), Some(SELECT));
        assert_eq!(tool_for_key("h"), None);
        assert_eq!(tool_for_key("x"), None);
    }
}
