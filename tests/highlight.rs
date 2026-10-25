use edtui::syntect::parsing::{ParseState, ScopeStack};
use std::io::Write;
use strudel::highlight::{THEME, initial_theme, load_themes, strudel_syntax_set, utf16_positions};

#[test]
fn highlighter_builds() {
    let names: Vec<_> = edtui::THEME_SET.themes.keys().cloned().collect();
    assert!(edtui::THEME_SET.themes.contains_key(THEME), "themes: {names:?}");
    assert!(strudel_syntax_set().find_syntax_by_extension("strudel").is_some());
}

#[test]
fn mini_notation_gets_its_own_scopes() {
    let set = strudel_syntax_set();
    let mut state = ParseState::new(set.find_syntax_by_extension("strudel").unwrap());
    let line = "$: s(\"bd*2 ~ [hh sd]\").fast(2)\n";
    let ops = state.parse_line(line, &set).unwrap();
    let mut stack = ScopeStack::new();
    let mut scopes_at = vec![String::new(); line.len()];
    let mut last = 0;
    for (i, op) in ops {
        let s = stack.to_string();
        scopes_at[last..i].fill(s);
        stack.apply(&op).unwrap();
        last = i;
    }
    let at = |sub: &str| scopes_at[line.find(sub).unwrap()].clone();
    assert!(at("$:").contains("keyword.control"), "{}", at("$:"));
    assert!(at("s(").contains("support.function"));
    assert!(at("bd").contains("string.quoted.double"));
    assert!(at("*2").contains("keyword.operator"));
    assert!(at("~").contains("constant.language"));
    assert!(at("[hh").contains("keyword.control"));
    assert!(at("fast").contains("entity.name.function"));
}

#[test]
fn themes_load_from_folder_and_saved_choice_falls_back() {
    let dir = std::env::temp_dir().join("strudel-test-themes");
    std::fs::create_dir_all(&dir).unwrap();
    let mut f = std::fs::File::create(dir.join("my-theme.tmTheme")).unwrap();
    f.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>name</key><string>my</string><key>settings</key><array>
<dict><key>settings</key><dict><key>background</key><string>#101010</string><key>foreground</key><string>#e0e0e0</string></dict></dict>
</array></dict></plist>"#).unwrap();
    let themes = load_themes(&dir);
    assert!(themes.themes.contains_key("my-theme"));
    assert!(themes.themes.contains_key(THEME));
    assert_eq!(initial_theme(&themes, Some("my-theme\n".into())), "my-theme");
    assert_eq!(initial_theme(&themes, Some("gone".into())), THEME);
    assert_eq!(initial_theme(&themes, None), THEME);
}

#[test]
fn utf16_positions_map_rows_and_cols() {
    let p = utf16_positions("ab\n🎵c");
    assert_eq!(p.len(), 6); // a b \n + 2 units for 🎵 + c
    assert_eq!((p[3].row, p[3].col), (1, 0));
    assert_eq!((p[5].row, p[5].col), (1, 1));
}
