// Rendering: editor, side pane, status line, command line.

use super::{App, Side};
use crate::highlight::{rgb, utf16_positions};
use edtui::{EditorMode, EditorTheme, EditorView, Highlight, LineNumbers, SyntaxHighlighter};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

impl App {
    pub(super) fn draw(&mut self, f: &mut ratatui::Frame) {
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
