use super::{Command, Path, PathBuf, copy, target_dir, workspace_root};

fn run(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("command failed: {command:?}")) }
}
fn debug(args: &[String]) -> Result<bool, String> {
    if args.iter().any(|arg| arg != "--debug") { return Err("only --debug is supported".into()); }
    Ok(args.iter().any(|arg| arg == "--debug"))
}
pub fn bundle(args: &[String]) -> Result<PathBuf, String> {
    let debug = debug(args)?;
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(workspace_root()).args(["build", "--locked", "-p", "neutron-app"]);
    if !debug { cargo.arg("--release"); }
    run(&mut cargo)?;
    let built = target_dir(debug);
    let bundle = built.join("Neutron.app");
    // Stage the whole bundle so a failed build never damages the previous one.
    let stage = built.join("Neutron.staging.app");
    if stage.exists() { std::fs::remove_dir_all(&stage).map_err(|e| e.to_string())?; }
    let macos = stage.join("Contents/MacOS");
    let resources = stage.join("Contents/Resources");
    std::fs::create_dir_all(&macos).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&resources).map_err(|e| e.to_string())?;
    copy(&built.join("neutron"), &macos.join("neutron"))?;
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    run(Command::new("xcrun").current_dir(workspace_root())
        .args(["swiftc", "-O", "-target", &format!("{arch}-apple-macosx12.0"), "-module-cache-path"])
        .arg(built.join("swift-cache"))
        .arg("crates/neutron-shell/macos/bridge.swift")
        .arg("-o").arg(macos.join("neutron-macos")))?;
    icon(&resources, &built)?;
    // The interface font, and the licence it is distributed under.
    let fonts = resources.join("fonts");
    std::fs::create_dir_all(&fonts).map_err(|e| e.to_string())?;
    for name in ["Manrope-Medium.ttf", "Manrope-Bold.ttf", "OFL.txt"] {
        copy(&workspace_root().join("assets/fonts").join(name), &fonts.join(name))?;
    }
    let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Neutron</string>
<key>CFBundleDisplayName</key><string>Neutron</string>
<key>CFBundleIdentifier</key><string>com.neutron.filemanager</string>
<key>CFBundleExecutable</key><string>neutron</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
<key>CFBundleIconFile</key><string>Neutron</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>CFBundleDocumentTypes</key><array>
  <dict>
    <key>CFBundleTypeName</key><string>Folder</string>
    <key>CFBundleTypeRole</key><string>Viewer</string>
    <key>LSHandlerRank</key><string>Alternate</string>
    <key>LSItemContentTypes</key><array><string>public.folder</string></array>
  </dict>
  <dict>
    <key>CFBundleTypeName</key><string>Item</string>
    <key>CFBundleTypeRole</key><string>Viewer</string>
    <key>LSHandlerRank</key><string>Alternate</string>
    <key>LSItemContentTypes</key><array><string>public.item</string></array>
  </dict>
</array>
<key>NSDesktopFolderUsageDescription</key><string>Neutron browses and manages files you choose on your Desktop.</string>
<key>NSDocumentsFolderUsageDescription</key><string>Neutron browses and manages files you choose in Documents.</string>
<key>NSDownloadsFolderUsageDescription</key><string>Neutron browses and manages files you choose in Downloads.</string>
<key>NSRemovableVolumesUsageDescription</key><string>Neutron browses and manages files on connected drives.</string>
<key>NSNetworkVolumesUsageDescription</key><string>Neutron browses and manages files on network volumes.</string>
<key>NSAppleEventsUsageDescription</key><string>Neutron uses Apple Events to reveal items and run macOS actions you trigger.</string>
</dict></plist>
"#, version = env!("CARGO_PKG_VERSION"));
    std::fs::write(stage.join("Contents/Info.plist"), plist).map_err(|e| e.to_string())?;
    let env_id = std::env::var("NEUTRON_SIGN_IDENTITY").ok().filter(|s| !s.is_empty());
    let entitlements = workspace_root().join("assets/entitlements.plist");
    sign_bundle(&stage, env_id.as_deref(), &entitlements)?;
    if bundle.exists() { std::fs::remove_dir_all(&bundle).map_err(|e| e.to_string())?; }
    std::fs::rename(&stage, &bundle).map_err(|e| e.to_string())?;
    println!("Built {}", bundle.display());
    Ok(bundle)
}
fn icon(resources: &Path, built: &Path) -> Result<(), String> {
    let iconset = built.join("Neutron.iconset");
    std::fs::create_dir_all(&iconset).map_err(|e| e.to_string())?;
    for size in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let pixels = (size * scale).to_string();
            let suffix = if scale == 2 { "@2x" } else { "" };
            run(Command::new("/usr/bin/sips").args(["-s", "format", "png", "-z", &pixels, &pixels])
                .arg(workspace_root().join("assets/icon.png"))
                .arg("--out").arg(iconset.join(format!("icon_{size}x{size}{suffix}.png"))))?;
        }
    }
    run(Command::new("/usr/bin/iconutil").args(["-c", "icns"]).arg(&iconset)
        .arg("-o").arg(resources.join("Neutron.icns")))
}
fn install_dir() -> Result<PathBuf, String> {
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?)
        .join("Applications/Neutron.app"))
}
pub fn install(args: &[String]) -> Result<(), String> {
    let source = bundle(args)?;
    let destination = install_dir()?;
    std::fs::create_dir_all(destination.parent().unwrap()).map_err(|e| e.to_string())?;
    // Replaced, not merged: `ditto` into an existing bundle keeps files the new
    // build no longer has, and a bundle with stray files fails its own code
    // signature check. Copied beside it first, so a failed copy leaves the
    // installed app as it was.
    let staged = destination.with_extension("installing.app");
    if staged.exists() {
        std::fs::remove_dir_all(&staged).map_err(|e| e.to_string())?;
    }
    run(Command::new("/usr/bin/ditto").arg(source).arg(&staged))?;
    if destination.exists() {
        std::fs::remove_dir_all(&destination).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&staged, &destination).map_err(|e| e.to_string())?;
    println!("Installed {}", destination.display());
    Ok(())
}
pub fn uninstall() -> Result<(), String> {
    let destination = install_dir()?;
    if destination.exists() { std::fs::remove_dir_all(&destination).map_err(|e| e.to_string())?; }
    println!("Removed {}", destination.display());
    Ok(())
}
pub fn show_paths() -> Result<(), String> {
    println!("Release bundle: {}", target_dir(false).join("Neutron.app").display());
    println!("Install location: {}", install_dir()?.display());
    Ok(())
}

fn sign_bundle(app: &Path, identity: Option<&str>, entitlements: &Path) -> Result<(), String> {
    let macos = app.join("Contents/MacOS");
    let bridge = macos.join("neutron-macos");
    let main_bin = macos.join("neutron");
    match identity {
        Some(id) if id != "-" => {
            for target in [bridge.as_path(), main_bin.as_path(), app] {
                run(Command::new("/usr/bin/codesign")
                    .args(["--force", "--options", "runtime", "--timestamp", "--entitlements"])
                    .arg(entitlements)
                    .args(["--sign", id])
                    .arg(target))?;
            }
        }
        _ => {
            // Ad-hoc signing permits local use; distribution still requires a
            // Developer ID signature and notarization by the publisher.
            run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&bridge))?;
            run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(app))?;
        }
    }
    run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(app))
}

fn resolve_identity(explicit: Option<&str>) -> Result<String, String> {
    if let Some(id) = explicit.filter(|s| !s.is_empty()) {
        return Ok(id.to_owned());
    }
    if let Ok(id) = std::env::var("NEUTRON_SIGN_IDENTITY")
        && !id.is_empty()
    {
        return Ok(id);
    }
    let output = Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map_err(|e| format!("could not run security find-identity: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ids = Vec::new();
    for line in stdout.lines() {
        if line.contains("Developer ID Application:")
            && let Some(start) = line.find('"')
            && let Some(end) = line[start + 1..].find('"')
        {
            ids.push(line[start + 1..start + 1 + end].to_owned());
        }
    }
    match ids.len() {
        1 => Ok(ids.into_iter().next().unwrap()),
        0 => Err("no Developer ID Application identity found; pass --identity or set NEUTRON_SIGN_IDENTITY".into()),
        _ => Err(format!(
            "multiple Developer ID Application identities found ({}); pass --identity or set NEUTRON_SIGN_IDENTITY",
            ids.join(", ")
        )),
    }
}

struct NotarizeOpts {
    app: PathBuf,
    identity: Option<String>,
    profile: String,
    no_dmg: bool,
}

fn parse_notarize_opts(args: &[String]) -> Result<NotarizeOpts, String> {
    let mut app: Option<PathBuf> = None;
    let mut identity: Option<String> = None;
    let mut profile = std::env::var("NEUTRON_NOTARY_PROFILE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "neutron-notary".to_owned());
    let mut no_dmg = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--identity" => {
                i += 1;
                let val = args.get(i).ok_or("--identity requires a value")?;
                identity = Some(val.clone());
            }
            "--profile" => {
                i += 1;
                let val = args.get(i).ok_or("--profile requires a value")?;
                profile = val.clone();
            }
            "--no-dmg" => no_dmg = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
            path if app.is_none() => app = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument: {extra}")),
        }
        i += 1;
    }
    let app = app.unwrap_or_else(|| target_dir(false).join("Neutron.app"));
    Ok(NotarizeOpts { app, identity, profile, no_dmg })
}

pub fn sign(args: &[String]) -> Result<(), String> {
    let opts = parse_notarize_opts(args)?;
    if !opts.app.exists() {
        return Err(format!("app bundle not found at {}", opts.app.display()));
    }
    let identity = resolve_identity(opts.identity.as_deref())?;
    let entitlements = workspace_root().join("assets/entitlements.plist");
    sign_bundle(&opts.app, Some(&identity), &entitlements)?;
    println!("Signed {} with {identity}", opts.app.display());
    Ok(())
}

pub fn notarize(args: &[String]) -> Result<(), String> {
    let opts = parse_notarize_opts(args)?;
    if !opts.app.exists() {
        return Err(format!("app bundle not found at {}", opts.app.display()));
    }
    let identity = resolve_identity(opts.identity.as_deref())?;
    let entitlements = workspace_root().join("assets/entitlements.plist");
    sign_bundle(&opts.app, Some(&identity), &entitlements)?;

    let parent = opts.app.parent().unwrap_or_else(|| Path::new("."));
    let zip = parent.join("Neutron-notarize.zip");
    if zip.exists() {
        std::fs::remove_file(&zip).map_err(|e| e.to_string())?;
    }
    run(Command::new("/usr/bin/ditto").args(["-c", "-k", "--keepParent"]).arg(&opts.app).arg(&zip))?;
    let submit = run(Command::new("/usr/bin/xcrun")
        .args(["notarytool", "submit"])
        .arg(&zip)
        .args(["--keychain-profile", &opts.profile, "--wait"]));
    let _ = std::fs::remove_file(&zip);
    submit?;

    run(Command::new("/usr/bin/xcrun").args(["stapler", "staple"]).arg(&opts.app))?;
    run(Command::new("/usr/bin/xcrun").args(["stapler", "validate"]).arg(&opts.app))?;
    let _ = run(Command::new("/usr/sbin/spctl").args(["-a", "-t", "execute", "-v"]).arg(&opts.app));
    println!("Notarized and stapled {}", opts.app.display());

    if !opts.no_dmg {
        let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
        let version = env!("CARGO_PKG_VERSION");
        let dmg = parent.join(format!("Neutron-{version}-{arch}.dmg"));
        let dmg_stage = parent.join("Neutron-dmg.staging");
        if dmg_stage.exists() {
            std::fs::remove_dir_all(&dmg_stage).map_err(|e| e.to_string())?;
        }
        if dmg.exists() {
            std::fs::remove_file(&dmg).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir_all(&dmg_stage).map_err(|e| e.to_string())?;
        run(Command::new("/usr/bin/ditto").arg(&opts.app).arg(dmg_stage.join("Neutron.app")))?;
        std::os::unix::fs::symlink("/Applications", dmg_stage.join("Applications")).map_err(|e| e.to_string())?;
        let mk_dmg = run(Command::new("/usr/bin/hdiutil")
            .args(["create", "-volname", "Neutron", "-srcfolder"])
            .arg(&dmg_stage)
            .args(["-ov", "-format", "UDZO"])
            .arg(&dmg));
        let _ = std::fs::remove_dir_all(&dmg_stage);
        mk_dmg?;

        run(Command::new("/usr/bin/codesign").args(["--force", "--timestamp", "--sign", &identity]).arg(&dmg))?;
        run(Command::new("/usr/bin/xcrun")
            .args(["notarytool", "submit"])
            .arg(&dmg)
            .args(["--keychain-profile", &opts.profile, "--wait"]))?;
        run(Command::new("/usr/bin/xcrun").args(["stapler", "staple"]).arg(&dmg))?;
        run(Command::new("/usr/bin/xcrun").args(["stapler", "validate"]).arg(&dmg))?;
        run(Command::new("/usr/sbin/spctl").args(["-a", "-t", "open", "--context", "context:primary-signature", "-v"]).arg(&dmg))?;
        println!("Built notarized DMG {}", dmg.display());
    }
    Ok(())
}
