//! Observe paste before egui-winit drops file-only clipboard events. This is a
//! local monitor for Neutron's own window, not a system-wide keyboard hook.
use std::cell::RefCell;
use std::ptr::NonNull;
use block2::RcBlock;
use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

struct Monitor(Retained<AnyObject>);
impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: the token came from addLocalMonitor below and stays on the
        // main thread until removed here.
        unsafe { NSEvent::removeMonitor(&self.0); }
    }
}
thread_local! { static MONITOR: RefCell<Option<Monitor>> = const { RefCell::new(None) }; }

pub fn watch(_owner: isize, tx: crossbeam_channel::Sender<()>) -> Result<(), String> {
    MainThreadMarker::new().ok_or("paste monitoring must start on the main thread")?;
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
        // SAFETY: AppKit supplies a live event for the callback's duration.
        let e = unsafe { event.as_ref() };
        let flags = e.modifierFlags();
        // Command+V only: Control+V is not paste on macOS, and a held key
        // must paste once rather than on every auto-repeat.
        if flags.contains(NSEventModifierFlags::Command) && !e.isARepeat()
            && !flags.intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Option | NSEventModifierFlags::Shift)
            && e.charactersIgnoringModifiers().is_some_and(|s| s.to_string().eq_ignore_ascii_case("v"))
        {
            let _ = tx.send(());
        }
        // Leave the event intact so text widgets still get ordinary paste.
        event.as_ptr()
    });
    // SAFETY: the handler returns the same valid event pointer; AppKit copies
    // the block, and MONITOR holds the token until main-thread teardown.
    let token = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) }
        .ok_or("could not monitor paste keys")?;
    MONITOR.with(|slot| *slot.borrow_mut() = Some(Monitor(token)));
    Ok(())
}
