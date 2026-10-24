// Audio engine: the upstream Strudel web REPL running in a bundled chrome-headless-shell.
// A local HTTP server serves the REPL build and proxies every remote fetch through a disk
// cache, so anything played once (or prefetched) works offline.

use chromiumoxide::cdp::browser_protocol::browser::{PermissionDescriptor, PermissionSetting, SetPermissionParams};
use chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType, MouseButton};
use chromiumoxide::cdp::js_protocol::runtime::{EventConsoleApiCalled, EventExceptionThrown};
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use url::Url;

pub const PORT: u16 = 4329;

pub enum Cmd {
    Play(String),
    Stop,
    Prefetch { all: bool },
}

pub enum Ev {
    Status(String),
    Playing(bool),
    EvalError(Option<String>),
    Log(String),
    /// UTF-16 [start, end) offsets into the evaluated code of the events sounding right now
    Active(Vec<(usize, usize)>),
}

const ACTIVE_MARK: &str = "\u{1}hl";

// Wraps the REPL's per-frame highlight call (same data strudel.cc uses to flash playing
// notes) and posts the active source ranges via console whenever they change.
const ACTIVE_HOOK: &str = r#"
(() => {
  const m = window.strudelMirror;
  const orig = m.highlight.bind(m);
  let last = '';
  m.highlight = (haps, time) => {
    orig(haps, time);
    const locs = haps
      .filter((h) => h.whole && h.context?.locations)
      .flatMap((h) => h.context.locations.map((l) => [l.start, l.end]));
    const s = JSON.stringify(locs);
    if (s !== last) {
      last = s;
      console.debug('\u0001hl' + s);
    }
  };
})();
"#;

const FETCH_HOOK: &str = r#"
(() => {
  const orig = window.fetch.bind(window);
  window.fetch = (input, init) => {
    const req = input instanceof Request ? input : null;
    const url = req ? req.url : String(input);
    const method = (init?.method || req?.method || 'GET').toUpperCase();
    // supabase = shared patterns db, must stay live
    if (method === 'GET' && /^https?:\/\//.test(url) && !url.includes('supabase.co') && !url.startsWith(location.origin)) {
      return orig('/sc/' + encodeURIComponent(url));
    }
    return orig(input, init);
  };
})();
"#;

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

pub fn spawn(home: PathBuf, events: Sender<Ev>) -> (UnboundedSender<Cmd>, std::thread::JoinHandle<()>) {
    let (tx, mut rx) = unbounded_channel();
    let handle = std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("127.0.0.1", PORT)) {
            Ok(s) => s,
            Err(e) => {
                let _ = events.send(Ev::Status(format!("port {PORT} busy (another strudel running?): {e}")));
                return;
            }
        };
        let h = home.clone();
        std::thread::spawn(move || serve(server, h));
        let (h, ev) = (home.clone(), events.clone());
        std::thread::spawn(move || prefetch(&h, false, &ev));

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut browser = match launch(&home, &events).await {
                Ok(b) => b,
                Err(e) => {
                    let _ = events.send(Ev::Status(format!("engine failed: {e}")));
                    return;
                }
            };
            let page = browser.pages().await.ok().and_then(|p| p.into_iter().next());
            let Some(page) = page else { return };
            let _ = events.send(Ev::Status("engine ready".into()));
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Cmd::Play(code) => {
                        let js = format!(
                            "(async () => {{ strudelMirror.setCode({}); await strudelMirror.evaluate(); \
                             const e = strudelMirror.repl.state.evalError; return e ? String(e.message ?? e) : ''; }})()",
                            serde_json::to_string(&code).unwrap()
                        );
                        match page.evaluate(js).await.map(|r| r.into_value::<String>()) {
                            Ok(Ok(e)) if e.is_empty() => {
                                let _ = events.send(Ev::EvalError(None));
                                let _ = events.send(Ev::Playing(true));
                            }
                            Ok(Ok(e)) => {
                                let _ = events.send(Ev::EvalError(Some(e)));
                            }
                            Ok(Err(e)) => {
                                let _ = events.send(Ev::EvalError(Some(e.to_string())));
                            }
                            Err(e) => {
                                let _ = events.send(Ev::EvalError(Some(e.to_string())));
                            }
                        }
                    }
                    Cmd::Stop => {
                        let _ = page.evaluate("strudelMirror.stop()").await;
                        let _ = events.send(Ev::Playing(false));
                    }
                    Cmd::Prefetch { all } => {
                        let (h, ev) = (home.clone(), events.clone());
                        std::thread::spawn(move || prefetch(&h, all, &ev));
                    }
                }
            }
            let _ = browser.close().await;
            let _ = browser.wait().await;
        });
    });
    (tx, handle)
}

async fn launch(home: &Path, events: &Sender<Ev>) -> Result<Browser, Box<dyn std::error::Error>> {
    let origin = format!("http://127.0.0.1:{PORT}");
    let _ = events.send(Ev::Status("starting engine…".into()));
    // with_head + explicit --headless: chromiumoxide's headless modes force --mute-audio
    let config = BrowserConfig::builder()
        .chrome_executable(home.join("chrome-headless-shell"))
        .user_data_dir(home.join("profile"))
        .with_head()
        .arg("headless")
        .arg(("autoplay-policy", "no-user-gesture-required"))
        .build()?;
    let (browser, mut handler) = Browser::launch(config).await?;
    tokio::spawn(async move { while handler.next().await.is_some() {} });

    for sysex in [false, true] {
        browser
            .execute(
                SetPermissionParams::builder()
                    .permission(PermissionDescriptor::builder().name("midi").sysex(sysex).build()?)
                    .setting(PermissionSetting::Granted)
                    .origin(origin.clone())
                    .build()?,
            )
            .await?;
    }
    let page = browser.new_page("about:blank").await?;

    let mut console = page.event_listener::<EventConsoleApiCalled>().await?;
    let ev = events.clone();
    tokio::spawn(async move {
        while let Some(e) = console.next().await {
            let text: Vec<String> = e
                .args
                .iter()
                .filter_map(|a| {
                    a.value
                        .as_ref()
                        .map(|v| v.as_str().map(String::from).unwrap_or(v.to_string()))
                })
                .filter(|s| !s.starts_with("%c") && !s.starts_with("background-color") && !s.starts_with("color:"))
                .collect();
            if let Some(json) = text.first().and_then(|t| t.strip_prefix(ACTIVE_MARK)) {
                if let Ok(locs) = serde_json::from_str(json) {
                    let _ = ev.send(Ev::Active(locs));
                }
            } else if !text.is_empty() {
                let _ = ev.send(Ev::Log(text.join(" ")));
            }
        }
    });
    let mut exceptions = page.event_listener::<EventExceptionThrown>().await?;
    let ev = events.clone();
    tokio::spawn(async move {
        while let Some(e) = exceptions.next().await {
            let d = &e.exception_details;
            let msg = d
                .exception
                .as_ref()
                .and_then(|x| x.description.clone())
                .unwrap_or(d.text.clone());
            let _ = ev.send(Ev::Log(format!("exception: {msg}")));
        }
    });

    page.add_init_script(FETCH_HOOK).await?;
    page.goto(origin.as_str()).await?;
    for _ in 0..150 {
        if page
            .evaluate("!!window.strudelMirror")
            .await?
            .into_value::<bool>()
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    page.evaluate(ACTIVE_HOOK).await?;
    // the REPL starts its AudioContext on the first mousedown
    for kind in [
        DispatchMouseEventType::MousePressed,
        DispatchMouseEventType::MouseReleased,
    ] {
        page.execute(
            DispatchMouseEventParams::builder()
                .r#type(kind)
                .x(10.0)
                .y(300.0)
                .button(MouseButton::Left)
                .click_count(1)
                .build()?,
        )
        .await?;
    }
    Ok(browser)
}

// ---- local server: REPL build + offline cache ----

fn serve(server: tiny_http::Server, home: PathBuf) {
    for rq in server.incoming_requests() {
        let home = home.clone();
        std::thread::spawn(move || {
            let path = rq.url().to_string();
            let (status, body, ctype) = if let Some(enc) = path.strip_prefix("/sc/") {
                let target = percent_encoding::percent_decode_str(enc)
                    .decode_utf8_lossy()
                    .into_owned();
                match Url::parse(&target)
                    .map_err(|_| 400)
                    .and_then(|u| fetch_cached(&home.join("cache"), &u).map(|b| (b, mime(u.path()))))
                {
                    Ok((b, t)) => (200, b, t),
                    Err(code) => (code, Vec::new(), "text/plain"),
                }
            } else {
                match static_file(&home.join("dist"), &path) {
                    Some((b, t)) => (200, b, t),
                    None => (404, Vec::new(), "text/plain"),
                }
            };
            let header = tiny_http::Header::from_bytes("Content-Type", ctype).unwrap();
            let _ = rq.respond(
                tiny_http::Response::from_data(body)
                    .with_status_code(status)
                    .with_header(header),
            );
        });
    }
}

fn static_file(dist: &Path, url_path: &str) -> Option<(Vec<u8>, &'static str)> {
    let path = url_path.split(['?', '#']).next()?;
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let mut p = dist.to_path_buf();
    for seg in decoded.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        p.push(seg);
    }
    if p.is_dir() {
        p.push("index.html");
    }
    let body = std::fs::read(&p).ok()?;
    Some((body, mime(&p.to_string_lossy())))
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// Maps a remote URL to `<cache>/<host>/<path>`, rejecting anything that could escape the cache dir.
pub fn cache_path(dir: &Path, url: &Url) -> Option<PathBuf> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let mut p = dir.join(url.host_str()?);
    let segs: Vec<&str> = url.path_segments()?.collect();
    for s in &segs {
        if s.is_empty() || *s == "." || *s == ".." || s.contains('\\') {
            continue;
        }
        p.push(s);
    }
    if segs.last().is_none_or(|s| s.is_empty()) {
        p.push("index");
    }
    if let Some(q) = url.query() {
        let q: String = q
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let name = format!("{}__{}", p.file_name()?.to_string_lossy(), q);
        p.set_file_name(name);
    }
    Some(p)
}

fn download(url: &str, path: &Path) -> Result<Vec<u8>, u16> {
    let res = ureq::get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => code,
        ureq::Error::Transport(_) => 504,
    })?;
    let mut body = Vec::new();
    res.into_reader()
        .take(1 << 30)
        .read_to_end(&mut body)
        .map_err(|_| 502u16)?;
    // write-then-rename so a reader never sees a half-written file
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension(format!("part{:?}", std::thread::current().id()).replace(['(', ')'], ""));
    if std::fs::write(&tmp, &body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
    Ok(body)
}

/// Cache-first for media, network-first for manifests (.json / query urls), cache as offline fallback.
fn fetch_cached(dir: &Path, url: &Url) -> Result<Vec<u8>, u16> {
    let path = cache_path(dir, url).ok_or(400u16)?;
    let manifest = url.path().ends_with(".json") || url.query().is_some();
    if !manifest && let Ok(body) = std::fs::read(&path) {
        return Ok(body);
    }
    match download(url.as_str(), &path) {
        Ok(body) => Ok(body),
        Err(code) => std::fs::read(&path).map_err(|_| code),
    }
}

// ---- prefetch ----

fn manifest_files(body: &[u8]) -> Vec<String> {
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

fn prefetch(home: &Path, all: bool, events: &Sender<Ev>) {
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
    let done = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicUsize::new(0);
    let (cache, done_ref, failed_ref) = (&cache, &done, &failed);
    std::thread::scope(|s| {
        for chunk in todo.chunks(total.div_ceil(8)) {
            s.spawn(move || {
                for u in chunk {
                    if fetch_cached(cache, u).is_err() {
                        failed_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let n = done_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_stays_inside_dir() {
        let dir = Path::new("/c");
        let p = |s: &str| cache_path(dir, &Url::parse(s).unwrap());
        assert_eq!(p("https://a.com/x/y.wav"), Some(PathBuf::from("/c/a.com/x/y.wav")));
        assert_eq!(
            p("https://a.com/x/../../../etc/passwd"),
            Some(PathBuf::from("/c/a.com/etc/passwd"))
        );
        assert_eq!(p("https://a.com/%2E%2E/%2E%2E/z"), Some(PathBuf::from("/c/a.com/z")));
        assert_eq!(p("https://a.com/"), Some(PathBuf::from("/c/a.com/index")));
        assert_eq!(
            p("https://a.com/s.json?x=1&y=2"),
            Some(PathBuf::from("/c/a.com/s.json__x_1_y_2"))
        );
        assert_eq!(p("file:///etc/passwd"), None);
    }

    #[test]
    fn static_file_stays_inside_dist() {
        let dist = std::env::temp_dir().join("strudel-test-dist");
        std::fs::create_dir_all(dist.join("learn")).unwrap();
        std::fs::write(dist.join("learn/index.html"), "ok").unwrap();
        assert_eq!(static_file(&dist, "/learn/?x=1").map(|(b, _)| b), Some(b"ok".to_vec()));
        assert!(static_file(&dist, "/../../../../etc/passwd").is_none());
        assert!(static_file(&dist, "/%2e%2e/%2e%2e/etc/passwd").is_none());
    }

    #[test]
    fn manifest_files_reads_all_shapes() {
        let m = br#"{"_base":"x","a":"a.wav","b":["b1.wav","b2.wav"],"c":{"c4":"c.wav","_base":"y"}}"#;
        let mut f = manifest_files(m);
        f.sort();
        assert_eq!(f, ["a.wav", "b1.wav", "b2.wav", "c.wav"]);
    }
}
