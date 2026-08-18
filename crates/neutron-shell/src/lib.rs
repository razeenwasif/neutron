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
//! Each module has a Windows implementation and a non-Windows stub selected by
//! `#[path]`, so the module name is the same on both. The stubs exist purely so
//! that `neutron-core`, `neutron-ui` and the portable halves of this crate can
//! be built and tested on Linux, which is a far faster inner loop than
//! cross-compiling. They are never shipped.
//!
//! Filled in at M1 (fast enumeration), M2 (sidebar places) and M3 (icons, file
//! launching, context menus).

#[cfg_attr(windows, path = "backdrop.rs")]
#[cfg_attr(not(windows), path = "backdrop_stub.rs")]
pub mod backdrop;

#[cfg_attr(windows, path = "fs.rs")]
#[cfg_attr(not(windows), path = "fs_stub.rs")]
pub mod fs;

#[cfg_attr(windows, path = "places.rs")]
#[cfg_attr(not(windows), path = "places_stub.rs")]
pub mod places;

#[cfg_attr(windows, path = "open.rs")]
#[cfg_attr(not(windows), path = "open_stub.rs")]
pub mod open;

#[cfg_attr(windows, path = "fileops.rs")]
#[cfg_attr(not(windows), path = "fileops_stub.rs")]
pub mod fileops;

#[cfg_attr(windows, path = "clipboard.rs")]
#[cfg_attr(not(windows), path = "clipboard_stub.rs")]
pub mod clipboard;

#[cfg_attr(windows, path = "dragout.rs")]
#[cfg_attr(not(windows), path = "dragout_stub.rs")]
pub mod dragout;

#[cfg_attr(windows, path = "pastekey.rs")]
#[cfg_attr(not(windows), path = "pastekey_stub.rs")]
pub mod pastekey;

#[cfg_attr(windows, path = "menu.rs")]
#[cfg_attr(not(windows), path = "menu_stub.rs")]
pub mod menu;

#[cfg_attr(windows, path = "shell_ns.rs")]
#[cfg_attr(not(windows), path = "shell_ns_stub.rs")]
pub mod shell_ns;

/// Icon resolution. Unlike the modules above this is a single file: the cache
/// key logic is pure and worth testing on Linux, so only the Win32 half inside
/// it is gated.
pub mod icons;

pub mod sta;
