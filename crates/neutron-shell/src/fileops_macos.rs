use std::path::{Path, PathBuf};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposal { Recycle, Permanent }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer { Copy, Move }

pub fn delete(paths: &[PathBuf], disposal: Disposal, _owner: isize) -> Result<(), String> {
    let paths = super::macos::path_strs(paths)?;
    super::macos::call(json!({"action":"delete", "paths":paths, "permanent":disposal == Disposal::Permanent})).map(|_| ())
}
pub fn transfer(paths: &[PathBuf], destination: &Path, how: Transfer, _owner: isize) -> Result<(), String> {
    let (paths, destination) = (super::macos::path_strs(paths)?, super::macos::path_str(destination)?);
    super::macos::call(json!({"action":if how == Transfer::Move {"move"} else {"copy"}, "paths":paths, "destination":destination})).map(|_| ())
}
pub fn default_transfer(source: &Path, destination: &Path) -> Transfer {
    use std::os::unix::fs::MetadataExt;
    match (source.symlink_metadata(), destination.metadata()) {
        (Ok(a), Ok(b)) if a.dev() == b.dev() => Transfer::Move,
        _ => Transfer::Copy,
    }
}
pub fn rename(path: &Path, new_name: &str, _owner: isize) -> Result<(), String> {
    let path = super::macos::path_str(path)?;
    super::macos::call(json!({"action":"rename", "paths":[path], "name":new_name})).map(|_| ())
}
pub fn create_folder(parent: &Path, _owner: isize) -> Result<String, String> {
    let parent = super::macos::path_str(parent)?;
    super::macos::call(json!({"action":"mkdir", "destination":parent}))?
        .as_str().map(str::to_owned).ok_or_else(|| "macOS returned no folder name".into())
}
