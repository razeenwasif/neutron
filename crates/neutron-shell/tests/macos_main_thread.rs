//! Tests that need the main thread. `harness = false`, so `main` here is the
//! process's real main thread rather than a test-runner worker.

#[cfg(target_os = "macos")]
fn main() {
    open_documents_event_reaches_the_channel();
    drag_without_a_window_is_an_error_not_a_crash();
    quick_look_without_a_window_is_an_error_not_a_crash();
    menu_items_reach_the_app_and_follow_its_state();
    sharing_without_a_window_is_an_error_not_a_crash();
    println!("macos_main_thread: 5 passed");
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

/// A menu built from a description: choosing an item queues its tag, and the
/// state the app publishes disables and ticks items when AppKit validates.
#[cfg(target_os = "macos")]
fn menu_items_reach_the_app_and_follow_its_state() {
    use neutron_shell::menubar::{self, Entry, Key, Menu};
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    menubar::install(vec![Menu {
        title: "Test".into(),
        entries: vec![
            Entry::Item { tag: 7, title: "Seven".into(), key: Some(Key::cmd("7")) },
            Entry::Separator,
            Entry::Item { tag: 8, title: "Eight".into(), key: None },
        ],
        windows: false,
    }])
    .expect("installs");

    let bar = app.mainMenu().expect("a menu bar");
    let top = bar.itemAtIndex(bar.numberOfItems() - 1).expect("the new menu");
    let menu = top.submenu().expect("with items");
    assert_eq!(menu.numberOfItems(), 3);

    menu.performActionForItemAtIndex(0);
    assert_eq!(menubar::take_chosen(), vec![7]);
    assert!(menubar::take_chosen().is_empty(), "taken twice");

    menubar::set_state([7].into_iter().collect(), [8].into_iter().collect());
    menu.update();
    let seven = menu.itemAtIndex(0).unwrap();
    let eight = menu.itemAtIndex(2).unwrap();
    assert!(!seven.isEnabled(), "a disabled item stayed enabled");
    assert!(eight.isEnabled());
    assert_eq!(eight.state(), 1, "a checked item was not ticked");
    assert_eq!(seven.state(), 0);
}

/// The share picker needs a window to point from; with none it says so.
#[cfg(target_os = "macos")]
fn sharing_without_a_window_is_an_error_not_a_crash() {
    let err = neutron_shell::share::share(&[std::path::PathBuf::from("/tmp")], (10.0, 10.0)).expect_err("no window");
    assert!(err.contains("window"), "{err}");
    assert!(neutron_shell::share::share(&[], (0.0, 0.0)).is_ok(), "nothing to share is not an error");
}
