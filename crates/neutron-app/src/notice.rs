//! One line that says what just happened.
//!
//! # Why this exists
//!
//! Until now, eleven file operations ended in a `tracing::warn!` and nothing on
//! screen. Rename to a name that already exists, paste into a folder you have
//! no rights to, delete a file another process has open — Neutron did the same
//! thing in every case, which was *nothing*, and looked exactly like a file
//! manager that had ignored the keystroke.
//!
//! That is the difference between a tool you use and a tool you check up on. A
//! file manager that fails silently is one you verify with a second file
//! manager afterwards, and at that point you are using the second one.
//!
//! # Errors stay, information goes
//!
//! An information notice — "3 files hashed", "2 servers saved" — clears itself
//! after a few seconds. It is a receipt for something the user watched happen.
//!
//! An **error does not**. It stays until it is replaced by something newer or
//! the user dismisses it. An error nobody saw is an error that did not happen,
//! and these are errors about their files.
//!
//! # One at a time
//!
//! The status bar has one line. Three separate note fields — one each for
//! archives, plugins and servers — meant three things competing for it and no
//! answer for which won. The newest thing that happened is what a person wants
//! to see, so the newest is what is kept.
//!
//! Nothing here replaces the log. Every notice is *also* logged, in full, with
//! whatever detail did not fit; the status bar is one line and some failures
//! have paragraphs behind them.

use std::time::{Duration, Instant};

/// How long an information notice stays before clearing itself.
///
/// Long enough to read a sentence after looking away and back; short enough
/// that it is gone before it becomes furniture.
pub const INFO_LIFETIME: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something worked. Clears itself.
    Info,
    /// Something did not work. Stays until replaced or dismissed.
    Error,
}

#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub level: Level,
    raised: Instant,
}

impl Notice {
    /// Whether this notice has outlived its welcome.
    pub fn expired(&self, now: Instant) -> bool {
        self.level == Level::Info && now.duration_since(self.raised) >= INFO_LIFETIME
    }

    /// How long until it clears, for scheduling a repaint. `None` for an error,
    /// which never clears on its own.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.level {
            Level::Error => None,
            Level::Info => Some(INFO_LIFETIME.saturating_sub(now.duration_since(self.raised))),
        }
    }
}

/// The one notice currently showing, if any.
#[derive(Debug, Default)]
pub struct Notices {
    current: Option<Notice>,
}

impl Notices {
    /// Reports something that worked.
    pub fn info(&mut self, text: impl Into<String>) {
        self.raise(Level::Info, text.into());
    }

    /// Reports something that did not.
    ///
    /// Also logs it: the status bar gets one line, and the log gets whatever
    /// else there was.
    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.raise(Level::Error, text);
    }

    /// Raises a notice that has *already* been logged where it happened —
    /// from a worker, through the channel. Logging it again here would put
    /// every worker failure in the log twice.
    pub fn raise_reported(&mut self, level: Level, text: String) {
        self.raise(level, text);
    }

    fn raise(&mut self, level: Level, text: String) {
        // An empty notice would be a blank line that looks like a rendering
        // fault rather than a message.
        if text.trim().is_empty() {
            return;
        }
        self.current = Some(Notice {
            text,
            level,
            raised: Instant::now(),
        });
    }

    /// Clears whatever is showing — the user clicked it away, or started
    /// something new that supersedes it.
    pub fn dismiss(&mut self) {
        self.current = None;
    }

    /// Clears an information notice, leaving an error alone.
    ///
    /// For the case where the thing the notice was about plainly finished —
    /// "Connecting to …" beside a listing that has visibly arrived. An error
    /// is never cleared this way: the user has still not acknowledged it.
    pub fn dismiss_info(&mut self) {
        if self.current.as_ref().is_some_and(|n| n.level == Level::Info) {
            self.current = None;
        }
    }

    /// Drops an information notice that has run its course.
    ///
    /// Returns how long until the next change, so the caller can schedule a
    /// repaint rather than relying on something else to keep the frame clock
    /// running.
    pub fn tick(&mut self) -> Option<Duration> {
        let now = Instant::now();
        match &self.current {
            Some(notice) if notice.expired(now) => {
                self.current = None;
                None
            }
            Some(notice) => notice.remaining(now),
            None => None,
        }
    }

    pub fn current(&self) -> Option<&Notice> {
        self.current.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_never_clears_itself() {
        // The whole point. A failure the user did not see is a failure that,
        // to them, did not happen.
        let mut notices = Notices::default();
        notices.error("could not rename");

        let notice = notices.current().expect("an error");
        assert!(!notice.expired(Instant::now() + INFO_LIFETIME * 100));
        assert_eq!(notice.remaining(Instant::now()), None);
    }

    #[test]
    fn information_clears_itself() {
        let mut notices = Notices::default();
        notices.info("3 files hashed");

        let notice = notices.current().expect("a notice");
        assert!(!notice.expired(Instant::now()));
        assert!(notice.expired(Instant::now() + INFO_LIFETIME));
    }

    #[test]
    fn the_newest_thing_wins() {
        // The status bar has one line, and what a person wants on it is
        // whatever just happened.
        let mut notices = Notices::default();
        notices.error("could not delete");
        notices.info("2 servers saved");
        assert_eq!(notices.current().unwrap().text, "2 servers saved");
    }

    #[test]
    fn an_error_can_replace_an_error() {
        let mut notices = Notices::default();
        notices.error("first");
        notices.error("second");
        assert_eq!(notices.current().unwrap().text, "second");
        assert_eq!(notices.current().unwrap().level, Level::Error);
    }

    #[test]
    fn dismissing_clears_it() {
        let mut notices = Notices::default();
        notices.error("could not paste");
        notices.dismiss();
        assert!(notices.current().is_none());
    }

    #[test]
    fn a_blank_message_is_not_shown() {
        // A blank status line reads as a rendering fault, not as a message.
        let mut notices = Notices::default();
        notices.info("   ");
        assert!(notices.current().is_none());
        notices.error("\n");
        assert!(notices.current().is_none());
    }

    #[test]
    fn ticking_reports_when_the_next_change_is_due() {
        // So the caller can schedule a repaint instead of relying on something
        // else to keep the frame clock running.
        let mut notices = Notices::default();
        notices.info("done");
        let remaining = notices.tick().expect("an information notice expires");
        assert!(remaining <= INFO_LIFETIME);

        notices.error("failed");
        assert_eq!(notices.tick(), None, "an error has no expiry to wait for");
        assert!(notices.current().is_some(), "and it is still showing");
    }

    #[test]
    fn clearing_information_leaves_an_error_standing() {
        // "Connecting…" should go once the folder is on screen. A failed
        // delete should not go just because something else succeeded.
        let mut notices = Notices::default();
        notices.info("connecting");
        notices.dismiss_info();
        assert!(notices.current().is_none());

        notices.error("delete failed");
        notices.dismiss_info();
        assert!(notices.current().is_some());
    }

    #[test]
    fn ticking_an_empty_surface_does_nothing() {
        assert_eq!(Notices::default().tick(), None);
    }
}
