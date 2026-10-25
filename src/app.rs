// The editor: state, key handling and the main loop.
// `:` commands are in app/commands.rs, rendering in app/draw.rs.

mod commands;
mod draw;

use crate::docs::Docs;
use crate::engine::{self, Cmd, Ev};
use crate::highlight::{initial_theme, load_themes, strudel_syntax_set};
use edtui::syntect::highlighting::{Theme, ThemeSet};
use edtui::syntect::parsing::{SyntaxReference, SyntaxSet};
use edtui::{EditorEventHandler, EditorMode, EditorState, Lines};
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::text::Line;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

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

enum Side {
    None,
    Log,
    Doc(Vec<Line<'static>>),
}

fn help_pane() -> Side {
    Side::Doc(HELP.lines().map(|l| Line::from(l.to_string())).collect())
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

pub fn run(path: PathBuf, home: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
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
        syntax: syntax_set.find_syntax_by_extension("strudel").unwrap().clone(),
        syntax_set,
        engine_status: "starting…".into(),
        eval_error: None,
        message: ":help for keys".into(),
        cmdline: None,
        leader: false,
        side: help_pane(),
        side_scroll: 0,
        log: Vec::new(),
        docs: Docs::load(&home.join("doc.json")),
        home,
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

    fn stop(&self) {
        let _ = self.engine.send(Cmd::Stop);
    }

    fn open(&mut self, side: Side) {
        self.side = side;
        self.side_scroll = 0;
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
            Some(lines) => self.open(Side::Doc(lines)),
            None => self.message = format!("no docs for \"{name}\""),
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
            return self.stop();
        }
        if self.state.mode == EditorMode::Normal {
            if self.leader {
                self.leader = false;
                match key.code {
                    KeyCode::Char('p') => return self.play(),
                    KeyCode::Char('s') => return self.stop(),
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
}
