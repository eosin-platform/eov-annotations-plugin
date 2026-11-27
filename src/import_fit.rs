//! How tall the import dialog (`ui/import-dialog.slint`) is.
//!
//! eov gives a plugin's dialog the size the plugin asks for when it opens
//! it, and does not measure texts for plugins. With the choices for layers
//! whose names exist already, 400 px are just enough for the English texts;
//! a language whose texts take more lines (Japanese wraps the last sentence)
//! gets the dialog taller by as many lines, so that everything shows without
//! scrolling. The lines are estimated from the characters of the texts, the
//! same way for the English texts and their translations, so the estimate's
//! errors cancel out and the English dialog keeps its size. What the
//! estimate misses can still be scrolled to.

use eov_plugin_api::i18n;

pub const WIDTH: f32 = 520.0;
/// The height the English texts fill.
pub const HEIGHT: f32 = 400.0;
/// At most: eov's window is at least 600 px high.
pub const MAX_HEIGHT: f32 = 560.0;

/// The Slint context of the dialog's own texts (its component's name).
const CONTEXT: &str = "ImportDialog";

/// Where a text is laid out: its font size and the width of its column (from
/// the dialog's 520 px: padding, scroll bar, radio button).
#[derive(Clone, Copy)]
struct Column {
    font: f32,
    width: f32,
}

const TEXT: Column = Column {
    font: 12.0,
    width: 478.0,
};
const CHOICE: Column = Column {
    font: 12.0,
    width: 444.0,
};
const CHOICE_DETAIL: Column = Column {
    font: 11.0,
    width: 444.0,
};
const HINT: Column = Column {
    font: 11.0,
    width: 478.0,
};

/// The texts of `ui/import-dialog.slint` shown with the choices, where they
/// are laid out. (`tests` checks they are the dialog's texts.)
pub const DIALOG_TEXTS: &[(&str, ColumnKind)] = &[
    ("Add to the existing layers", ColumnKind::Choice),
    (
        "Imported annotations join the layer with the same name.",
        ColumnKind::ChoiceDetail,
    ),
    ("Replace the existing layers", ColumnKind::Choice),
    (
        "The existing layers are deleted (this can be undone).",
        ColumnKind::ChoiceDetail,
    ),
    ("Keep both", ColumnKind::Choice),
    (
        "Imported layers get a new name, such as \"Tumour (2)\".",
        ColumnKind::ChoiceDetail,
    ),
    (
        "The import is one step: Ctrl+Z (⌘Z) removes everything it added.",
        ColumnKind::Hint,
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Choice,
    ChoiceDetail,
    Hint,
}

impl ColumnKind {
    fn column(self) -> Column {
        match self {
            ColumnKind::Text => TEXT,
            ColumnKind::Choice => CHOICE,
            ColumnKind::ChoiceDetail => CHOICE_DETAIL,
            ColumnKind::Hint => HINT,
        }
    }
}

/// The dialog's height. `texts` are the texts made by the plugin that the
/// dialog shows, as (shown, English, where); the dialog's own texts are
/// added when it offers the choices (`conflicts`).
pub fn height(conflicts: bool, texts: &[(&str, &str, ColumnKind)]) -> f32 {
    if !conflicts {
        return HEIGHT;
    }
    let mut extra = 0.0;
    for (msgid, kind) in DIALOG_TEXTS {
        let shown = i18n::translate(Some(CONTEXT), msgid);
        extra += extra_height(&shown, msgid, *kind);
    }
    for (shown, english, kind) in texts {
        extra += extra_height(shown, english, *kind);
    }
    (HEIGHT + extra.max(0.0)).min(MAX_HEIGHT)
}

/// How much higher `shown` is than `english` in the same column.
fn extra_height(shown: &str, english: &str, kind: ColumnKind) -> f32 {
    let column = kind.column();
    let lines = |text: &str| lines(text, column) as f32;
    (lines(shown) - lines(english)) * line_height(column.font)
}

fn line_height(font: f32) -> f32 {
    (font * 1.4).ceil()
}

/// The lines `text` takes in `column`. The estimated widths are within 5 %
/// of the real ones (checked against Slint's layout of the dialog in every
/// language); a text that needs more than one line leaves the ends of its
/// lines empty, so only nine tenths of the width are counted on then.
fn lines(text: &str, column: Column) -> u32 {
    text.split('\n')
        .map(|paragraph| {
            let width = 1.05 * paragraph.chars().map(advance).sum::<f32>() * column.font;
            if width <= column.width {
                1
            } else {
                (width / (column.width * 0.9)).ceil() as u32
            }
        })
        .sum()
}

/// A character's width in ems, roughly as eov's interface font draws it.
fn advance(c: char) -> f32 {
    match c {
        // Isolates, joiners and combining marks take no room of their own.
        '\u{200B}'..='\u{200F}' | '\u{2066}'..='\u{2069}' | '\u{0300}'..='\u{036F}' => 0.0,
        '\u{0900}'..='\u{0903}'
        | '\u{093A}'..='\u{094F}'
        | '\u{0E31}'
        | '\u{0E34}'..='\u{0E3A}' => 0.0,
        '\u{0E47}'..='\u{0E4E}' => 0.0,
        ' ' | '\u{00A0}' | '\u{202F}' => 0.26,
        // Chinese, Japanese and Korean characters, and full-width forms.
        '\u{1100}'..='\u{115F}'
        | '\u{2E80}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE30}'..='\u{FE4F}'
        | '\u{FF00}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}' => 1.0,
        // Greek and Cyrillic letters are wider than Latin ones.
        '\u{0370}'..='\u{03FF}' | '\u{0400}'..='\u{04FF}' => 0.58,
        'i' | 'j' | 'l' | 'f' | 't' | 'r' | '.' | ',' | ':' | ';' | '\'' | '(' | ')' | '!'
        | '|' => 0.32,
        'm' | 'w' | 'M' | 'W' => 0.82,
        c if c.is_uppercase() => 0.64,
        _ => 0.54,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_keeps_the_size() {
        let summary = "GeoJSON · 2 annotations in 1 layer · 2 classes";
        let conflicts = "Layers with the same name already exist: test-annotations";
        let texts = [
            (summary, summary, ColumnKind::Text),
            (conflicts, conflicts, ColumnKind::Text),
        ];
        assert_eq!(height(true, &texts), HEIGHT);
        assert_eq!(height(false, &texts), HEIGHT);
    }

    #[test]
    fn longer_texts_make_it_taller() {
        let english = "The import is one step: Ctrl+Z (⌘Z) removes everything it added.";
        let japanese = "インポートは 1 回の操作として記録されます。Ctrl+Z（⌘Z）で、インポートで追加されたものをすべて取り消せます。";
        assert_eq!(lines(english, HINT), 1);
        assert_eq!(lines(japanese, HINT), 2);
        let texts = [(japanese, english, ColumnKind::Hint)];
        assert_eq!(height(true, &texts), HEIGHT + line_height(11.0));
        // Shorter texts do not make it smaller; it never outgrows eov's window.
        assert_eq!(
            height(true, &[(&english[..10], english, ColumnKind::Hint)]),
            HEIGHT
        );
        let long = "インポート".repeat(400);
        assert_eq!(
            height(true, &[(&long, english, ColumnKind::Hint)]),
            MAX_HEIGHT
        );
    }

    #[test]
    fn the_texts_are_the_dialogs() {
        let dialog = include_str!("../ui/import-dialog.slint");
        for (msgid, _) in DIALOG_TEXTS {
            let literal = format!("@tr(\"{}\")", msgid.replace('"', "\\\""));
            assert!(dialog.contains(&literal), "{literal} is not in the dialog");
        }
    }
}
