//! Third-party commands, run as separate processes.
//!
//! A plugin is a directory under `%APPDATA%\Neutron\plugins` containing a
//! [`Manifest`](manifest::Manifest) and a program. The manifest declares
//! commands; Neutron shows them in the command palette and, when the manifest
//! asks, in the right-click menu. Choosing one runs the program with the
//! current folder and selection on its standard input, and acts on the lines it
//! writes back.
//!
//! # Why separate processes and not a dynamic library
//!
//! Three reasons, in order of how much they cost to get wrong.
//!
//! **A plugin must not be able to take the window down.** Release builds are
//! `panic = "abort"`, so an in-process plugin that panics — or dereferences a
//! null pointer, or overflows a buffer — kills the file manager and everything
//! unsaved in it. A child process that dies is an exit status.
//!
//! **A plugin must not be able to hang the window.** The whole architecture
//! rests on the UI thread touching nothing but plain memory. An in-process
//! plugin has to be *trusted* to observe that; a child process is held to it,
//! because the host waits on a channel with a deadline and kills what does not
//! answer.
//!
//! **Rust has no stable ABI.** In-process plugins would have to speak C across
//! the boundary, which throws away every type in the codebase and means every
//! plugin recompiles when the host's compiler changes. The wire format here is
//! newline-delimited JSON, so a plugin can be written in anything that can
//! read a line and print one — including a shell script.
//!
//! The cost is process startup per invocation, a few milliseconds on Windows.
//! Commands are chosen by a human at human intervals; nothing here is on a
//! frame path.
//!
//! # What this is not
//!
//! **It is not a sandbox.** A plugin is a program the user installed, and it
//! runs with the user's own rights: it can read and write everything they can.
//! Isolating it further would mean a restricted token and a job object, which
//! is a real and separate piece of work — and one that would break the useful
//! plugins, since "act on these files" is the point.
//!
//! What the design does give is narrower and worth stating exactly:
//!
//! - **Listing a plugin never runs it.** Discovery reads manifests. Nothing is
//!   executed until a person picks a command, so an installed plugin cannot run
//!   at startup, and a malformed one cannot run at all.
//! - **Nothing appears that is not written in a file the user can read.** Every
//!   title in the palette comes from a manifest on disk, not from a program's
//!   output. A plugin cannot invent a command that looks like one of Neutron's.
//! - **The host distrusts what comes back.** A plugin's output is parsed,
//!   length-capped, and stripped of control characters before it reaches the
//!   screen, and paths are checked before anything is done with them. See
//!   [`sanitize`].
//!
//! # Layout
//!
//! ```text
//! %APPDATA%\Neutron\plugins\
//!   checksums\
//!     plugin.json      the manifest — commands, and the program to run
//!     checksums.exe    named by the manifest's `run` field
//! ```

pub mod discover;
pub mod host;
pub mod manifest;
pub mod protocol;
pub mod sanitize;

pub use discover::{Installed, LoadError, PluginSet};
pub use host::{Outcome, invoke};
pub use manifest::{CommandDef, Manifest, Needs};
pub use protocol::{PROTOCOL, Request, Response};
