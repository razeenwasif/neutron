// No console window on Windows release builds. Debug builds keep it so tracing
// output is visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Neutron — a faster Windows file explorer.
//!
//! # Threading contract
//!
//! **The UI thread only ever touches plain memory.** Every filesystem call,
//! every COM call, every network request happens on a worker and returns
//! through a `crossbeam-channel`; the paint thread renders whatever snapshot is
//! current and never waits for one.
//!
//! This is the whole performance argument. Most of what makes Explorer feel
//! slow is a blocking call on the thread that draws — a stat on a sleeping
//! drive, an icon handler, a cloud placeholder fetch. Refusing to have any such
//! call is most of the win, and it is a property that has to be maintained
//! deliberately: a single innocent-looking `std::fs::metadata` in a paint
//! function reintroduces the stall.

mod address;
mod app;
mod archive_ops;
mod archives;
mod bench;
#[cfg_attr(target_os = "macos", path = "chrome_macos.rs")]
mod chrome;
mod cloud;
mod columns;
mod commands;
mod confirm;
mod connect;
mod dashboard;
mod finder;
mod header;
mod icon_service;
mod index_client;
mod loader;
mod menubar;
mod notice;
mod preview;
mod panes;
mod persist;
mod pins;
mod place;
mod rail;
mod recent;
mod remote;
mod remote_ops;
mod plugins;
mod sidebar;
mod thumbs;
mod transfers;
mod undo;
mod startup;
mod workspace;

use std::time::Instant;

fn main() -> eframe::Result<()> {
    let launched = Instant::now();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NEUTRON_LOG")
                .unwrap_or_else(|_| "neutron=info,warn".into()),
        )
        // stderr, not stdout. Rust block-buffers stdout when it is redirected
        // to a file, so diagnostics from a still-running process sat unflushed
        // in an 8KB buffer and never appeared — which made the app look silent
        // while it was in fact logging. stderr is unbuffered.
        .with_writer(std::io::stderr)
        .init();

    // `--bench <path> [iterations]` measures enumeration and sorting without
    // opening a window, so directory-scaling numbers are not confounded by GPU
    // bring-up or frame pacing.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--bench") {
        let Some(path) = args.get(pos + 1) else {
            eprintln!("usage: neutron --bench <path> [iterations]");
            std::process::exit(2);
        };
        let iterations = args
            .get(pos + 2)
            .and_then(|n| n.parse().ok())
            .unwrap_or(6usize)
            .max(1);
        bench::run(path, iterations);
    }

    // An optional path argument opens that folder instead of the home
    // directory, so Neutron can stand in for Explorer from a shell or a
    // "open with" association.
    //
    // Made absolute against the directory it was typed in. `neutron .` would
    // otherwise open a location called "." — titled ".", with nowhere for Up
    // to go — and save it, to be resolved next time against wherever the next
    // launch happens to start, which from the Finder is `/`.
    let start_path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .map(|a| std::path::absolute(a).map_or_else(|_| a.clone(), |p| p.to_string_lossy().into_owned()));

    let options = eframe::NativeOptions {
        viewport: window_builder().with_inner_size([1280.0, 800.0]),
        // Not transparent on Windows. Measurement showed wgpu cannot provide a
        // surface with a transparent `CompositeAlphaMode` on any backend
        // available there (DX12, Vulkan, GL all logged the same rejection), so
        // requesting it only produced a warning per launch and no blur.
        // Neutron paints its own ground and drifting colour fields instead —
        // see `neutron_ui::ambient`.
        // wgpu rather than glow: the icon atlas will be a single large texture
        // with frequent partial uploads, which wgpu expresses far better.
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_config(),
        ..Default::default()
    };

    // Folders and files handed over by the system — "Open With Neutron", a
    // drop on the Dock icon. Listened for before eframe starts, because the
    // ones that launched the app arrive before it has built anything.
    let (open_tx, opened) = crossbeam_channel::unbounded();
    if let Err(e) = neutron_shell::open_requests::watch(open_tx) {
        tracing::warn!("{e}; files opened from the Finder will not reach Neutron");
    }

    let mut startup = startup::StartupTimer::new(launched);
    startup.mark_setup_done();

    eframe::run_native(
        "Neutron",
        options,
        Box::new(move |cc| Ok(Box::new(app::NeutronApp::new(cc, startup, start_path, opened)))),
    )
}

/// The window icon, generated from assets/icon.svg by scripts/render-icon.sh.
/// The macOS Dock uses the matching high-resolution ICNS in the app bundle.
/// How every Neutron window is made: the first, and each opened with New
/// Window, so they all have the same chrome.
pub(crate) fn window_builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("Neutron")
        .with_min_inner_size([720.0, 420.0])
        // Neutron draws its own title bar. A system one sits above the
        // layout as a separate strip in the OS's colours and corner radius,
        // and nothing done inside the window makes the two read as one
        // object. See `chrome.rs`, including what has to be put back by
        // hand once the OS stops drawing the frame.
        .with_decorations(cfg!(target_os = "macos"))
        .with_fullsize_content_view(cfg!(target_os = "macos"))
        .with_title_shown(!cfg!(target_os = "macos"))
        .with_titlebar_shown(!cfg!(target_os = "macos"))
        .with_titlebar_buttons_shown(true)
        .with_resizable(true)
        // On the Mac the window is transparent so that Neutron can round
        // its corners itself, and paints a solid ground inside them; see
        // `theme::paint_ground`. Metal composites a transparent surface.
        .with_transparent(cfg!(target_os = "macos"))
        // The OS traces a transparent window's shadow around whatever is
        // drawn. With the ground solid that is the rounded window, which is
        // the shadow a window should have. (When the cards floated over the
        // desktop it traced each card's own shadow instead, and was off.)
        .with_has_shadow(true)
        .with_icon(app_icon())
}

fn app_icon() -> egui::IconData {
    const RGBA: &[u8] = include_bytes!("../../../assets/icon_256.rgba");
    const SIZE: u32 = 256;

    // A truncated or mis-sized asset would otherwise show up as a garbled icon;
    // failing the build is far easier to diagnose.
    const _: () = assert!(RGBA.len() == (SIZE * SIZE * 4) as usize);

    egui::IconData {
        rgba: RGBA.to_vec(),
        width: SIZE,
        height: SIZE,
    }
}

/// Pins the GPU backend to DX12 on Windows.
///
/// wgpu's default backend order prefers Vulkan, and on Windows that is
/// measurably worse for a desktop app: the Vulkan loader enumerates every
/// registered layer at instance creation, which on a machine with capture or
/// overlay software (OBS, Steam, Discord) means loading third-party DLLs into
/// the process before the first frame. Measured here, that alone accounted for
/// most of a 1.4s cold start.
///
/// DX12 is also simply the better-supported path on Windows — no loader
/// indirection, no dependency on a vendor-installed ICD.
fn wgpu_config() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut config = eframe::egui_wgpu::WgpuConfiguration::default();

    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        #[cfg(windows)]
        {
            // Overridable because GPU bring-up dominates cold start and the
            // fastest backend is driver- and machine-dependent — being able to
            // A/B it without a rebuild is what turned a guess into a measurement.
            setup.instance_descriptor.backends = match std::env::var("NEUTRON_GPU_BACKEND")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "vulkan" => eframe::wgpu::Backends::VULKAN,
                "gl" => eframe::wgpu::Backends::GL,
                "all" => eframe::wgpu::Backends::all(),
                _ => eframe::wgpu::Backends::DX12,
            };
        }
        #[cfg(target_os = "macos")]
        { setup.instance_descriptor.backends = eframe::wgpu::Backends::METAL; }
        // A file manager is not a game: integrated graphics draw this UI
        // perfectly well, and forcing the discrete GPU awake costs battery and
        // adds seconds of spin-up on hybrid laptops.
        setup.power_preference = match std::env::var("NEUTRON_GPU_POWER")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "high" => eframe::wgpu::PowerPreference::HighPerformance,
            _ => eframe::wgpu::PowerPreference::LowPower,
        };
    }

    config
}
