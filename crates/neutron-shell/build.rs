fn main() {
    println!("cargo:rerun-if-changed=macos/bridge.swift");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => panic!("unsupported macOS architecture: {other}"),
    };
    let helper = out.join("neutron-macos");
    let status = std::process::Command::new("xcrun")
        .args(["swiftc", "-O", "-target", &format!("{arch}-apple-macosx12.0"),
            "-module-cache-path"])
        .arg(out.join("swift-cache"))
        .arg("macos/bridge.swift").arg("-o").arg(&helper)
        .status().expect("install the Xcode command line tools to build Neutron on macOS");
    assert!(status.success(), "could not build the macOS shell bridge");
    println!("cargo:rustc-env=NEUTRON_MACOS_HELPER={}", helper.display());
}
