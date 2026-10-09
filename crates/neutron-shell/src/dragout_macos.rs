//! Dragging files out of Neutron, into Finder or anything else that accepts a
//! drop.
//!
//! # Main thread, and non-blocking
//!
//! The reverse of Windows. `DoDragDrop` is a modal loop that must not run on
//! the UI thread; `beginDraggingSession` is an AppKit view method that must
//! run *on* it, and returns at once — the drag then proceeds inside the
//! application's ordinary event loop, so the window keeps painting without any
//! help. [`drag`] therefore returns as soon as the session has begun, and
//! refuses (with an `Err`) when called from any other thread.
//!
//! # The event
//!
//! A dragging session must be started from a mouse-down or mouse-dragged
//! event. egui reports its drag from within a frame, by which time AppKit's
//! `currentEvent` is usually — but not always — the drag event that produced
//! it. When it is something else, an equivalent mouse-dragged event is made
//! at the pointer's current position.

use std::cell::OnceCell;
use std::path::PathBuf;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventType, NSWindow, NSWorkspace,
};
use objc2_foundation::{NSArray, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    Cancelled,
    Completed,
}

define_class!(
    /// Tells AppKit what a drop may do with the files.
    // SAFETY: NSObject has no subclassing requirements, and this adds no
    // ivars and overrides nothing.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NeutronDragSource"]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(&self, _session: &NSDraggingSession, context: NSDraggingContext) -> NSDragOperation {
            // Anything another application likes: Finder moves within a
            // volume and copies across, and makes an alias with ⌘⌥. Within
            // Neutron's own window, where winit accepts file drops as copies,
            // a link would mean nothing.
            if context == NSDraggingContext::OutsideApplication {
                NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Link
            } else {
                NSDragOperation::Copy | NSDragOperation::Move
            }
        }
    }
);

thread_local! {
    /// One source for the life of the process. Whether a session retains its
    /// source is not documented, so it is kept alive here rather than trusted
    /// to; it is stateless, so sharing it between drags costs nothing.
    static SOURCE: OnceCell<Retained<DragSource>> = const { OnceCell::new() };
}

/// Begins dragging `paths` out of the key window.
///
/// **Main (UI) thread only** — unlike Windows. Returns an `Err` saying so from
/// any other thread. Returns [`Dropped::Completed`] once the session has
/// begun; the drop itself happens later, inside the event loop, and is not
/// reported. A move by the target shows up through the folder watcher.
///
/// `owner` is unused: the drag starts from the key window.
pub fn drag(paths: &[PathBuf], _owner: isize) -> Result<Dropped, String> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err("on macOS a drag must be started from the main (UI) thread".to_owned());
    };
    if paths.is_empty() {
        return Ok(Dropped::Cancelled);
    }
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(mtm);
        let window = app.keyWindow().or_else(|| app.mainWindow())
            .ok_or("Neutron has no window to drag from")?;
        let view = window.contentView().ok_or("Neutron's window has no view to drag from")?;
        let event = drag_event(mtm, &app, &window)?;
        let origin = view.convertPoint_fromView(event.locationInWindow(), None);

        let workspace = NSWorkspace::sharedWorkspace();
        let mut items = Vec::with_capacity(paths.len());
        for (i, path) in paths.iter().enumerate() {
            let path = NSString::from_str(crate::macos::path_str(path)?);
            let url = NSURL::fileURLWithPath(&path);
            let item = NSDraggingItem::initWithPasteboardWriter(
                NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
            // Fanned slightly, like Finder's stack, for the first few; the
            // rest sit under them.
            let step = (i.min(4) * 4) as f64;
            let frame = NSRect::new(
                NSPoint::new(origin.x - 16.0 + step, origin.y - 16.0 + step),
                NSSize::new(32.0, 32.0),
            );
            let icon = workspace.iconForFile(&path);
            // SAFETY: the contents of a dragging frame may be an NSImage.
            unsafe { item.setDraggingFrame_contents(frame, Some(&icon)) };
            items.push(item);
        }
        let items = NSArray::from_retained_slice(&items);

        let source = SOURCE.with(|cell| {
            cell.get_or_init(|| {
                // SAFETY: plain NSObject initialisation of a class with no ivars.
                unsafe { msg_send![DragSource::alloc(mtm), init] }
            }).clone()
        });
        // AppKit raises rather than returning an error — for an event it
        // considers unsuitable, say — and an exception must not unwind into
        // Rust.
        objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            view.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&*source));
        }))
        .map_err(|e| match e {
            Some(e) => format!("macOS refused to start the drag: {e:?}"),
            None => "macOS refused to start the drag".to_owned(),
        })?;
        Ok(Dropped::Completed)
    })
}

/// The mouse event to start the session from: the current one when it is a
/// left-button press or drag in this window, otherwise a synthesised drag at
/// the pointer.
fn drag_event(mtm: MainThreadMarker, app: &NSApplication, window: &NSWindow) -> Result<Retained<NSEvent>, String> {
    if let Some(event) = app.currentEvent() {
        let mouse = matches!(event.r#type(), NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged);
        if mouse && event.window(mtm).is_some_and(|w| &*w == window) {
            return Ok(event);
        }
    }
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDragged,
        window.mouseLocationOutsideOfEventStream(),
        NSEvent::modifierFlags_class(),
        NSProcessInfo::processInfo().systemUptime(),
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
    .ok_or_else(|| "could not describe the drag to macOS".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_off_the_main_thread() {
        // Test bodies run on spawned threads, never the main one.
        let err = drag(&[PathBuf::from("/tmp")], 0).unwrap_err();
        assert!(err.contains("main"), "{err}");
    }
}
