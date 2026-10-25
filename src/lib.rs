// strudel: terminal live-coding REPL for Strudel with vim editing.

pub mod app;
mod docs;
pub mod engine;
pub mod highlight;

use std::path::PathBuf;

/// Data dir: REPL build, docs, headless Chrome, sample cache, profile, theme.
pub fn home() -> PathBuf {
    std::env::var_os("STRUDEL_HOME").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Application Support/strudel")
    })
}
