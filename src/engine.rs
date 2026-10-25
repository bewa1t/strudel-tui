// Audio engine: the upstream Strudel web REPL running in a bundled chrome-headless-shell.
// A local HTTP server serves the REPL build and proxies every remote fetch through a disk
// cache, so anything played once (or prefetched) works offline.

use chromiumoxide::cdp::browser_protocol::browser::{PermissionDescriptor, PermissionSetting, SetPermissionParams};
use chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType, MouseButton};
use chromiumoxide::cdp::js_protocol::runtime::{EventConsoleApiCalled, EventExceptionThrown};
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

pub mod prefetch;
pub mod server;

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
        std::thread::spawn(move || server::serve(server, h));
        let (h, ev) = (home.clone(), events.clone());
        std::thread::spawn(move || prefetch::prefetch(&h, false, &ev));

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
                        let err = match page.evaluate(js).await.map(|r| r.into_value::<String>()) {
                            Ok(Ok(e)) => e,
                            Ok(Err(e)) => e.to_string(),
                            Err(e) => e.to_string(),
                        };
                        if err.is_empty() {
                            let _ = events.send(Ev::EvalError(None));
                            let _ = events.send(Ev::Playing(true));
                        } else {
                            let _ = events.send(Ev::EvalError(Some(err)));
                        }
                    }
                    Cmd::Stop => {
                        let _ = page.evaluate("strudelMirror.stop()").await;
                        let _ = events.send(Ev::Playing(false));
                    }
                    Cmd::Prefetch { all } => {
                        let (h, ev) = (home.clone(), events.clone());
                        std::thread::spawn(move || prefetch::prefetch(&h, all, &ev));
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
