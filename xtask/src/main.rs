//! Build driver for developing in WSL and building with the Windows toolchain.
//!
//! # The problem this exists to solve
//!
//! The source lives on the WSL filesystem, which Windows reaches only as the
//! UNC path `\\wsl.localhost\<distro>\...`. Much of the MSVC toolchain refuses
//! UNC working directories outright — `cmd.exe` announces "UNC paths are not
//! supported. Defaulting to Windows directory" and silently continues in the
//! wrong place, which turns into baffling build-script failures.
//!
//! So this maps the WSL share to a drive letter and runs the Windows `cargo.exe`
//! with a normal drive-letter working directory. No tool in the chain ever sees
//! a UNC path.
//!
//! It also forces `CARGO_TARGET_DIR` onto local NTFS. Leaving the target
//! directory on the WSL side routes every intermediate object file through the
//! 9p bridge, and that single change is worth more compile time than everything
//! else here combined.
//!
//! Usage: `cargo xtask build|run|test|check|clean [-- extra cargo args]`

use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Drive letters tried when mapping the WSL share, in order. Deliberately from
/// the back of the alphabet to avoid colliding with real volumes — this machine
/// already uses A, B, C, D, F, G, I.
const CANDIDATE_DRIVES: &[char] = &['N', 'M', 'K', 'L', 'P', 'Q', 'R', 'S'];

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let (cmd, rest) = args
        .split_first()
        .map(|(c, r)| (c.as_str(), r))
        .unwrap_or(("help", &[]));

    match cmd {
        "build" => cargo(&["build"], rest),
        "run" => cargo(&["run", "--bin", "neutron"], rest),
        "test" => cargo(&["test"], rest),
        "check" => cargo(&["check"], rest),
        "clippy" => cargo(&["clippy"], rest),
        "clean" => cargo(&["clean"], rest),
        "where" => {
            let env = WinEnv::detect()?;
            println!("distro:     {}", env.distro);
            println!("unc:        {}", env.unc_root);
            println!("drive:      {}:", env.drive);
            println!("source:     {}", env.win_source_dir);
            println!("target dir: {}", env.win_target_dir);
            println!("cargo:      {}", env.cargo_exe.display());
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <build|run|test|check|clippy|clean|where> [-- cargo args]"
            );
            Ok(())
        }
    }
}

struct WinEnv {
    distro: String,
    /// `\\wsl.localhost\<distro>`
    unc_root: String,
    drive: char,
    /// Source dir as Windows sees it, e.g. `N:\home\amaterasu\Neutron`.
    win_source_dir: String,
    /// Target dir on local NTFS.
    win_target_dir: String,
    /// Linux-side path to the Windows cargo.
    cargo_exe: PathBuf,
}

impl WinEnv {
    fn detect() -> Result<Self> {
        let distro = env::var("WSL_DISTRO_NAME")
            .context("WSL_DISTRO_NAME is unset — xtask must run inside WSL")?;
        let unc_root = format!(r"\\wsl.localhost\{distro}");

        let repo = repo_root()?;
        let drive = ensure_mapped(&unc_root)?;

        // /home/amaterasu/Neutron -> N:\home\amaterasu\Neutron
        let rel = repo
            .to_str()
            .context("repo path is not valid UTF-8")?
            .trim_start_matches('/')
            .replace('/', r"\");
        let win_source_dir = format!(r"{drive}:\{rel}");

        let win_target_dir = env::var("NEUTRON_WIN_TARGET_DIR").unwrap_or_else(|_| {
            let user = env::var("NEUTRON_WIN_USER").unwrap_or_else(|_| detect_win_user());
            format!(r"C:\Users\{user}\.neutron-target")
        });

        let cargo_exe = find_windows_cargo()?;

        Ok(Self {
            distro,
            unc_root,
            drive,
            win_source_dir,
            win_target_dir,
            cargo_exe,
        })
    }
}

/// Walks up from this file to the workspace root.
fn repo_root() -> Result<PathBuf> {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR is <repo>/xtask.
    dir.pop();
    Ok(dir)
}

fn detect_win_user() -> String {
    // `cmd.exe /c echo %USERNAME%` would need a valid working directory, which
    // is the very thing we may not have yet. Reading the mounted Users
    // directory avoids the chicken-and-egg problem.
    if let Ok(entries) = std::fs::read_dir("/mnt/c/Users") {
        let skip = [
            "All Users",
            "Default",
            "Default User",
            "Public",
            "desktop.ini",
        ];
        let mut candidates: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !skip.contains(&n.as_str()))
            .collect();
        candidates.sort();
        if let Some(first) = candidates.into_iter().next() {
            return first;
        }
    }
    "Default".to_owned()
}

/// Locates the Windows `cargo.exe`, preferring an explicit override.
fn find_windows_cargo() -> Result<PathBuf> {
    if let Ok(p) = env::var("NEUTRON_WIN_CARGO") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        bail!("NEUTRON_WIN_CARGO points at {}, which does not exist", p.display());
    }

    let user = detect_win_user();
    let candidates = [
        PathBuf::from(format!("/mnt/c/Users/{user}/.cargo/bin/cargo.exe")),
        PathBuf::from("/mnt/c/Program Files/Rust/bin/cargo.exe"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    bail!(
        "no Windows cargo.exe found (looked in {}). \
         Install rustup on the Windows side, or set NEUTRON_WIN_CARGO.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Returns a drive letter mapped to `unc_root`, creating the mapping if needed.
fn ensure_mapped(unc_root: &str) -> Result<char> {
    if let Ok(d) = env::var("NEUTRON_DRIVE") {
        if let Some(c) = d.chars().next() {
            return Ok(c.to_ascii_uppercase());
        }
    }

    if let Some(existing) = find_existing_mapping(unc_root) {
        return Ok(existing);
    }

    for &drive in CANDIDATE_DRIVES {
        // `net use` is run without a working directory that could itself be a
        // UNC path, since we invoke net.exe with cwd set to /mnt/c.
        let out = Command::new("net.exe")
            .args(["use", &format!("{drive}:"), unc_root, "/persistent:yes"])
            .current_dir("/mnt/c")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("failed to run net.exe — is WSL interop enabled?")?;

        if out.status.success() {
            eprintln!("xtask: mapped {drive}: -> {unc_root}");
            return Ok(drive);
        }
    }

    bail!(
        "could not map any of {CANDIDATE_DRIVES:?} to {unc_root}. \
         Map one manually (`net use N: {unc_root} /persistent:yes`) and set NEUTRON_DRIVE=N."
    )
}

/// Parses `net use` output for a drive already pointing at `unc_root`.
fn find_existing_mapping(unc_root: &str) -> Option<char> {
    let out = Command::new("net.exe")
        .args(["use"])
        .current_dir("/mnt/c")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&out.stdout);
    let target = unc_root.to_ascii_lowercase();

    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains(&target) {
            continue;
        }
        // Lines look like: "OK           N:        \\wsl.localhost\Ubuntu ..."
        // Find the token that is a bare drive letter followed by a colon.
        for token in line.split_whitespace() {
            let bytes = token.as_bytes();
            if bytes.len() == 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_alphabetic() {
                return Some((bytes[0] as char).to_ascii_uppercase());
            }
        }
    }
    None
}

/// Runs the Windows cargo against the mapped-drive copy of the workspace.
///
/// The working directory is set to `/mnt/c` rather than the repo. That looks
/// wrong but is deliberate: `Command::current_dir` is resolved by the Linux
/// side, and WSL interop translates any path under `/home` back into
/// `\\wsl.localhost\...` — reintroducing the exact UNC working directory this
/// wrapper exists to avoid. Pointing cargo at the workspace with
/// `--manifest-path` on the mapped drive sidesteps the translation entirely.
fn cargo(subcommand: &[&str], extra: &[String]) -> Result<()> {
    let env = WinEnv::detect()?;

    let root = repo_root()?;
    if !root.join("Cargo.toml").exists() {
        bail!("workspace root not found at {}", root.display());
    }

    // Strip a leading `--` so both `cargo xtask run -- --flag` and
    // `cargo xtask run --flag` work.
    let extra: Vec<&String> = extra.iter().skip_while(|a| a.as_str() == "--").collect();

    let manifest = format!(r"{}\Cargo.toml", env.win_source_dir);

    let mut cmd = Command::new(&env.cargo_exe);
    cmd.args(subcommand)
        .arg("--manifest-path")
        .arg(&manifest)
        // Passed as a flag, not as CARGO_TARGET_DIR. Environment variables set
        // here do not survive the WSL→Windows interop boundary, and when the
        // target directory silently falls back to the WSL share the build fails
        // deep in rustc with "could not create session directory lock file"
        // (the 9p filesystem does not implement LockFileEx). A CLI flag cannot
        // be lost this way.
        .arg("--target-dir")
        .arg(&env.win_target_dir)
        .args(&extra)
        // The Windows cargo must not inherit the Linux toolchain's environment
        // or it will try to invoke a Linux rustc and fail confusingly.
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTUP_HOME")
        .env_remove("RUSTC")
        .env_remove("CARGO")
        .env_remove("CARGO_HOME")
        .env_remove("LD_LIBRARY_PATH")
        .current_dir("/mnt/c");

    eprintln!(
        "xtask: cargo {} --manifest-path {manifest}  (target {})",
        subcommand.join(" "),
        env.win_target_dir
    );

    let status = cmd.status().context("failed to launch Windows cargo.exe")?;
    if !status.success() {
        bail!("cargo {} failed", subcommand.join(" "));
    }
    Ok(())
}
