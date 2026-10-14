//! The system's Quick Look panel, inside Neutron: Space to open it on the
//! selection and to close it, the arrows to move through the folder with the
//! preview following.
//!
//! # How the panel finds its items
//!
//! `QLPreviewPanel` is shared by the whole application and asks for a
//! controller by walking the responder chain of the key window. Whoever says
//! yes to `acceptsPreviewPanelControl:` is handed the panel and becomes its
//! data source. Neutron's window belongs to winit, whose view cannot be given
//! new methods, so a responder of Neutron's own is inserted into the chain
//! just after that view — Apple's documented way to add behaviour to a chain
//! you do not own — and it answers for the panel.
//!
//! Everything here is AppKit and runs on the main thread. A request from any
//! other thread — the context menu is built on a worker — is queued and shown
//! by [`pump`], which the app calls each frame.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventType, NSResponder};
use objc2_foundation::{NSInteger, NSString, NSURL};

// `QLPreviewPanel` lives in Quartz, which nothing else here links.
#[link(name = "Quartz", kind = "framework")]
unsafe extern "C" {}

/// What the panel is showing.
#[derive(Default)]
struct Items {
    urls: RefCell<Vec<Retained<NSURL>>>,
}

define_class!(
    /// Answers for the panel: its controller, data source and delegate.
    // SAFETY: NSResponder has no subclassing requirements beyond being used
    // on the main thread, which `MainThreadOnly` enforces; nothing is
    // overridden that the superclass relies on.
    #[unsafe(super(NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NeutronQuickLook"]
    #[ivars = Items]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    impl Controller {
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts(&self, _panel: &AnyObject) -> Bool {
            Bool::YES
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin(&self, panel: &AnyObject) {
            // SAFETY: the panel takes any object for these, holding them
            // weakly; this one lives for the whole process (see CONTROLLER).
            unsafe {
                let _: () = msg_send![panel, setDataSource: self];
                let _: () = msg_send![panel, setDelegate: self];
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end(&self, _panel: &AnyObject) {}

        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn count(&self, _panel: &AnyObject) -> NSInteger {
            self.ivars().urls.borrow().len() as NSInteger
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn item(&self, _panel: &AnyObject, index: NSInteger) -> Option<Retained<NSURL>> {
            usize::try_from(index).ok().and_then(|i| self.ivars().urls.borrow().get(i).cloned())
        }

        /// Keys the panel does not use itself. With one item it has nothing
        /// to do with Up and Down, so they move Neutron's cursor instead, as
        /// in the Finder, and the preview follows.
        #[unsafe(method(previewPanel:handleEvent:))]
        fn handle(&self, panel: &AnyObject, event: &NSEvent) -> Bool {
            if event.r#type() != NSEventType::KeyDown {
                return Bool::NO;
            }
            match event.keyCode() {
                // Up and Down.
                126 | 125 => {
                    let delta = if event.keyCode() == 126 { -1 } else { 1 };
                    MOVES.with(|m| m.borrow_mut().push(delta));
                    wake();
                    Bool::YES
                }
                // Space closes, as it opened.
                49 => {
                    // SAFETY: orderOut: takes an optional sender.
                    let _: () = unsafe { msg_send![panel, orderOut: None::<&AnyObject>] };
                    Bool::YES
                }
                _ => Bool::NO,
            }
        }
    }
);

thread_local! {
    /// One controller for the life of the process: the panel holds its data
    /// source weakly, so something has to keep it alive.
    static CONTROLLER: OnceCell<Retained<Controller>> = const { OnceCell::new() };
    /// Cursor moves asked for from inside the panel, for the app to apply.
    static MOVES: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

/// A request from a thread that cannot touch AppKit.
static PENDING: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// How the app is woken when something here needs a frame: a request from a
/// worker, or a key pressed in the panel, neither of which winit sees.
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(wake));
}

fn wake() {
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

/// Asks for the panel from any thread. Shown at the next [`pump`].
pub fn request(paths: Vec<PathBuf>) {
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some(paths);
    }
    wake();
}

/// Shows anything requested from another thread. Main thread, each frame.
pub fn pump() {
    let pending = PENDING.lock().ok().and_then(|mut p| p.take());
    if let Some(paths) = pending {
        let _ = show(&paths, 0);
    }
}

/// Cursor moves made with the arrows while the panel had the keyboard.
pub fn take_moves() -> Vec<isize> {
    MOVES.with(|m| std::mem::take(&mut *m.borrow_mut()))
}

/// Opens the panel on `paths` at `index`, or closes it if it is open.
pub fn toggle(paths: &[PathBuf], index: usize) -> Result<(), String> {
    if is_open() {
        close();
        Ok(())
    } else {
        show(paths, index)
    }
}

/// Whether the panel is on screen.
pub fn is_open() -> bool {
    let Some(class) = panel_class() else { return false };
    // SAFETY: class methods of QLPreviewPanel; asking whether the shared
    // panel exists first, so this never creates one just to ask.
    unsafe {
        let exists: Bool = msg_send![class, sharedPreviewPanelExists];
        if !exists.as_bool() {
            return false;
        }
        let panel: Retained<AnyObject> = msg_send![class, sharedPreviewPanel];
        let visible: Bool = msg_send![&panel, isVisible];
        visible.as_bool()
    }
}

/// Shows `paths` in the open panel, if it is open: the cursor moved, and the
/// preview follows it.
pub fn follow(paths: &[PathBuf], index: usize) {
    if !is_open() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let controller = controller(mtm);
    set_items(&controller, paths);
    if let Some(class) = panel_class() {
        // SAFETY: the shared panel exists (it is open); these take no
        // ownership of their arguments.
        unsafe {
            let panel: Retained<AnyObject> = msg_send![class, sharedPreviewPanel];
            let _: () = msg_send![&panel, reloadData];
            let _: () = msg_send![&panel, setCurrentPreviewItemIndex: index as NSInteger];
        }
    }
}

fn close() {
    if let Some(class) = panel_class() {
        // SAFETY: as in `is_open`; orderOut: takes an optional sender.
        unsafe {
            let panel: Retained<AnyObject> = msg_send![class, sharedPreviewPanel];
            let _: () = msg_send![&panel, orderOut: None::<&AnyObject>];
        }
    }
}

fn show(paths: &[PathBuf], index: usize) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Quick Look can only be opened from the main thread")?;
    if paths.is_empty() {
        return Ok(());
    }
    let class = panel_class().ok_or("Quick Look is not available")?;
    autoreleasepool(|_| {
        let controller = controller(mtm);
        set_items(&controller, paths);
        insert_into_chain(mtm, &controller)?;
        // SAFETY: class and instance methods of QLPreviewPanel. AppKit can
        // raise from these, and an exception must not unwind into Rust.
        objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
            let panel: Retained<AnyObject> = msg_send![class, sharedPreviewPanel];
            let _: () = msg_send![&panel, updateController];
            let _: () = msg_send![&panel, reloadData];
            let _: () = msg_send![&panel, setCurrentPreviewItemIndex: index.min(paths.len() - 1) as NSInteger];
            let _: () = msg_send![&panel, makeKeyAndOrderFront: None::<&AnyObject>];
        }))
        .map_err(|e| match e {
            Some(e) => format!("Quick Look could not open: {e:?}"),
            None => "Quick Look could not open".to_owned(),
        })
    })
}

fn panel_class() -> Option<&'static AnyClass> {
    AnyClass::get(c"QLPreviewPanel")
}

fn controller(mtm: MainThreadMarker) -> Retained<Controller> {
    CONTROLLER.with(|cell| {
        cell.get_or_init(|| {
            let this = Controller::alloc(mtm).set_ivars(Items::default());
            // SAFETY: NSResponder's designated initialiser.
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

fn set_items(controller: &Controller, paths: &[PathBuf]) {
    let urls = paths
        .iter()
        .filter_map(|p| p.to_str())
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(p)))
        .collect();
    *controller.ivars().urls.borrow_mut() = urls;
}

/// Puts the controller in the key window's chain, just after its content
/// view, unless it is there already.
fn insert_into_chain(mtm: MainThreadMarker, controller: &Controller) -> Result<(), String> {
    let app = NSApplication::sharedApplication(mtm);
    let window = app.keyWindow().or_else(|| app.mainWindow()).ok_or("Neutron has no window")?;
    let view = window.contentView().ok_or("Neutron's window has no view")?;
    let ours: &NSResponder = controller;
    // SAFETY: the chain is only read and relinked here, on the main thread;
    // the controller outlives the window (see CONTROLLER), so the view never
    // points at a freed responder.
    unsafe {
        let next = view.nextResponder();
        if next.as_deref().is_some_and(|n| std::ptr::eq(n, ours)) {
            return Ok(());
        }
        controller.setNextResponder(next.as_deref());
        view.setNextResponder(Some(ours));
    }
    Ok(())
}
