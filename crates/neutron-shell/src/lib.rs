//! Windows filesystem and shell (COM) backends.
//!
//! # Threading contract
//!
//! Nothing in this crate may be called from the UI thread. COM calls here can
//! block for seconds — a disconnected network drive, a cloud placeholder that
//! triggers a download, a slow third-party context-menu handler — and the whole
//! performance argument for Neutron rests on the paint thread never blocking.
//!
//! Shell COM objects additionally require an STA, so they run on the
//! [`sta::StaPool`] and communicate over channels.
//!
//! # Platform selection
//!
//! Windows and macOS select their native backends at compile time. Other
//! platforms retain stubs for portable unit tests.

#[cfg(target_os = "macos")]
mod macos;

#[cfg_attr(windows, path = "backdrop.rs")]
#[cfg_attr(not(windows), path = "backdrop_stub.rs")]
pub mod backdrop;

#[cfg_attr(windows, path = "fs.rs")]
#[cfg_attr(target_os = "macos", path = "fs_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "fs_stub.rs")]
pub mod fs;

#[cfg_attr(windows, path = "places.rs")]
#[cfg_attr(target_os = "macos", path = "places_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "places_stub.rs")]
pub mod places;

#[cfg_attr(windows, path = "open.rs")]
#[cfg_attr(target_os = "macos", path = "open_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "open_stub.rs")]
pub mod open;

#[cfg_attr(windows, path = "fileops.rs")]
#[cfg_attr(target_os = "macos", path = "fileops_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "fileops_stub.rs")]
pub mod fileops;

#[cfg_attr(windows, path = "clipboard.rs")]
#[cfg_attr(target_os = "macos", path = "clipboard_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "clipboard_stub.rs")]
pub mod clipboard;

pub mod watch;

#[cfg_attr(windows, path = "dragout.rs")]
#[cfg_attr(target_os = "macos", path = "dragout_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "dragout_stub.rs")]
pub mod dragout;

#[cfg_attr(windows, path = "pastekey.rs")]
#[cfg_attr(target_os = "macos", path = "pastekey_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "pastekey_stub.rs")]
pub mod pastekey;

// Unlike its neighbours this needs no `_stub`: the Win32 half is already behind
// a `cfg` inside the file, because the shared rectangle and its tests are
// ordinary code worth compiling everywhere.
pub mod snaplayout;

#[cfg_attr(windows, path = "menu.rs")]
#[cfg_attr(target_os = "macos", path = "menu_macos.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "menu_stub.rs")]
pub mod menu;

#[cfg_attr(windows, path = "shell_ns.rs")]
#[cfg_attr(not(windows), path = "shell_ns_stub.rs")]
pub mod shell_ns;

/// Icon resolution. Unlike the modules above this is a single file: the cache
/// key logic is pure and worth testing on Linux, so only the Win32 half inside
/// it is gated.
pub mod icons;

pub mod thumbnails;

#[cfg_attr(target_os = "macos", path = "tags_macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "tags_stub.rs")]
pub mod tags;

#[cfg_attr(target_os = "macos", path = "menubar_macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "menubar_stub.rs")]
pub mod menubar;

#[cfg_attr(target_os = "macos", path = "quicklook_macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "quicklook_stub.rs")]
pub mod quicklook;

pub mod transfer;

pub mod sta;

/// Documents macOS asks Neutron to open (Finder's Open With, the Dock icon).
/// One file: the non-macOS half is a one-line no-op.
pub mod open_requests;

/// Launching a terminal and a second window. One file rather than a `#[path]`
/// pair: only the process-creation flags differ by platform.
pub mod terminal;

/// Windows Credential Manager. One file rather than a `#[path]` pair: only the
/// Win32 half inside it is gated, and the non-Windows stub is three functions.
pub mod credentials;
