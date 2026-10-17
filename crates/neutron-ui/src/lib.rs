//! Neutron's widget layer: theme tokens and the reusable view components.
//!
//! Nothing here performs I/O. Widgets render whatever snapshot the app hands
//! them, which is what keeps the paint thread free of blocking calls.

pub mod ambient;
pub mod atlas;
pub mod columns;
pub mod context_menu;
pub mod drop;
pub mod file_list;
pub mod format;
pub mod highlight;
pub mod icons;
pub mod tags;
pub mod term_view;
pub mod theme;
pub mod thumb;

pub use context_menu::{MenuOutcome, MenuState};
pub use file_list::Marks;
pub use theme::{Palette, ROW_HEIGHT, ThemeMode};
