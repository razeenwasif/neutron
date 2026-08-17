//! Neutron's widget layer: theme tokens and the reusable view components.
//!
//! Nothing here performs I/O. Widgets render whatever snapshot the app hands
//! them, which is what keeps the paint thread free of blocking calls.

pub mod ambient;
pub mod atlas;
pub mod file_list;
pub mod format;
pub mod icons;
pub mod theme;

pub use theme::{Palette, ROW_HEIGHT, ThemeMode};
