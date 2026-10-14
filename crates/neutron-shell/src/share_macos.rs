//! Sharing files the way the Finder's Share menu does: the system's picker,
//! with AirDrop, Mail, Messages and whatever else is installed, or AirDrop
//! straight away.

use std::cell::RefCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSSharingService, NSSharingServiceNameSendViaAirDrop, NSSharingServicePicker};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};

thread_local! {
    /// Whatever is on screen, kept alive until the next share replaces it:
    /// the system holds neither the picker nor the service for us.
    static SHOWN: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// File URLs for `paths`, as the untyped array the sharing APIs take.
fn urls(paths: &[PathBuf]) -> Result<Retained<NSArray>, String> {
    let urls: Vec<Retained<AnyObject>> = paths
        .iter()
        .map(|p| {
            crate::macos::path_str(p)
                .map(|s| Retained::into_super(Retained::into_super(NSURL::fileURLWithPath(&NSString::from_str(s)))))
        })
        .collect::<Result<_, _>>()?;
    Ok(NSArray::from_retained_slice(&urls))
}

/// Shows the share picker for `paths`, pointing at `at` — in points from the
/// key window's top-left. Main thread only.
pub fn share(paths: &[PathBuf], at: (f32, f32)) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("sharing can only be started from the main thread")?;
    if paths.is_empty() {
        return Ok(());
    }
    let app = NSApplication::sharedApplication(mtm);
    let window = app.keyWindow().or_else(|| app.mainWindow()).ok_or("Neutron has no window to share from")?;
    let view = window.contentView().ok_or("Neutron's window has no view")?;
    let items = urls(paths)?;
    // A one-point rectangle where the pointer is, in the view's own
    // coordinates, which count up from the bottom unless it is flipped.
    let height = view.bounds().size.height;
    let y = if view.isFlipped() { at.1 as f64 } else { height - at.1 as f64 };
    let anchor = NSRect::new(NSPoint::new(at.0 as f64, y), NSSize::new(1.0, 1.0));
    // SAFETY: the picker takes an array of objects to share; file URLs are
    // what the Finder gives it.
    let picker = unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
    // AppKit raises rather than returning an error; that must not unwind
    // into Rust.
    objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
        picker.showRelativeToRect_ofView_preferredEdge(anchor, &view, NSRectEdge::MinY);
    }))
    .map_err(|_| "the share menu could not be shown".to_owned())?;
    SHOWN.with(|s| *s.borrow_mut() = Some(Retained::into_super(Retained::into_super(picker))));
    Ok(())
}

/// Sends `paths` with AirDrop, opening its window. Main thread only.
pub fn airdrop(paths: &[PathBuf]) -> Result<(), String> {
    let _mtm = MainThreadMarker::new().ok_or("AirDrop can only be started from the main thread")?;
    if paths.is_empty() {
        return Ok(());
    }
    // SAFETY: a named service from the system's own list.
    let service = unsafe { NSSharingService::sharingServiceNamed(NSSharingServiceNameSendViaAirDrop) }
        .ok_or("AirDrop is not available on this Mac")?;
    let items = urls(paths)?;
    // SAFETY: as for the picker, file URLs.
    if !unsafe { service.canPerformWithItems(Some(&items)) } {
        return Err("AirDrop cannot send these".into());
    }
    objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe { service.performWithItems(&items) }))
        .map_err(|_| "AirDrop could not be started".to_owned())?;
    SHOWN.with(|s| *s.borrow_mut() = Some(Retained::into_super(Retained::into_super(service))));
    Ok(())
}
