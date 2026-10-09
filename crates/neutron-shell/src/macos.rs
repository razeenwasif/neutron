//! Calls the native bridge on a worker thread. Bundles carry it beside Neutron;
//! cargo runs use the copy produced by build.rs.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use serde_json::Value;

/// Quick queries that must not wedge a worker on a hung network mount. File
/// transfers and keychain prompts can legitimately take longer, so they have
/// no limit.
const QUICK: &[&str] = &["capacity", "clipboard-read", "clipboard-write", "clipboard-text", "reveal"];
const TIMEOUT: Duration = Duration::from_secs(10);

/// A path as a JSON string. `json!` panics on non-UTF-8 paths, and release
/// builds abort on panic, so the conversion has to be checked first.
pub(crate) fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| {
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
        format!("{name} has a name Neutron cannot pass to macOS")
    })
}
pub(crate) fn path_strs(paths: &[PathBuf]) -> Result<Vec<&str>, String> {
    paths.iter().map(|p| path_str(p)).collect()
}

pub(crate) fn call(mut request: Value) -> Result<Value, String> {
    // Swift's synthesized Decodable requires the non-optional paths field.
    if request.get("paths").is_none() { request["paths"] = serde_json::json!([]); }
    let quick = request["action"].as_str().is_some_and(|a| QUICK.contains(&a));
    let bundled = std::env::current_exe().ok()
        .and_then(|p| p.parent().map(|p| p.join("neutron-macos")));
    let helper = bundled.filter(|p| p.is_file())
        .unwrap_or_else(|| env!("NEUTRON_MACOS_HELPER").into());
    let mut child = Command::new(helper)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| format!("could not start macOS services: {e}"))?;
    let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let written = child.stdin.take().unwrap().write_all(&bytes);
    if let Err(e) = written {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("could not send macOS request: {e}"));
    }
    // Read on a separate thread so a full pipe cannot deadlock against the
    // timeout, and so a helper stuck in the kernel can be abandoned.
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = tx.send(stdout.read_to_end(&mut out).map(|_| out));
    });
    let received = if quick { rx.recv_timeout(TIMEOUT).ok() } else { rx.recv().ok() };
    let Some(read) = received else {
        let _ = child.kill();
        // A process blocked on a dead mount may not exit at once; reap it
        // elsewhere rather than waiting here.
        std::thread::spawn(move || { let _ = child.wait(); });
        return Err("macOS services did not respond; a disk or network volume may be unresponsive".into());
    };
    let stdout = read.map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    let response: Value = serde_json::from_slice(&stdout)
        .map_err(|_| "macOS services returned an invalid response".to_owned())?;
    if let Some(error) = response["error"].as_str() { return Err(error.to_owned()); }
    if !status.success() { return Err("macOS services failed".into()); }
    Ok(response["result"].clone())
}
