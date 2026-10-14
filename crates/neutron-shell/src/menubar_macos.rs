//! The menu bar: File, Edit, View, Go and Window, built from a description
//! the app hands over, with every item answered by the app.
//!
//! # Shortcuts and text fields
//!
//! A menu item's shortcut is claimed by the menu before the window sees the
//! key — except when the item is disabled, in which case the key goes on to
//! the window as if the menu were not there. That is the whole of how text
//! fields keep their keys: while one has focus, the app disables the items
//! whose shortcuts editing uses (Copy, Paste, Select All, Command-Delete…),
//! and those keys reach the field untouched.
//!
//! Enabled and checked are decided by the app each frame with [`set_state`]
//! and read by AppKit when it validates the menu, which it does before
//! showing one and before matching a shortcut.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSString};

/// A shortcut, as the menu shows and matches it.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// The character: a lower-case letter, a digit, punctuation, or one of
    /// the special keys below.
    pub key: &'static str,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

impl Key {
    /// Command and `key`.
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

/// The Up Arrow, as a key equivalent.
pub const UP: &str = "\u{F700}";
/// The Delete (backspace) key, as a key equivalent.
pub const BACKSPACE: &str = "\u{8}";

/// One entry in a menu.
pub enum Entry {
    /// Answered by the app, by `tag`.
    Item { tag: u32, title: String, key: Option<Key> },
    /// Answered by AppKit along the responder chain: Minimize, Zoom.
    System { title: String, action: &'static CStr, key: Option<Key> },
    Separator,
    Submenu { title: String, entries: Vec<Entry> },
}

/// A menu in the bar.
pub struct Menu {
    pub title: String,
    pub entries: Vec<Entry>,
    /// The Window menu, which macOS fills with the open windows.
    pub windows: bool,
}

/// A menu item chosen, by its tag.
pub type Chosen = u32;

define_class!(
    /// The target of every app-answered item.
    // SAFETY: NSObject has no subclassing requirements; nothing is overridden.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NeutronMenuTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(neutronCommand:))]
        fn command(&self, sender: &NSMenuItem) {
            let tag = sender.tag() as u32;
            CHOSEN.with(|c| c.borrow_mut().push(tag));
            if let Some(wake) = WAKER.get() {
                wake();
            }
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate(&self, item: &NSMenuItem) -> Bool {
            let tag = item.tag() as u32;
            let checked = STATE.with(|s| s.borrow().1.contains(&tag));
            item.setState(if checked { 1 } else { 0 });
            if let Some(title) = TITLES.with(|t| t.borrow().get(&tag).cloned()) {
                item.setTitle(&NSString::from_str(&title));
            }
            Bool::new(!STATE.with(|s| s.borrow().0.contains(&tag)))
        }
    }
);

thread_local! {
    static TARGET: OnceCell<Retained<Target>> = const { OnceCell::new() };
    static CHOSEN: RefCell<Vec<Chosen>> = const { RefCell::new(Vec::new()) };
    /// Disabled tags, checked tags.
    static STATE: RefCell<(HashSet<u32>, HashSet<u32>)> = RefCell::new((HashSet::new(), HashSet::new()));
    /// Titles that change with the state: "Undo Move to Trash".
    static TITLES: RefCell<HashMap<u32, String>> = RefCell::new(HashMap::new());
}

static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// How the app is woken when an item is chosen: winit does not see menus.
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(wake));
}

/// Items chosen since the last call.
pub fn take_chosen() -> Vec<Chosen> {
    CHOSEN.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Which items are disabled and which are ticked, from now on.
pub fn set_state(disabled: HashSet<u32>, checked: HashSet<u32>) {
    STATE.with(|s| *s.borrow_mut() = (disabled, checked));
}

/// Titles for items whose wording follows the state, applied when AppKit next
/// validates them. Items not named keep the title they were built with.
pub fn set_titles(titles: HashMap<u32, String>) {
    TITLES.with(|t| *t.borrow_mut() = titles);
}

/// Adds `menus` to the menu bar, after the application menu. Main thread,
/// once; a second call does nothing.
pub fn install(menus: Vec<Menu>) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("the menu bar can only be built on the main thread")?;
    if TARGET.with(|t| t.get().is_some()) {
        return Ok(());
    }
    let target: Retained<Target> = TARGET.with(|t| {
        t.get_or_init(|| {
            // SAFETY: plain NSObject initialisation of a class with no ivars.
            unsafe { msg_send![Target::alloc(mtm), init] }
        })
        .clone()
    });

    let app = NSApplication::sharedApplication(mtm);
    let bar = app.mainMenu().unwrap_or_else(|| {
        let bar = NSMenu::new(mtm);
        app.setMainMenu(Some(&bar));
        bar
    });
    for menu in menus {
        let built = build(mtm, &target, &menu.title, &menu.entries);
        let top = NSMenuItem::new(mtm);
        top.setTitle(&NSString::from_str(&menu.title));
        top.setSubmenu(Some(&built));
        bar.addItem(&top);
        if menu.windows {
            app.setWindowsMenu(Some(&built));
        }
    }
    Ok(())
}

fn build(mtm: MainThreadMarker, target: &Target, title: &str, entries: &[Entry]) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for entry in entries {
        match entry {
            Entry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            Entry::Item { tag, title, key } => {
                let item = item(mtm, title, Some(sel!(neutronCommand:)), *key);
                item.setTag(*tag as isize);
                // SAFETY: the target is kept alive for the process in
                // TARGET; a menu item holds its target unretained.
                unsafe { item.setTarget(Some(target as &AnyObject)) };
                menu.addItem(&item);
            }
            Entry::System { title, action, key } => {
                // No target: AppKit sends it along the responder chain, where
                // the window answers Minimize and Zoom itself.
                let item = item(mtm, title, Some(Sel::register(action)), *key);
                menu.addItem(&item);
            }
            Entry::Submenu { title, entries } => {
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(title));
                item.setSubmenu(Some(&build(mtm, target, title, entries)));
                menu.addItem(&item);
            }
        }
    }
    menu
}

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: Option<Key>) -> Retained<NSMenuItem> {
    let equivalent = NSString::from_str(key.map_or("", |k| k.key));
    // SAFETY: the action is a selector the target (or the chain) answers.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &equivalent,
        )
    };
    if let Some(key) = key {
        let mut mask = NSEventModifierFlags::Command;
        if key.shift {
            mask |= NSEventModifierFlags::Shift;
        }
        if key.option {
            mask |= NSEventModifierFlags::Option;
        }
        if key.control {
            mask |= NSEventModifierFlags::Control;
        }
        item.setKeyEquivalentModifierMask(mask);
    }
    item
}
