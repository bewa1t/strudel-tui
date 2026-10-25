// `:` commands.

use super::{App, Side, help_pane};
use crate::engine::{self, Cmd};
use ratatui::style::Stylize;
use ratatui::text::Line;

impl App {
    pub(super) fn run_command(&mut self, cmd: &str) {
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
            "stop" | "hush" | "s" => self.stop(),
            "docs" | "doc" | "h" | "help" if arg.is_empty() => self.open(help_pane()),
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
            "log" => self.open(Side::Log),
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
                self.open(Side::Doc(lines));
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
}
