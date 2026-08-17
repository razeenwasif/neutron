//! Launching files and folders with their registered handler.
//!
//! `ShellExecuteExW` rather than `ShellExecuteW`: the Ex form takes a struct, so
//! it can carry `SEE_MASK_FLAG_NO_UI` and — importantly — an explicit verb and
//! an owner window. The plain form cannot suppress the shell's own error
//! dialogs, and a modal box appearing on a worker thread is worse than useless,
//! since it has no message pump to dismiss it with.
//!
//! # Threading
//!
//! **STA pool only.** Launching a file runs the registered handler's
//! association lookup, which touches the registry, may load a shell extension,
//! and for a network or cloud path can block for many seconds. It is also why
//! this returns nothing useful beyond success: the process being launched is
//! not ours to wait for.

use std::path::Path;

use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

/// What to do with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// The item's default action — run an executable, open a document in its
    /// registered application, open a folder in Explorer.
    Open,
    /// Explicit "open with the associated editor", where one is registered.
    Edit,
    /// The shell properties dialog.
    Properties,
    /// Run elevated. Triggers a UAC prompt.
    RunAs,
}

impl Verb {
    /// The verb string, or `None` for the default action.
    ///
    /// `Open` deliberately passes no verb rather than the literal `"open"`.
    /// They are not the same: a file type whose default is `edit` or `play`
    /// would be launched wrongly by forcing `open`, and some types register no
    /// `open` verb at all, which fails outright.
    fn as_pcwstr(self) -> Option<&'static [u16]> {
        // Static NUL-terminated UTF-16 literals.
        const EDIT: &[u16] = &[b'e' as u16, b'd' as u16, b'i' as u16, b't' as u16, 0];
        const PROPERTIES: &[u16] = &[
            b'p' as u16,
            b'r' as u16,
            b'o' as u16,
            b'p' as u16,
            b'e' as u16,
            b'r' as u16,
            b't' as u16,
            b'i' as u16,
            b'e' as u16,
            b's' as u16,
            0,
        ];
        const RUNAS: &[u16] = &[
            b'r' as u16,
            b'u' as u16,
            b'n' as u16,
            b'a' as u16,
            b's' as u16,
            0,
        ];

        match self {
            Verb::Open => None,
            Verb::Edit => Some(EDIT),
            Verb::Properties => Some(PROPERTIES),
            Verb::RunAs => Some(RUNAS),
        }
    }

    /// Whether the shell needs to show UI of its own for this verb.
    ///
    /// Properties *is* a dialog, so suppressing UI would make it a silent
    /// no-op; the elevation consent dialog is likewise the whole point of
    /// `runas`.
    fn needs_ui(self) -> bool {
        matches!(self, Verb::Properties | Verb::RunAs)
    }
}

/// Launches `path` with `verb`.
///
/// **Worker thread only.** `owner` is the main window handle, used to parent
/// any dialog the shell does show; passing 0 leaves such a dialog ownerless and
/// able to appear behind the main window.
pub fn shell_execute(path: &Path, verb: Verb, owner: isize) -> Result<(), String> {
    shell_execute_with_args(path, &[], verb, owner)
}

/// As [`shell_execute`], with command-line arguments.
///
/// Arguments are joined with spaces and each is quoted, which is the best that
/// can be done through this API: `ShellExecuteExW` takes one command-line
/// string and leaves splitting it to the target program's own parser. Callers
/// passing user-controlled data must keep that in mind — everything Neutron
/// passes here is its own.
pub fn shell_execute_with_args(
    path: &Path,
    args: &[String],
    verb: Verb,
    owner: isize,
) -> Result<(), String> {
    // Deliberately *not* `\\?\`-prefixed. That prefix disables Win32 path
    // parsing, which is what makes it fast for enumeration — but the shell's
    // association machinery does not understand it and fails on paths carrying
    // it. Long paths are the cost; correctness for ordinary ones is the trade.
    let wide = to_wide(path);
    let verb_ptr = verb
        .as_pcwstr()
        .map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));

    let joined = quote_args(args);
    let args_wide: Vec<u16> = joined.encode_utf16().chain(std::iter::once(0)).collect();
    let args_ptr = if args.is_empty() {
        PCWSTR::null()
    } else {
        PCWSTR(args_wide.as_ptr())
    };

    let mut mask = SEE_MASK_NOASYNC;
    if !verb.needs_ui() {
        mask |= SEE_MASK_FLAG_NO_UI;
    }

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: mask,
        hwnd: windows::Win32::Foundation::HWND(owner as *mut _),
        lpVerb: verb_ptr,
        lpFile: PCWSTR(wide.as_ptr()),
        lpParameters: args_ptr,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };

    // SAFETY: `info` is fully initialised with its own size, and every pointer
    // in it addresses a buffer that outlives the call. SEE_MASK_NOASYNC keeps
    // the shell from returning before it has finished with those buffers, which
    // is required whenever the caller may exit or free them promptly — the
    // documented cause of launches that silently do nothing.
    let ok = unsafe { ShellExecuteExW(&mut info) };

    ok.map_err(|e| {
        tracing::warn!(path = %path.display(), ?verb, error = %e, "shell execute failed");
        e.message()
    })
}

/// Joins arguments into one command line, quoting each.
///
/// Embedded quotes are escaped rather than dropped: a silently mangled argument
/// is far harder to diagnose than a rejected one.
fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| format!("\"{}\"", a.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_action_passes_no_verb() {
        // Forcing "open" launches file types whose default is `edit` or `play`
        // with the wrong handler, and fails outright on types that register no
        // `open` verb.
        assert!(Verb::Open.as_pcwstr().is_none());
    }

    #[test]
    fn verb_strings_are_nul_terminated() {
        // They are handed to Win32 as raw pointers; a missing terminator reads
        // off the end of the slice.
        for verb in [Verb::Edit, Verb::Properties, Verb::RunAs] {
            let s = verb.as_pcwstr().expect("a named verb");
            assert_eq!(*s.last().unwrap(), 0, "{verb:?} is not terminated");
            assert!(s.len() > 1);
        }
    }

    #[test]
    fn dialog_verbs_keep_their_ui() {
        // Suppressing UI for Properties makes it a silent no-op, and for RunAs
        // it would suppress the consent prompt the verb exists to raise.
        assert!(Verb::Properties.needs_ui());
        assert!(Verb::RunAs.needs_ui());
        assert!(!Verb::Open.needs_ui());
    }

    #[test]
    fn arguments_are_quoted_individually() {
        // One command-line string is all the API takes, so a path with a space
        // must not split into two arguments at the far end.
        assert_eq!(
            quote_args(&["--serve".into(), "my pipe".into()]),
            "\"--serve\" \"my pipe\""
        );
        assert_eq!(quote_args(&[]), "");
    }

    #[test]
    fn embedded_quotes_are_escaped_rather_than_dropped() {
        assert_eq!(quote_args(&["a\"b".into()]), "\"a\\\"b\"");
    }

    #[test]
    fn paths_are_widened_with_a_terminator() {
        let w = to_wide(Path::new(r"C:\x"));
        assert_eq!(
            w,
            vec![b'C' as u16, b':' as u16, b'\\' as u16, b'x' as u16, 0]
        );
    }

    #[test]
    fn non_ascii_paths_survive_widening() {
        // UTF-16 round-tripping is where filename handling usually breaks.
        let w = to_wide(Path::new("C:\\日本\\🦀.txt"));
        let back = String::from_utf16(&w[..w.len() - 1]).expect("valid UTF-16");
        assert_eq!(back, "C:\\日本\\🦀.txt");
    }
}
