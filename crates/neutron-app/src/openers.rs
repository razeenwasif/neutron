//! Which app opens which kind of file, when it should not be the system's
//! choice: `openers.json` in Neutron's Application Support folder, mapping an
//! extension to an app —
//!
//! ```json
//! { "pdf": "/Applications/Quark.app" }
//! ```
//!
//! An app that declares document types to macOS is sent the file as the
//! Finder sends one, so a running app opens it rather than a second copy
//! starting. One that declares none is started with the file as its
//! argument, a new instance each time — the only way such an app hears of it.

use std::path::{Path, PathBuf};

fn config() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Neutron/openers.json"))
}

/// The app chosen for `file`, if one is. Reads the file each time — it is a
/// few bytes, and read on a worker — so an edit takes effect at once.
pub fn app_for(file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config()?).ok()?;
    app_in(&text, file)
}

fn app_in(config: &str, file: &Path) -> Option<PathBuf> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let map: std::collections::HashMap<String, String> = serde_json::from_str(config).ok()?;
    let app = map.iter().find(|(k, _)| k.trim_start_matches('.').eq_ignore_ascii_case(&ext))?.1;
    Some(PathBuf::from(app)).filter(|p| p.exists())
}

/// Whether `app` tells macOS what documents it opens.
fn declares_documents(app: &Path) -> bool {
    std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleDocumentTypes", "raw", "-o", "/dev/null"])
        .arg(app.join("Contents/Info.plist"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Opens `file` with `app`. Blocks until the launch is handed to macOS.
pub fn open_with(app: &Path, file: &Path) -> Result<(), String> {
    let mut open = std::process::Command::new("/usr/bin/open");
    if declares_documents(app) {
        open.arg("-a").arg(app).arg(file);
    } else {
        open.arg("-n").arg("-a").arg(app).arg("--args").arg(file);
    }
    let status = open.status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{} would not start", app.display())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_maps_to_an_app_that_exists() {
        let config = r#"{ "PDF": "/Applications", ".txt": "/nowhere/Missing.app" }"#;
        assert_eq!(app_in(config, Path::new("/a/b.pdf")), Some(PathBuf::from("/Applications")));
        assert_eq!(app_in(config, Path::new("/a/b.txt")), None, "an app that is not there is not used");
        assert_eq!(app_in(config, Path::new("/a/b.md")), None);
        assert_eq!(app_in("not json", Path::new("/a/b.pdf")), None);
    }

    #[test]
    fn an_app_is_known_by_whether_it_declares_documents() {
        assert!(declares_documents(Path::new("/System/Applications/Preview.app")));
        assert!(!declares_documents(Path::new("/nowhere/Missing.app")));
    }
}
