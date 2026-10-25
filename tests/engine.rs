use std::path::{Path, PathBuf};
use strudel::engine::prefetch::manifest_files;
use strudel::engine::server::{cache_path, static_file};
use url::Url;

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
