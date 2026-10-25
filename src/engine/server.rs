// Local HTTP server: serves the REPL build and proxies remote fetches through the disk cache.

use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

pub(super) fn serve(server: tiny_http::Server, home: PathBuf) {
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

pub fn static_file(dist: &Path, url_path: &str) -> Option<(Vec<u8>, &'static str)> {
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
pub(super) fn fetch_cached(dir: &Path, url: &Url) -> Result<Vec<u8>, u16> {
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
