//! Project tasks. `cargo xtask <command>`.
//!
//! # This crate died once
//!
//! It used to be a build driver: the source lived on a WSL filesystem, the
//! binary was built by the Windows toolchain, and this mapped the share to a
//! drive letter because much of MSVC refuses UNC working directories. That
//! setup is gone and so was this crate, deleted rather than left to describe a
//! machine that no longer exists.
//!
//! It is back with a different job — the one thing `cargo` genuinely cannot do,
//! which is put the built program somewhere a person can run it from.
//!
//! No dependencies. Everything here is a process spawn and a file copy, and a
//! task runner with a dependency tree is a thing to maintain rather than a
//! thing to use.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Everything the install copies.
///
/// `neutron-indexer.exe` is not optional. `index_client::helper_path` looks for
/// it *beside* the running executable and refuses to launch a UAC prompt for a
/// file that is not there — so an install of the main binary alone produces a
/// program whose search is permanently broken, and says so only when you use
/// it.
const ARTIFACTS: &[&str] = &["neutron.exe", "neutron-indexer.exe"];

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    let result = match command.as_str() {
        "install" => install(&rest),
        "uninstall" => uninstall(),
        "where" => show_paths(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

const USAGE: &str = "\
cargo xtask <command>

  install [--debug]   build and copy Neutron into %LOCALAPPDATA%\\Programs\\Neutron
  uninstall           remove that folder
  where               print the paths involved and what is currently installed";

/// `%LOCALAPPDATA%\Programs\Neutron`.
///
/// A folder of its own rather than loose files in `Programs`, because two
/// executables and their future company are an application, not a pair of
/// strays — and because uninstalling is then one directory rather than a list
/// that has to stay in step with what was copied.
fn install_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or("LOCALAPPDATA is not set; this is a Windows-only install")?;
    Ok(PathBuf::from(base).join("Programs").join("Neutron"))
}

fn target_dir(debug: bool) -> PathBuf {
    let profile = if debug { "debug" } else { "release" };
    workspace_root().join("target").join(profile)
}

/// The workspace root, from this crate's manifest rather than the current
/// directory — so the task works from anywhere inside the tree.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level under the workspace root")
        .to_path_buf()
}

fn install(args: &[String]) -> Result<(), String> {
    let debug = args.iter().any(|a| a == "--debug");
    let dest = install_dir()?;
    let built = target_dir(debug);

    // Built first. Copying a stale binary because the build was forgotten is
    // the one failure this task exists to prevent, and it is invisible
    // afterwards.
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo
        .current_dir(workspace_root())
        .arg("build")
        .arg("-p")
        .arg("neutron-app")
        .arg("-p")
        .arg("neutron-indexer");
    if !debug {
        cargo.arg("--release");
    }
    let status = cargo
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err("the build failed; nothing was copied".into());
    }

    std::fs::create_dir_all(&dest)
        .map_err(|e| format!("could not create {}: {e}", dest.display()))?;

    for name in ARTIFACTS {
        let from = built.join(name);
        let to = dest.join(name);
        if !from.is_file() {
            return Err(format!("{} was not built", from.display()));
        }
        copy(&from, &to)?;
        println!("  {} -> {}", name, to.display());
    }

    println!("\ninstalled to {}", dest.display());
    println!("run it with:  {}", dest.join("neutron.exe").display());
    Ok(())
}

/// Copies one file, with a readable answer for the failure that actually
/// happens.
///
/// Windows holds an executable open while it runs, so overwriting a copy of
/// Neutron you are currently using fails with a bare "Access is denied" — which
/// says nothing about the running window that is causing it.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    // Matched on the raw code rather than on `ErrorKind`. A running executable
    // gives ERROR_SHARING_VIOLATION, which Rust does not map to
    // `PermissionDenied`, so the first version of this checked for that kind
    // and the helpful message never appeared.
    const ACCESS_DENIED: i32 = 5;
    const SHARING_VIOLATION: i32 = 32;

    match std::fs::copy(from, to) {
        Ok(_) => Ok(()),
        Err(e)
            if matches!(e.raw_os_error(), Some(ACCESS_DENIED | SHARING_VIOLATION)) =>
        {
            Err(format!(
                "{} is in use. Close the running copy and try again.",
                to.display()
            ))
        }
        Err(e) => Err(format!("could not copy to {}: {e}", to.display())),
    }
}

fn uninstall() -> Result<(), String> {
    let dest = install_dir()?;
    if !dest.exists() {
        println!("nothing installed at {}", dest.display());
        return Ok(());
    }
    // Only the directory this task created, never a path from an argument:
    // a remove-recursive that takes a path is one typo away from being a very
    // different program.
    std::fs::remove_dir_all(&dest).map_err(|e| {
        format!(
            "could not remove {}: {e}. Close it if it is running.",
            dest.display()
        )
    })?;
    println!("removed {}", dest.display());
    Ok(())
}

fn show_paths() -> Result<(), String> {
    let dest = install_dir()?;
    println!("workspace   {}", workspace_root().display());
    println!("release     {}", target_dir(false).display());
    println!("install to  {}", dest.display());
    println!();
    for name in ARTIFACTS {
        let at = dest.join(name);
        match std::fs::metadata(&at) {
            Ok(m) => println!("  {name}: installed, {} bytes", m.len()),
            Err(_) => println!("  {name}: not installed"),
        }
    }
    Ok(())
}
