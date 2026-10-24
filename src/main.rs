// strudel: terminal live-coding REPL for Strudel with vim editing.
//   strudel [file.strudel]
mod engine;

use edtui::syntect::highlighting::{Theme, ThemeSet};
use edtui::syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet};
use edtui::{
    EditorEventHandler, EditorMode, EditorState, EditorTheme, EditorView, Highlight, Index2, LineNumbers, Lines,
    SyntaxHighlighter, THEME_SET,
};
use engine::{Cmd, Ev};
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const THEME: &str = "one-dark";
const SYNTAX: &str = include_str!("strudel.sublime-syntax");

const NEW_FILE: &str = "// ctrl-enter / space p: play+update   ctrl-. / space s: stop   K: docs   :help\n\
$: s(\"bd*2, ~ sd, hh*8\").bank(\"RolandTR909\")\n\
$: note(\"<c2 eb2 g1 bb1>*2\").s(\"sawtooth\").lpf(600)\n";

const HELP: &str = "normal mode
  space p    play / update
  ctrl-enter play / update
  space s    stop
  ctrl-.     stop
  K          docs for word
  esc        close pane
  pgup/pgdn  scroll pane

commands
  :w :q :q! :wq :x
  :play :stop :hush
  :docs <fn> function docs
  :theme     list themes
  :theme <name>
             switch theme
  :web       docs in browser
  :log       engine log
  :prefetch  default samples
  :prefetch all
             + VCSL & soundfonts
             (~2.5GB)";

// ---- docs from upstream doc.json (jsdoc) ----

struct Docs {
    entries: Vec<serde_json::Value>,
    by_name: HashMap<String, usize>,
}

impl Docs {
    fn load(path: &std::path::Path) -> Docs {
        let docs: Vec<serde_json::Value> = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("docs").and_then(|d| d.as_array()).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.get("description").is_some() && d.get("undocumented").is_none())
            .collect();
        let mut by_name = HashMap::new();
        for (i, d) in docs.iter().enumerate() {
            let names = d.get("synonyms").and_then(|s| s.as_array()).into_iter().flatten();
            for n in names.chain(d.get("name")).filter_map(|n| n.as_str()) {
                by_name.entry(n.to_string()).or_insert(i);
            }
        }
        Docs { entries: docs, by_name }
    }

    fn render(&self, name: &str) -> Option<Vec<Line<'static>>> {
        let d = &self.entries[*self.by_name.get(name)?];
        let s = |v: Option<&serde_json::Value>| v.and_then(|x| x.as_str()).map(strip_html).unwrap_or_default();
        let mut out = vec![Line::from(s(d.get("name")).bold().fg(Color::Cyan))];
        if let Some(syn) = d.get("synonyms_text").and_then(|x| x.as_str()) {
            out.push(Line::from(format!("synonyms: {syn}").dark_gray()));
        }
        out.push(Line::default());
        out.extend(s(d.get("description")).lines().map(|l| Line::from(l.to_string())));
        for p in d.get("params").and_then(|p| p.as_array()).into_iter().flatten() {
            let ty = p
                .pointer("/type/names")
                .and_then(|t| t.as_array())
                .map(|t| t.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" | "));
            out.push(Line::from(vec![
                Span::from(format!("  {} ", s(p.get("name")))).fg(Color::Yellow),
                Span::from(format!("{} ", ty.unwrap_or_default())).dark_gray(),
                Span::from(s(p.get("description"))),
            ]));
        }
        for ex in d
            .get("examples")
            .and_then(|e| e.as_array())
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str())
        {
            out.push(Line::default());
            out.extend(ex.lines().map(|l| Line::from(l.to_string()).fg(Color::Green)));
        }
        Some(out)
    }
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

// ---- app ----

enum Side {
    None,
    Log,
    Doc(Vec<Line<'static>>),
}

struct App {
    path: PathBuf,
    saved: String,
    state: EditorState,
    keys: EditorEventHandler,
    engine: UnboundedSender<Cmd>,
    events: Receiver<Ev>,
    playing: bool,
    /// code as last sent to the engine; active ranges are offsets into it
    evaluated: String,
    active: Vec<(usize, usize)>,
    home: PathBuf,
    themes: Arc<ThemeSet>,
    theme_name: String,
    theme: Theme,
    syntax: SyntaxReference,
    syntax_set: Arc<SyntaxSet>,
    engine_status: String,
    eval_error: Option<String>,
    message: String,
    cmdline: Option<String>,
    leader: bool,
    side: Side,
    side_scroll: u16,
    log: Vec<String>,
    docs: Docs,
    quit: bool,
}

impl App {
    fn text(&self) -> String {
        self.state.lines.flatten(&Some('\n')).into_iter().collect()
    }

    fn modified(&self) -> bool {
        self.text() != self.saved
    }

    fn play(&mut self) {
        self.evaluated = self.text();
        let _ = self.engine.send(Cmd::Play(self.evaluated.clone()));
        self.message = "evaluating…".into();
    }

    fn save(&mut self) -> bool {
        let text = self.text();
        match std::fs::write(&self.path, &text) {
            Ok(()) => {
                self.saved = text;
                self.message = format!("\"{}\" written", self.path.display());
                true
            }
            Err(e) => {
                self.message = format!("write failed: {e}");
                false
            }
        }
    }

    fn word_under_cursor(&self) -> String {
        let row: Vec<char> = self
            .state
            .lines
            .get(edtui::RowIndex::new(self.state.cursor.row))
            .cloned()
            .unwrap_or_default();
        let is_word = |c: &char| c.is_alphanumeric() || *c == '_' || *c == '$';
        let col = self.state.cursor.col.min(row.len().saturating_sub(1));
        if row.is_empty() || !is_word(&row[col]) {
            return String::new();
        }
        let start = row[..col].iter().rposition(|c| !is_word(c)).map_or(0, |i| i + 1);
        let end = row[col..]
            .iter()
            .position(|c| !is_word(c))
            .map_or(row.len(), |i| col + i);
        row[start..end].iter().collect()
    }

    fn show_docs(&mut self, name: &str) {
        match self.docs.render(name) {
            Some(lines) => {
                self.side = Side::Doc(lines);
                self.side_scroll = 0;
            }
            None => self.message = format!("no docs for \"{name}\""),
        }
    }

    fn run_command(&mut self, cmd: &str) {
        let (name, arg) = cmd
            .trim()
            .split_once(' ')
            .map_or((cmd.trim(), ""), |(a, b)| (a, b.trim()));
        match name {
            "w" => {
                if self.save() && self.playing {
                    self.play();
                }
            }
            "q" if self.modified() => self.message = "unsaved changes (:q! to discard, :wq to save)".into(),
            "q" | "q!" => self.quit = true,
            "wq" | "x" => self.quit = self.save(),
            "play" | "p" => self.play(),
            "stop" | "hush" | "s" => {
                let _ = self.engine.send(Cmd::Stop);
            }
            "docs" | "doc" | "h" | "help" if arg.is_empty() => {
                self.side = Side::Doc(HELP.lines().map(|l| Line::from(l.to_string())).collect());
                self.side_scroll = 0;
            }
            "docs" | "doc" | "h" | "help" => self.show_docs(arg),
            "web" => {
                let url = format!(
                    "http://127.0.0.1:{}/{}",
                    engine::PORT,
                    if arg.is_empty() {
                        "workshop/getting-started/"
                    } else {
                        arg.trim_start_matches('/')
                    }
                );
                let _ = std::process::Command::new("open").arg(&url).spawn();
                self.message = format!("opened {url}");
            }
            "log" => {
                self.side = Side::Log;
                self.side_scroll = 0;
            }
            "theme" if arg.is_empty() => {
                let list = self.themes.themes.keys().map(|n| {
                    if *n == self.theme_name {
                        Line::from(format!("● {n}")).bold()
                    } else {
                        Line::from(format!("  {n}"))
                    }
                });
                let mut lines = vec![Line::from(":theme <name> to switch"), Line::default()];
                lines.extend(list);
                lines.push(Line::default());
                lines.push(
                    Line::from(format!("add .tmTheme files to {}", self.home.join("themes").display())).dark_gray(),
                );
                self.side = Side::Doc(lines);
                self.side_scroll = 0;
            }
            "theme" => match self.themes.themes.get(arg) {
                Some(t) => {
                    self.theme = t.clone();
                    self.theme_name = arg.to_string();
                    let _ = std::fs::write(self.home.join("theme"), arg);
                    self.message = format!("theme: {arg}");
                }
                None => self.message = format!("no theme \"{arg}\" (:theme lists them)"),
            },
            "prefetch" => {
                let _ = self.engine.send(Cmd::Prefetch { all: arg == "all" });
                self.message = "prefetch started".into();
            }
            "" => {}
            _ => self.message = format!("unknown command :{name}  (:help)"),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if let Some(line) = self.cmdline.as_mut() {
            match key.code {
                KeyCode::Esc => self.cmdline = None,
                KeyCode::Enter => {
                    let cmd = self.cmdline.take().unwrap_or_default();
                    self.run_command(&cmd);
                }
                KeyCode::Backspace if line.is_empty() => self.cmdline = None,
                KeyCode::Backspace => {
                    line.pop();
                }
                KeyCode::Char(c) => line.push(c),
                _ => {}
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // global: ctrl-enter / ctrl-. (need the kitty keyboard protocol, e.g. alacritty, kitty, wezterm, ghostty)
        if ctrl && key.code == KeyCode::Enter {
            return self.play();
        }
        if ctrl && key.code == KeyCode::Char('.') {
            let _ = self.engine.send(Cmd::Stop);
            return;
        }
        if self.state.mode == EditorMode::Normal {
            if self.leader {
                self.leader = false;
                match key.code {
                    KeyCode::Char('p') => return self.play(),
                    KeyCode::Char('s') => {
                        let _ = self.engine.send(Cmd::Stop);
                        return;
                    }
                    _ => {}
                }
            }
            match key.code {
                KeyCode::Esc if !matches!(self.side, Side::None) => {
                    self.side = Side::None;
                    return;
                }
                KeyCode::Char(' ') => {
                    self.leader = true;
                    return;
                }
                KeyCode::Char(':') => {
                    self.cmdline = Some(String::new());
                    self.message.clear();
                    return;
                }
                KeyCode::Char('K') => {
                    let w = self.word_under_cursor();
                    if !w.is_empty() {
                        self.show_docs(&w);
                    }
                    return;
                }
                KeyCode::PageDown => {
                    self.side_scroll = self.side_scroll.saturating_add(10);
                    return;
                }
                KeyCode::PageUp => {
                    self.side_scroll = self.side_scroll.saturating_sub(10);
                    return;
                }
                _ => {}
            }
        }
        self.keys.on_key_event(key, &mut self.state);
    }

    fn on_engine(&mut self, ev: Ev) {
        match ev {
            Ev::Status(s) => self.engine_status = s,
            Ev::Playing(p) => {
                self.playing = p;
                if !p {
                    self.active.clear();
                }
                self.message.clear();
            }
            Ev::Active(a) => self.active = a,
            Ev::EvalError(e) => {
                if let Some(e) = &e {
                    self.log.push(format!("error: {e}"));
                }
                self.eval_error = e;
                self.message.clear();
            }
            Ev::Log(l) => {
                self.log.push(l);
                if self.log.len() > 500 {
                    self.log.drain(..100);
                }
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        let [main, status, bottom] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)]).areas(f.area());
        let side_width = if matches!(self.side, Side::None) { 0 } else { 38 };
        let [left, right] = Layout::horizontal([
            Constraint::Percentage(100 - side_width),
            Constraint::Percentage(side_width),
        ])
        .areas(main);

        // flash what's playing, like strudel.cc; offsets only match the code as evaluated
        // ponytail: highlights pause while the buffer differs from the evaluated code; map them through edits if that bugs you
        self.state.clear_highlights();
        if self.playing && !self.active.is_empty() && self.text() == self.evaluated {
            let pos = utf16_positions(&self.evaluated);
            for &(start, end) in &self.active {
                if let (Some(&s), Some(&e)) = (pos.get(start), pos.get(end.saturating_sub(1))) {
                    self.state
                        .add_highlight(Highlight::new(s, e, Style::new().add_modifier(Modifier::REVERSED)));
                }
            }
        }
        let highlighter = SyntaxHighlighter::with_sets(
            self.theme.clone(),
            self.themes.clone(),
            self.syntax.clone(),
            self.syntax_set.clone(),
        );

        let s = &self.theme.settings;
        let base = Style::new()
            .fg(rgb(s.foreground, Color::Reset))
            .bg(rgb(s.background, Color::Reset));
        f.render_widget(Block::new().style(base), f.area());
        let mut theme = EditorTheme::default().hide_status_line().base(base);
        if let Some(c) = s.gutter_foreground.or(s.foreground) {
            theme = theme.line_numbers_style(base.fg(rgb(Some(c), Color::Reset)).add_modifier(Modifier::DIM));
        }
        if let Some(c) = s.selection {
            theme = theme.selection_style(Style::new().bg(rgb(Some(c), Color::Reset)));
        }
        f.render_widget(
            EditorView::new(&mut self.state)
                .theme(theme)
                .syntax_highlighter(Some(highlighter))
                .line_numbers(LineNumbers::Absolute)
                .wrap(false),
            left,
        );

        let (title, lines): (&str, Vec<Line>) = match &self.side {
            Side::None => ("", vec![]),
            Side::Doc(l) => ("docs · esc closes", l.clone()),
            Side::Log => (
                "log · esc closes",
                self.log
                    .iter()
                    .rev()
                    .take(200)
                    .rev()
                    .map(|l| {
                        let style = if l.starts_with("error") || l.starts_with("exception") {
                            Style::new().red()
                        } else {
                            Style::new()
                        };
                        Line::styled(l.clone(), style)
                    })
                    .collect(),
            ),
        };
        let scroll = match self.side {
            // keep the log pinned to the bottom
            Side::Log => (lines.len() as u16)
                .saturating_sub(right.height.saturating_sub(2))
                .saturating_sub(self.side_scroll),
            _ => self.side_scroll,
        };
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(
                    Block::new()
                        .borders(Borders::LEFT)
                        .title(format!(" {title} "))
                        .border_style(Style::new().dark_gray()),
                ),
            right,
        );

        let mode = match self.state.mode {
            EditorMode::Normal => " NORMAL ".on_blue(),
            EditorMode::Insert => " INSERT ".on_green(),
            EditorMode::Visual => " VISUAL ".on_magenta(),
            EditorMode::Search => " SEARCH ".on_yellow(),
        };
        let play = if self.playing {
            " ▶ playing ".fg(Color::Green)
        } else {
            " ■ stopped ".dark_gray()
        };
        let name = format!(" {}{} ", self.path.display(), if self.modified() { " [+]" } else { "" });
        let pos = format!(" {}:{} ", self.state.cursor.row + 1, self.state.cursor.col + 1);
        f.render_widget(
            Line::from(vec![
                mode.black().add_modifier(Modifier::BOLD),
                Span::from(name),
                play.bold(),
                Span::from(format!("│ {} ", self.engine_status)).dark_gray(),
                Span::from(pos).dark_gray(),
            ])
            .style(Style::new().on_black()),
            status,
        );

        let bottom_line = if let Some(c) = &self.cmdline {
            Line::from(format!(":{c}"))
        } else if let Some(e) = &self.eval_error {
            Line::from(e.lines().next().unwrap_or("").to_string()).red()
        } else {
            Line::from(self.message.clone())
        };
        f.render_widget(bottom_line, bottom);
        if let Some(c) = &self.cmdline {
            f.set_cursor_position((bottom.x + 1 + c.chars().count() as u16, bottom.y));
        }
    }
}

fn strudel_syntax_set() -> Arc<SyntaxSet> {
    let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
    builder.add(SyntaxDefinition::load_from_str(SYNTAX, true, Some("strudel")).expect("strudel.sublime-syntax"));
    Arc::new(builder.build())
}

/// (row, col) for every UTF-16 code unit of `text` (JS string offsets are UTF-16).
fn utf16_positions(text: &str) -> Vec<Index2> {
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

/// Bundled edtui themes plus any .tmTheme files in `dir`.
fn load_themes(dir: &std::path::Path) -> Arc<ThemeSet> {
    let mut set = ThemeSet {
        themes: THEME_SET.themes.clone(),
    };
    if dir.is_dir() {
        let _ = set.add_from_folder(dir);
    }
    Arc::new(set)
}

/// The saved theme if it still exists, else the default.
fn initial_theme(themes: &ThemeSet, saved: Option<String>) -> String {
    saved
        .map(|s| s.trim().to_string())
        .filter(|s| themes.themes.contains_key(s))
        .unwrap_or_else(|| THEME.to_string())
}

fn rgb(c: Option<edtui::syntect::highlighting::Color>, fallback: Color) -> Color {
    c.map_or(fallback, |c| Color::Rgb(c.r, c.g, c.b))
}

fn home() -> PathBuf {
    std::env::var_os("STRUDEL_HOME").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Application Support/strudel")
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "untitled.strudel".into()));
    let home = home();
    if !home.join("chrome-headless-shell").exists() || !home.join("dist").exists() {
        eprintln!(
            "strudel isn't installed in {}: run update.sh from the strudel-tui repo",
            home.display()
        );
        std::process::exit(1);
    }
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    let text = if saved.is_empty() {
        NEW_FILE.to_string()
    } else {
        saved.clone()
    };

    let syntax_set = strudel_syntax_set();
    let themes = load_themes(&home.join("themes"));
    let theme_name = initial_theme(&themes, std::fs::read_to_string(home.join("theme")).ok());
    let (tx, rx) = std::sync::mpsc::channel();
    let (engine, engine_thread) = engine::spawn(home.clone(), tx);

    let mut app = App {
        path,
        saved,
        state: EditorState::new(Lines::from(text.as_str())),
        keys: EditorEventHandler::default(),
        engine,
        events: rx,
        playing: false,
        evaluated: String::new(),
        active: Vec::new(),
        theme: themes.themes[&theme_name].clone(),
        theme_name,
        themes,
        home: home.clone(),
        syntax: syntax_set.find_syntax_by_extension("strudel").unwrap().clone(),
        syntax_set,
        engine_status: "starting…".into(),
        eval_error: None,
        message: ":help for keys".into(),
        cmdline: None,
        leader: false,
        side: Side::Doc(HELP.lines().map(|l| Line::from(l.to_string())).collect()),
        side_scroll: 0,
        log: Vec::new(),
        docs: Docs::load(&home.join("doc.json")),
        quit: false,
    };

    let mut terminal = ratatui::init();
    let enhanced = ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let result = (|| -> std::io::Result<()> {
        let mut dirty = true;
        while !app.quit {
            // only redraw on change: the syntax highlighter is rebuilt per draw
            if dirty {
                terminal.draw(|f| app.draw(f))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(50))? {
                dirty = true;
                match event::read()? {
                    Event::Key(k) => app.on_key(k),
                    Event::Paste(s) => app.keys.on_paste_event(s, &mut app.state),
                    _ => {}
                }
            }
            while let Ok(ev) = app.events.try_recv() {
                dirty = true;
                app.on_engine(ev);
            }
        }
        Ok(())
    })();
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    // closing the channel makes the engine shut chrome down
    drop(app.engine);
    let _ = engine_thread.join();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn highlighter_builds() {
        let names: Vec<_> = edtui::THEME_SET.themes.keys().cloned().collect();
        assert!(edtui::THEME_SET.themes.contains_key(super::THEME), "themes: {names:?}");
        assert!(
            super::strudel_syntax_set()
                .find_syntax_by_extension("strudel")
                .is_some()
        );
    }

    #[test]
    fn mini_notation_gets_its_own_scopes() {
        use edtui::syntect::parsing::{ParseState, ScopeStack};
        let set = super::strudel_syntax_set();
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
        use std::io::Write;
        f.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>name</key><string>my</string><key>settings</key><array>
<dict><key>settings</key><dict><key>background</key><string>#101010</string><key>foreground</key><string>#e0e0e0</string></dict></dict>
</array></dict></plist>"#).unwrap();
        let themes = super::load_themes(&dir);
        assert!(themes.themes.contains_key("my-theme"));
        assert!(themes.themes.contains_key(super::THEME));
        assert_eq!(super::initial_theme(&themes, Some("my-theme\n".into())), "my-theme");
        assert_eq!(super::initial_theme(&themes, Some("gone".into())), super::THEME);
        assert_eq!(super::initial_theme(&themes, None), super::THEME);
    }

    #[test]
    fn utf16_positions_map_rows_and_cols() {
        let p = super::utf16_positions("ab\n🎵c");
        assert_eq!(p.len(), 6); // a b \n + 2 units for 🎵 + c
        assert_eq!((p[3].row, p[3].col), (1, 0));
        assert_eq!((p[5].row, p[5].col), (1, 1));
    }
}
