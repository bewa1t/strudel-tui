// Function docs from upstream doc.json (jsdoc), for `K` and `:docs`.

use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::path::Path;

pub struct Docs {
    entries: Vec<serde_json::Value>,
    by_name: HashMap<String, usize>,
}

impl Docs {
    pub fn load(path: &Path) -> Docs {
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

    pub fn render(&self, name: &str) -> Option<Vec<Line<'static>>> {
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
