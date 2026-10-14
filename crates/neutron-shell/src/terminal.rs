//! Opening a terminal at a folder, and a second Neutron window.
//!
//! # Why not the shell verb
//!
//! Windows 11 registers an "Open in Terminal" verb, so this could be a
//! `ShellExecuteW` call. It is not, for two reasons: the verb does not exist on
//! Windows 10, and it opens whatever the user's *default* terminal is through a
//! registry indirection that fails silently when the registered handler has
//! been uninstalled — which is exactly what happens on a machine where Windows
//! Terminal was replaced.
//!
//! Launching the program directly gives an error that names what went wrong.
//!
//! # The order
//!
//! A configured command first, then Windows Terminal, then `cmd.exe` — which is
//! on every Windows installation ever made and cannot be missing.
//!
//! # Why the configured command exists
//!
//! `wt.exe -d <dir>` is the documented way to open Windows Terminal at a
//! folder, and it works — for a profile whose command line Windows Terminal
//! controls. A WSL profile installed from the Store is *not* one of those: the
//! profile launches the distribution's own launcher, which starts in the Linux
//! home directory and ignores the working directory it was given. Measured, not
//! assumed: `wt -d <dir> cmd.exe` lands in `<dir>`, and `wt -d <dir>` with a
//! Store WSL profile lands in `~`.
//!
//! Nothing Neutron can pass fixes that, because the profile decides. What does
//! fix it is naming the command yourself:
//!
//! ```json
//! { "command": "wt.exe", "args": ["-d", "{dir}", "wsl.exe", "--cd", "{dir}"] }
//! ```
//!
//! A **file** rather than an environment variable, for the reason
//! `neutron_cloud::config` gives at length: environment variables only reach a
//! process that inherits them, and a file manager is started from the taskbar,
//! the Start menu, or by Explorer opening a folder — none of which inherit
//! anything.

use std::path::{Path, PathBuf};

/// A terminal command named by the user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// The program to run.
    pub command: String,
    /// Its arguments. Every `{dir}` is replaced with the folder's path.
    #[serde(default)]
    pub args: Vec<String>,
}

/// `%APPDATA%\Neutron\terminal.json`.
pub fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support").into_os_string();
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("Neutron").join("terminal.json"))
}

/// Reads the configured terminal, if there is one.
///
/// A file that will not parse is a warning rather than an error: the built-in
/// order still opens *a* terminal, and the log says why the configured one was
/// not used.
pub fn config() -> Option<Config> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Config>(&text) {
        Ok(config) if !config.command.trim().is_empty() => Some(config),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "terminal.json names no command");
            None
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "terminal.json could not be read: {e}");
            None
        }
    }
}

/// Substitutes the folder into a configured argument list.
///
/// Every occurrence, not the first: the WSL recipe above needs `{dir}` twice,
/// once for Windows Terminal and once for `wsl --cd`.
pub fn expand(args: &[String], dir: &Path) -> Vec<String> {
    let dir = dir.to_string_lossy();
    args.iter()
        .map(|arg| arg.replace("{dir}", dir.as_ref()))
        .collect()
}

/// Opens a terminal with `dir` as its working directory.
pub fn open_at(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }

    if let Some(configured) = config() {
        let args = expand(&configured.args, dir);
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        match spawn(&configured.command, &borrowed, dir, false) {
            Ok(()) => return Ok(()),
            // Reported rather than silently fallen through: the user wrote this
            // down, so being quietly ignored is worse than an error.
            Err(e) => return Err(format!("{} (from terminal.json)", e)),
        }
    }

    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/bin/open")
            .args(["-a", "Terminal", "--"]).arg(dir).output().map_err(|e| e.to_string())?;
        if output.status.success() { Ok(()) }
        else { Err(String::from_utf8_lossy(&output.stderr).trim().into()) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        match spawn("wt.exe", &["-d"], dir, true) {
            Ok(()) => return Ok(()),
            Err(e) => tracing::debug!("Windows Terminal not available: {e}"),
        }
        spawn("cmd.exe", &[], dir, false)
            .map_err(|e| format!("could not open a terminal: {e}"))
    }
}

/// Starts `program`, passing `dir` after `args` when `dir_is_argument`, and
/// otherwise as the working directory.
fn spawn(
    program: &str,
    args: &[&str],
    dir: &Path,
    dir_is_argument: bool,
) -> Result<(), String> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    if dir_is_argument {
        command.arg(dir);
    } else {
        // Also set for the configured case: a terminal whose profile inherits
        // the parent's directory then lands in the right place without the
        // command having to say so twice.
        command.current_dir(dir);
    }
    console(&mut command);
    command.spawn().map(|_| ()).map_err(|e| format!("{program}: {e}"))
}

/// A console program launched from a windowed one needs a console of its own,
/// or it attaches to nothing and appears not to have started.
#[cfg(windows)]
fn console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    command.creation_flags(CREATE_NEW_CONSOLE);
}

#[cfg(not(windows))]
fn console(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_occurrence_of_the_folder_is_substituted() {
        // The WSL recipe needs it twice — once for Windows Terminal's own
        // working directory and once for `wsl --cd`.
        let args: Vec<String> = ["-d", "{dir}", "wsl.exe", "--cd", "{dir}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = expand(&args, Path::new("C:/notes"));
        assert_eq!(out, ["-d", "C:/notes", "wsl.exe", "--cd", "C:/notes"]);
    }

    #[test]
    fn arguments_without_the_placeholder_are_left_alone() {
        let args = vec!["--login".to_owned(), "-i".to_owned()];
        assert_eq!(expand(&args, Path::new("C:/x")), args);
    }

    #[test]
    fn the_configured_terminal_parses_from_the_documented_shape() {
        let config: Config = serde_json::from_str(
            r#"{"command":"wt.exe","args":["-d","{dir}","wsl.exe","--cd","{dir}"]}"#,
        )
        .expect("should parse");
        assert_eq!(config.command, "wt.exe");
        assert_eq!(config.args.len(), 5);
    }

    #[test]
    fn a_terminal_with_no_arguments_is_allowed() {
        // `{ "command": "alacritty.exe" }` — the working directory is set on
        // the process, so a terminal that inherits it needs no arguments.
        let config: Config =
            serde_json::from_str(r#"{"command":"alacritty.exe"}"#).expect("should parse");
        assert!(config.args.is_empty());
    }

    #[test]
    fn the_config_file_sits_beside_the_others() {
        let path = config_path().expect("a path");
        assert!(path.ends_with("Neutron/terminal.json"), "{}", path.display());
    }

    #[test]
    fn a_path_that_is_not_a_folder_is_refused_before_anything_is_launched() {
        // Launching a terminal in a file's path fails somewhere deep in the
        // shell with a message about the directory being invalid; saying so
        // here costs one `is_dir` call.
        let error = open_at(Path::new("/definitely/not/a/folder")).expect_err("should refuse");
        assert!(error.contains("not a folder"), "{error}");
    }
}
