// strudel [file.strudel]

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "untitled.strudel".into()));
    let home = strudel::home();
    if !home.join("chrome-headless-shell").exists() || !home.join("dist").exists() {
        eprintln!(
            "strudel isn't installed in {}: run update.sh from the strudel-tui repo",
            home.display()
        );
        std::process::exit(1);
    }
    strudel::app::run(path, home)
}
