//! Hygiene for text and paths that came from outside.
//!
//! Two different kinds of outside, both untrusted:
//!
//! - **Manifests**, which a user installed but did not write. A title is drawn
//!   into a menu row.
//! - **Plugin output**, which a program produced while running. A status line is
//!   drawn into the status bar, and a path may be navigated to or opened.
//!
//! The failure modes are not hypothetical. A newline in a menu label makes one
//! row as tall as three and pushes the rest of the menu off screen. A label of
//! ten thousand characters makes a popup wider than the monitor with no way to
//! dismiss it, because the dismiss target is off screen too. A carriage return
//! in a status line overprints the text beside it. None of these need malice —
//! a plugin that prints an error message with a trailing newline produces the
//! first one by accident.
//!
//! So text arriving from either place is flattened to one line, stripped of
//! control characters, and cut to a length the layout can hold.

/// Longest menu or palette title kept. Beyond this the row is wider than a
/// sensible popup and the text is unreadable anyway.
pub const MAX_TITLE: usize = 80;

/// Longest status line kept. The status bar is one line across the window.
pub const MAX_STATUS: usize = 300;

/// Flattens untrusted text to a single line and caps its length.
///
/// Control characters — including the newlines and tabs that break layout —
/// become spaces, runs of whitespace collapse to one, and the result is trimmed
/// and truncated to `max` **characters**, not bytes, so a multi-byte character
/// is never cut in half.
///
/// Truncation appends an ellipsis, so a shortened label is visibly shortened
/// rather than looking like a label that happened to end there.
pub fn text(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max * 4));
    let mut pending_space = false;

    for c in raw.chars() {
        // Two kinds of unwanted character, handled differently.
        //
        // Invisible ones are *deleted*: the zero-width and bidi format
        // characters, which are not "control" to Rust but hide or reorder the
        // text around them — the trick behind a label that reads as one thing
        // and is another. Replacing them with a space would leave a visible gap
        // mid-word, so they leave nothing.
        if matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                     | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        {
            continue;
        }
        // Layout-breaking ones — newlines, tabs, C0 and C1 controls — become a
        // space, because they stood between words and removing them outright
        // would run those words together.
        if c.is_control() || c.is_whitespace() {
            // Leading whitespace never becomes a space, so the result needs no
            // trim afterwards.
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }

    if out.chars().count() > max {
        // `max - 1` leaves room for the ellipsis, so the whole thing still fits
        // whatever `max` was chosen to fit.
        let keep = max.saturating_sub(1);
        out = out.chars().take(keep).collect::<String>();
        // A truncation landing mid-word looks less like a mistake without the
        // dangling partial word's trailing space.
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('…');
    }

    out
}

/// A title for a menu row or a palette entry.
pub fn title(raw: &str) -> String {
    text(raw, MAX_TITLE)
}

/// A line for the status bar.
pub fn status(raw: &str) -> String {
    text(raw, MAX_STATUS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newline_cannot_make_a_menu_row_three_lines_tall() {
        assert_eq!(text("Copy\nhash\nnow", 80), "Copy hash now");
    }

    #[test]
    fn a_carriage_return_cannot_overprint_the_status_bar() {
        assert_eq!(text("done\rHACKED", 80), "done HACKED");
    }

    #[test]
    fn runs_of_whitespace_collapse() {
        assert_eq!(text("  a \t\t  b  ", 80), "a b");
    }

    #[test]
    fn a_long_label_is_cut_and_says_so() {
        let out = text(&"x".repeat(500), 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // Cutting by bytes here would produce invalid UTF-8 or a replacement
        // character. Every kept character must survive whole.
        let out = text("日本語のファイル名をとても長く", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.chars().all(|c| c == '…' || c.is_alphabetic()));
    }

    #[test]
    fn text_that_fits_is_left_alone() {
        assert_eq!(text("Copy SHA-256", 80), "Copy SHA-256");
        assert!(!text("Copy SHA-256", 80).ends_with('…'));
    }

    #[test]
    fn bidi_overrides_are_stripped() {
        // U+202E reverses the text after it, which is how "gnp.exe" is made to
        // read as "exe.png" in a list. A label is never worth that.
        let out = text("photo\u{202E}gnp.exe", 80);
        assert!(!out.contains('\u{202E}'), "{out:?}");
    }

    #[test]
    fn zero_width_characters_cannot_hide_inside_a_label() {
        assert_eq!(text("Cop\u{200B}y", 80), "Copy");
    }

    #[test]
    fn an_empty_or_all_whitespace_label_comes_back_empty() {
        // The caller rejects the plugin over this; it must not come back as a
        // string of spaces that looks like a real but blank menu row.
        assert!(text("", 80).is_empty());
        assert!(text(" \n\t ", 80).is_empty());
    }
}
