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
</dict></plist>
"#, version = env!("CARGO_PKG_VERSION"));
    std::fs::write(stage.join("Contents/Info.plist"), plist).map_err(|e| e.to_string())?;
    // Ad-hoc signing permits local use; distribution still requires a Developer
    // ID signature and notarization by the publisher.
    run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(macos.join("neutron-macos")))?;
    run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&stage))?;
    run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&stage))?;
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
