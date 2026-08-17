//! Embeds the Windows application manifest.
//!
//! The manifest is what grants long-path support, per-monitor DPI awareness,
//! and Common Controls v6 (needed for themed shell context menus). Without it
//! embedded at link time none of those apply, so this is not optional polish.

fn main() {
    println!("cargo:rerun-if-changed=neutron.manifest");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("neutron.manifest");
        // Windows uses the first icon resource in the executable for the window,
        // the taskbar, and Explorer alike, so embedding it here is all that is
        // needed — no separate runtime call to set a window icon.
        res.set_icon("../../assets/icon.ico");
        res.set("FileDescription", "Neutron File Explorer");
        res.set("ProductName", "Neutron");
        res.set("OriginalFilename", "neutron.exe");
        if let Err(e) = res.compile() {
            // A missing resource compiler should not block development builds;
            // the app still runs, just without manifest-granted features.
            println!("cargo:warning=failed to embed manifest: {e}");
        }
    }
}
