//! There is a menu bar only on the Mac. Elsewhere these accept the same
//! description and do nothing with it.

use std::collections::HashSet;
use std::ffi::CStr;

#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub key: &'static str,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

impl Key {
    pub const fn cmd(key: &'static str) -> Key {
        Key { key, shift: false, option: false, control: false }
    }
    pub const fn shift(self) -> Key {
        Key { shift: true, ..self }
    }
    pub const fn option(self) -> Key {
        Key { option: true, ..self }
    }
    pub const fn control(self) -> Key {
        Key { control: true, ..self }
    }
}

pub const UP: &str = "\u{F700}";
pub const BACKSPACE: &str = "\u{8}";

pub enum Entry {
    Item { tag: u32, title: String, key: Option<Key> },
    System { title: String, action: &'static CStr, key: Option<Key> },
    Separator,
    Submenu { title: String, entries: Vec<Entry> },
}

pub struct Menu {
    pub title: String,
    pub entries: Vec<Entry>,
    pub windows: bool,
}

pub type Chosen = u32;

pub fn set_waker(_wake: impl Fn() + Send + Sync + 'static) {}
pub fn take_chosen() -> Vec<Chosen> {
    Vec::new()
}
pub fn set_state(_disabled: HashSet<u32>, _checked: HashSet<u32>) {}
pub fn set_titles(_titles: std::collections::HashMap<u32, String>) {}
pub fn install(_menus: Vec<Menu>) -> Result<(), String> {
    Ok(())
}
