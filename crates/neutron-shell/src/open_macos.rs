use std::path::Path;
use std::process::Command;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb { Open, Edit, Properties, RunAs }
pub fn shell_execute(path: &Path, verb: Verb, owner: isize) -> Result<(), String> {
    shell_execute_with_args(path, &[], verb, owner)
}
pub fn shell_execute_with_args(path: &Path, args: &[String], verb: Verb, _owner: isize) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/open");
    match verb {
        Verb::Open => {},
        Verb::Edit => { command.arg("-t"); },
        Verb::Properties => { command.arg("-R"); },
        Verb::RunAs => return Err("administrator launch is unavailable on macOS".into()),
    }
    command.arg("--").arg(path);
    if !args.is_empty() { command.arg("--args").args(args); }
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() { Ok(()) }
    else { Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()) }
}
