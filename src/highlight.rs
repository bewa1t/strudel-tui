// Syntax highlighting, color themes, and mapping the REPL's playing ranges onto the buffer.

use edtui::syntect::highlighting::ThemeSet;
use edtui::syntect::parsing::{SyntaxDefinition, SyntaxSet};
use edtui::{Index2, THEME_SET};
use ratatui::style::Color;
use std::path::Path;
use std::sync::Arc;

pub const THEME: &str = "one-dark";
const SYNTAX: &str = include_str!("strudel.sublime-syntax");

pub fn strudel_syntax_set() -> Arc<SyntaxSet> {
    let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
    builder.add(SyntaxDefinition::load_from_str(SYNTAX, true, Some("strudel")).expect("strudel.sublime-syntax"));
    Arc::new(builder.build())
}

/// Bundled edtui themes plus any .tmTheme files in `dir`.
pub fn load_themes(dir: &Path) -> Arc<ThemeSet> {
    let mut set = ThemeSet {
        themes: THEME_SET.themes.clone(),
    };
    if dir.is_dir() {
        let _ = set.add_from_folder(dir);
    }
    Arc::new(set)
}

/// The saved theme if it still exists, else the default.
pub fn initial_theme(themes: &ThemeSet, saved: Option<String>) -> String {
    saved
        .map(|s| s.trim().to_string())
        .filter(|s| themes.themes.contains_key(s))
        .unwrap_or_else(|| THEME.to_string())
}

pub fn rgb(c: Option<edtui::syntect::highlighting::Color>, fallback: Color) -> Color {
    c.map_or(fallback, |c| Color::Rgb(c.r, c.g, c.b))
}

/// (row, col) for every UTF-16 code unit of `text` (JS string offsets are UTF-16).
pub fn utf16_positions(text: &str) -> Vec<Index2> {
    let mut out = Vec::with_capacity(text.len());
    let (mut row, mut col) = (0, 0);
    for c in text.chars() {
        for _ in 0..c.len_utf16() {
            out.push(Index2::new(row, col));
        }
        if c == '\n' {
            (row, col) = (row + 1, 0);
        } else {
            col += 1;
        }
    }
    out
}
