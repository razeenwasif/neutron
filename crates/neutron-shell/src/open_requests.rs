//! Documents macOS asks Neutron to open: "Open With ▸ Neutron" in Finder, a
//! folder dropped on the Dock icon, `open -a Neutron <path>`.
//!
//! macOS delivers these as Apple Events (`kAEOpenDocuments`, and `kAEGetURL`
//! for `file://` URLs) rather than as command-line arguments, and winit does
//! not surface them, so this installs handlers of its own and forwards the
//! paths over a channel. Elsewhere [`watch`] does nothing — documents arrive
//! on the command line there.
//!
//! The bundle's `Info.plist` must also declare what Neutron opens
//! (`CFBundleDocumentTypes`, e.g. `public.folder` and `public.item` with the
//! role `Viewer`), or Finder neither offers it nor lets a drop land.

use std::path::PathBuf;

use crossbeam_channel::Sender;

/// Starts forwarding open-document requests to `tx`, one `Vec` per request.
///
/// **Main thread only**, and **as early as possible** — ideally in `main`,
/// before `eframe::run_native`, keeping the receiver for the app. The request
/// that *launched* Neutron arrives while AppKit is still finishing launching,
/// before eframe constructs the app; installed later, this sees only requests
/// made while Neutron is already running. Installing early is safe: the
/// handlers are re-installed when AppKit posts
/// `NSApplicationWillFinishLaunchingNotification`, after its own defaults.
///
/// Returns an `Err` off the main thread, or if called twice.
pub fn watch(tx: Sender<Vec<PathBuf>>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return appkit::watch(tx);
    #[cfg(not(target_os = "macos"))]
    {
        drop(tx);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod appkit {
    use super::*;

    use std::cell::OnceCell;

    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{NSObject, NSObjectProtocol};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::NSApplicationWillFinishLaunchingNotification;
    use objc2_foundation::{
        NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSURL,
    };

    /// Four-character codes from `AERegistry.h`. Spelled out because the
    /// CoreServices bindings would be a whole framework crate for four numbers.
    const K_CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
    const K_AE_OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
    const K_INTERNET_EVENT_CLASS: u32 = u32::from_be_bytes(*b"GURL");
    const K_AE_GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and nothing is
        // overridden; the methods below are new selectors.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "NeutronOpenRequests"]
        #[ivars = Sender<Vec<PathBuf>>]
        struct Handler;

        unsafe impl NSObjectProtocol for Handler {}

        impl Handler {
            #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
            fn handle_open(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                if let Some(direct) = direct_object(event) {
                    self.forward(file_paths(&direct));
                }
            }

            #[unsafe(method(handleGetURL:withReplyEvent:))]
            fn handle_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                let url = direct_object(event).and_then(|d| d.stringValue());
                if let Some(path) = url.and_then(|u| file_url_path(&u.to_string())) {
                    self.forward(vec![path]);
                }
            }

            #[unsafe(method(applicationWillFinishLaunching:))]
            fn will_finish_launching(&self, _notification: &NSNotification) {
                self.install();
            }
        }
    );

    impl Handler {
        fn forward(&self, paths: Vec<PathBuf>) {
            if !paths.is_empty() {
                // A dropped receiver means the app has gone; nothing to do.
                let _ = self.ivars().send(paths);
            }
        }

        fn install(&self) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            // SAFETY: both selectors are implemented above with the
            // `(event, reply)` signature the manager calls, and `self` lives
            // for the rest of the process in `HANDLER`.
            unsafe {
                let _: () = msg_send![&manager, setEventHandler: self,
                    andSelector: sel!(handleOpenDocuments:withReplyEvent:),
                    forEventClass: K_CORE_EVENT_CLASS, andEventID: K_AE_OPEN_DOCUMENTS];
                let _: () = msg_send![&manager, setEventHandler: self,
                    andSelector: sel!(handleGetURL:withReplyEvent:),
                    forEventClass: K_INTERNET_EVENT_CLASS, andEventID: K_AE_GET_URL];
            }
        }
    }

    thread_local! {
        /// The handler lives as long as the process: the event manager and
        /// the notification centre hold it unretained.
        static HANDLER: OnceCell<Retained<Handler>> = const { OnceCell::new() };
    }

    pub(super) fn watch(tx: Sender<Vec<PathBuf>>) -> Result<(), String> {
        let mtm = MainThreadMarker::new()
            .ok_or("open requests must be watched from the main thread")?;
        HANDLER.with(|cell| {
            if cell.get().is_some() {
                return Err("open requests are already being watched".to_owned());
            }
            let handler = Handler::alloc(mtm).set_ivars(tx);
            // SAFETY: plain NSObject initialisation.
            let handler: Retained<Handler> = unsafe { msg_send![super(handler), init] };
            // Now, for a call made after launching; and again once AppKit
            // has installed its own defaults, for a call made before.
            handler.install();
            // SAFETY: the selector is implemented above with the signature a
            // notification observer needs, and the observer is never freed.
            // Reading the notification name does not create NSApp, which
            // must be left for winit to create as its own subclass.
            unsafe {
                NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                    &handler, sel!(applicationWillFinishLaunching:),
                    Some(NSApplicationWillFinishLaunchingNotification), None,
                );
            }
            let _ = cell.set(handler);
            Ok(())
        })
    }

    fn direct_object(event: &NSAppleEventDescriptor) -> Option<Retained<NSAppleEventDescriptor>> {
        // SAFETY: `paramDescriptorForKeyword:` takes an `AEKeyword` (UInt32)
        // and returns a nullable descriptor.
        unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] }
    }

    /// The file paths in an open-documents direct object: normally a list of
    /// file URLs or aliases, occasionally a single one.
    pub(super) fn file_paths(direct: &NSAppleEventDescriptor) -> Vec<PathBuf> {
        autoreleasepool(|_| {
            let path = |d: &NSAppleEventDescriptor| {
                d.fileURLValue().and_then(|u: Retained<NSURL>| u.path()).map(|p| PathBuf::from(p.to_string()))
            };
            let count = direct.numberOfItems();
            if count == 0 {
                return path(direct).into_iter().collect();
            }
            // Apple Event lists are 1-based.
            (1..=count).filter_map(|i| direct.descriptorAtIndex(i)).filter_map(|d| path(&d)).collect()
        })
    }

    /// The path of a `file://` URL; `None` for any other scheme.
    pub(super) fn file_url_path(url: &str) -> Option<PathBuf> {
        if !url.get(..7)?.eq_ignore_ascii_case("file://") {
            return None;
        }
        autoreleasepool(|_| {
            let url = NSURL::URLWithString(&objc2_foundation::NSString::from_str(url))?;
            url.path().map(|p| PathBuf::from(p.to_string()))
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::appkit::{file_paths, file_url_path};
    use super::*;
    use objc2_foundation::{NSAppleEventDescriptor, NSString, NSURL};

    #[test]
    fn a_list_of_file_urls_becomes_paths() {
        let list = NSAppleEventDescriptor::listDescriptor();
        for (i, p) in ["/tmp/a b", "/Users/日本語"].iter().enumerate() {
            let url = NSURL::fileURLWithPath(&NSString::from_str(p));
            list.insertDescriptor_atIndex(&NSAppleEventDescriptor::descriptorWithFileURL(&url), i as isize + 1);
        }
        assert_eq!(file_paths(&list), vec![PathBuf::from("/tmp/a b"), PathBuf::from("/Users/日本語")]);
        let single = NSAppleEventDescriptor::descriptorWithFileURL(&NSURL::fileURLWithPath(&NSString::from_str("/tmp")));
        assert_eq!(file_paths(&single), vec![PathBuf::from("/tmp")]);
    }

    #[test]
    fn only_file_urls_are_taken() {
        assert_eq!(file_url_path("file:///tmp/a%20b"), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(file_url_path("https://example.com/x"), None);
        assert_eq!(file_url_path("neutron://x"), None);
    }

    #[test]
    fn refuses_off_the_main_thread() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        assert!(watch(tx).is_err());
    }
}
