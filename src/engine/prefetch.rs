// Prefetch: downloads whole sample packs into the cache so they work offline.

use super::Ev;
use super::server::{cache_path, fetch_cached};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use url::Url;

// Packs from upstream website/src/repl/prebake.mjs: (manifest, base url the REPL uses).
const CDN: &str = "https://strudel.b-cdn.net";
const DEFAULT_PACKS: &[(&str, &str)] = &[
    ("piano.json", "piano/"),
    ("tidal-drum-machines.json", "tidal-drum-machines/machines/"),
    ("uzu-drumkit.json", "uzu-drumkit/"),
    ("uzu-wavetables.json", "uzu-wavetables/"),
    ("mridangam.json", "mrid/"),
];
const BIG_PACKS: &[(&str, &str)] = &[("vcsl.json", "VCSL/")]; // ~2GB
const SOUNDFONT_URL: &str = "https://felixroos.github.io/webaudiofontdata/sound";

pub fn manifest_files(body: &[u8]) -> Vec<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(body) else {
        return vec![];
    };
    let strs = |v: &serde_json::Value| -> Vec<String> {
        match v {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            serde_json::Value::Object(o) => o
                .iter()
                .filter(|(k, _)| !k.starts_with('_'))
                .filter_map(|(_, x)| x.as_str().map(String::from))
                .collect(),
            _ => vec![],
        }
    };
    map.iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .flat_map(|(_, v)| strs(v))
        .collect()
}

pub(super) fn prefetch(home: &Path, all: bool, events: &Sender<Ev>) {
    let cache = home.join("cache");
    let mut urls = Vec::new();
    let packs = DEFAULT_PACKS.iter().chain(if all { BIG_PACKS } else { &[] });
    for (manifest, base) in packs {
        let Ok(u) = Url::parse(&format!("{CDN}/{manifest}")) else {
            continue;
        };
        if let Ok(body) = fetch_cached(&cache, &u) {
            urls.extend(manifest_files(&body).into_iter().map(|f| format!("{CDN}/{base}{f}")));
        }
    }
    if all {
        // gm.json is generated from upstream packages/soundfonts/gm.mjs by update.sh
        if let Ok(serde_json::Value::Object(gm)) =
            serde_json::from_slice(&std::fs::read(home.join("gm.json")).unwrap_or_default())
        {
            for fonts in gm.values().filter_map(|v| v.as_array()) {
                urls.extend(
                    fonts
                        .iter()
                        .filter_map(|f| f.as_str())
                        .map(|f| format!("{SOUNDFONT_URL}/{f}.js")),
                );
            }
        }
    }
    let todo: Vec<Url> = urls
        .iter()
        .filter_map(|s| Url::parse(s).ok())
        .filter(|u| cache_path(&cache, u).is_some_and(|p| !p.exists()))
        .collect();
    let total = todo.len();
    if total == 0 {
        let _ = events.send(Ev::Status(format!("samples: all {} cached", urls.len())));
        return;
    }
    let done = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let (cache, done_ref, failed_ref) = (&cache, &done, &failed);
    std::thread::scope(|s| {
        for chunk in todo.chunks(total.div_ceil(8)) {
            s.spawn(move || {
                for u in chunk {
                    if fetch_cached(cache, u).is_err() {
                        failed_ref.fetch_add(1, Ordering::Relaxed);
                    }
                    let n = done_ref.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 25 == 0 || n == total {
                        let _ = events.send(Ev::Status(format!("prefetch {n}/{total}")));
                    }
                }
            });
        }
    });
    let failed = failed.into_inner();
    let _ = events.send(Ev::Status(format!(
        "prefetch done: {total} files{}",
        if failed > 0 {
            format!(", {failed} failed (offline?)")
        } else {
            String::new()
        }
    )));
}
