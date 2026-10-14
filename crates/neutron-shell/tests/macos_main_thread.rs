//! Tests that need the main thread. `harness = false`, so `main` here is the
//! process's real main thread rather than a test-runner worker.

#[cfg(target_os = "macos")]
fn main() {
    open_documents_event_reaches_the_channel();
    drag_without_a_window_is_an_error_not_a_crash();
    quick_look_without_a_window_is_an_error_not_a_crash();
    println!("macos_main_thread: 3 passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {}

/// Sends a real `odoc` Apple Event to this process. Events addressed to the
/// current process are dispatched straight to the installed handler, so this
/// exercises the handler registration and the descriptor parsing end to end.
#[cfg(target_os = "macos")]
fn open_documents_event_reaches_the_channel() {
    use objc2::{ClassType, msg_send};
    use objc2::rc::Retained;
    use objc2_foundation::{NSAppleEventDescriptor, NSError, NSString, NSURL};

    let (tx, rx) = crossbeam_channel::unbounded();
    neutron_shell::open_requests::watch(tx.clone()).expect("installs on the main thread");
    assert!(neutron_shell::open_requests::watch(tx).is_err(), "a second install is refused");

    let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
    let list = NSAppleEventDescriptor::listDescriptor();
    for (i, p) in ["/tmp", "/Applications"].iter().enumerate() {
        let url = NSURL::fileURLWithPath(&NSString::from_str(p));
        list.insertDescriptor_atIndex(&NSAppleEventDescriptor::descriptorWithFileURL(&url), i as isize + 1);
    }
    let target = NSAppleEventDescriptor::currentProcessDescriptor();
    // kAutoGenerateReturnID = -1, kAnyTransactionID = 0.
    unsafe {
        let event: Retained<NSAppleEventDescriptor> = msg_send![NSAppleEventDescriptor::class(),
            appleEventWithEventClass: code(b"aevt"), eventID: code(b"odoc"),
            targetDescriptor: &*target, returnID: -1i16, transactionID: 0i32];
        let _: () = msg_send![&event, setParamDescriptor: &*list, forKeyword: code(b"----")];
        // kAENoReply = 1.
        let mut error: *mut NSError = std::ptr::null_mut();
        let _reply: Option<Retained<NSAppleEventDescriptor>> =
            msg_send![&event, sendEventWithOptions: 1usize, timeout: 5.0f64, error: &mut error];
        assert!(error.is_null(), "sending the event failed");
    }
    let paths = rx.recv_timeout(std::time::Duration::from_secs(2)).expect("the handler forwarded the paths");
    assert_eq!(paths, vec![std::path::PathBuf::from("/tmp"), std::path::PathBuf::from("/Applications")]);
}

#[cfg(target_os = "macos")]
fn drag_without_a_window_is_an_error_not_a_crash() {
    let result = neutron_shell::dragout::drag(&[std::path::PathBuf::from("/tmp")], 0);
    let err = result.expect_err("there is no window to drag from");
    assert!(err.contains("window"), "{err}");
}

/// The panel's class is reachable (Quartz is linked), it starts closed, and
/// asking for it with no window to attach to says so rather than raising.
#[cfg(target_os = "macos")]
fn quick_look_without_a_window_is_an_error_not_a_crash() {
    use neutron_shell::quicklook;
    assert!(objc2::runtime::AnyClass::get(c"QLPreviewPanel").is_some(), "Quartz is not linked");
    assert!(!quicklook::is_open());
    let err = quicklook::toggle(&[std::path::PathBuf::from("/tmp")], 0).expect_err("there is no window");
    assert!(err.contains("window"), "{err}");
    assert!(quicklook::take_moves().is_empty());
    quicklook::follow(&[std::path::PathBuf::from("/tmp")], 0);
}
