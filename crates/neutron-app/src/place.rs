//! What to call the location a pane is showing, when an operation cannot act
//! on it.
//!
//! # Why this is not just a match in `app.rs`
//!
//! Four kinds of location can be open in a pane, and only one of them —
//! [`NodeId::Path`] — has a filesystem path. Every operation that writes files
//! reaches for that path, and for the other three it finds nothing.
//!
//! What happened before was that the *shell* namespace (This PC, Control
//! Panel, the inside of a zip) and *cloud* locations returned early and did
//! nothing at all, while a server got a sentence. Pressing `F2` in a zip and
//! pressing `F2` on a server are the same situation and produced opposite
//! feedback, and the silent one reads as broken key handling rather than as a
//! missing feature — which is the exact failure `notice.rs` was written to end.
//!
//! # Unsupported and not-yet are different answers
//!
//! A server and a Drive folder are places Neutron *will* be able to write to;
//! the code is missing, so the sentence ends in "yet" and promises something.
//!
//! Control Panel is not. There is no folder to create in it and no file to
//! rename, and "yet" would be a promise about something that is not going to
//! happen. So the shell namespace gets a flat statement instead.

use neutron_core::namespace::NodeId;
use neutron_remote::Sites;

/// A location with no filesystem path, named for a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pathless {
    /// What to call it: "Google Drive", "This PC", the site's own display name.
    pub name: String,
    /// Whether Neutron might do this here one day. Decides the trailing "yet".
    pub later: bool,
}

/// Describes a location that has no path, or `None` when it has one.
///
/// `sites` resolves a server's id to the name the user gave it, so the sentence
/// says "files.example.com" rather than an internal id — the id is a key, and
/// the sidebar never shows it.
pub fn pathless(id: &NodeId, sites: &Sites) -> Option<Pathless> {
    match id {
        NodeId::Path(_) => None,
        // The dashboard has no files in it at all, so an operation that wants
        // one is not "not yet" — it is a category error, and the sentence says
        // so without promising anything.
        NodeId::Home => Some(Pathless {
            name: "the dashboard".to_owned(),
            later: false,
        }),
        NodeId::Remote { site, .. } => Some(Pathless {
            // A site that is not in the list is one whose entry was removed
            // while a tab still showed it. Naming it "that server" is better
            // than leaking the id, and the sentence still makes sense.
            name: sites
                .get(site)
                .map(|s| s.display().to_owned())
                .unwrap_or_else(|| "that server".to_owned()),
            later: true,
        }),
        NodeId::Cloud { .. } => Some(Pathless {
            name: id.display_name(),
            later: true,
        }),
        NodeId::Shell { display, .. } => Some(Pathless {
            name: display.to_string(),
            later: false,
        }),
        // Its files are all over the disk; what needs one folder cannot be
        // done here.
        NodeId::Tag { name } => Some(Pathless {
            name: format!("the files tagged {name}"),
            later: false,
        }),
        // Read-only by design: browsed and copied out of, never changed.
        NodeId::Archive { archive, .. } => Some(Pathless {
            name: archive
                .file_name()
                .map(|n| format!("{}, which is read-only", n.to_string_lossy()))
                .unwrap_or_else(|| "an archive".to_owned()),
            later: false,
        }),
    }
}

/// The sentence shown when an operation cannot run where the pane is pointing.
///
/// `what` is a predicate that takes the place as its object and carries its own
/// preposition — "delete from", "paste into", "rename anything in" — so that
/// one phrasing works for every operation and every location.
pub fn cannot(what: &str, place: &Pathless) -> String {
    let yet = if place.later { " yet" } else { "" };
    format!("Neutron cannot {what} {}{yet}", place.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use neutron_remote::site::{Protocol, Site};

    fn sites() -> Sites {
        Sites {
            sites: vec![Site {
                id: "prod".to_owned(),
                label: "files.example.com".to_owned(),
                protocol: Protocol::Sftp,
                host: "files.example.com".to_owned(),
                port: None,
                username: "razeen".to_owned(),
                path: None,
                fingerprint: None,
                auth: Default::default(),
                trusted_cert: None,
            }],
        }
    }

    #[test]
    fn a_real_folder_is_not_pathless() {
        assert_eq!(pathless(&NodeId::path("C:\notes"), &sites()), None);
    }

    #[test]
    fn a_server_is_named_the_way_the_sidebar_names_it() {
        let place = pathless(&NodeId::remote("prod", "/pub"), &sites()).expect("pathless");
        assert_eq!(place.name, "files.example.com");
        assert!(place.later);
    }

    /// A tab can outlive its entry in `sites.json`, which is edited by hand.
    /// The id is an internal key and must not reach the status bar.
    #[test]
    fn a_server_that_is_no_longer_saved_is_not_named_by_its_id() {
        let place = pathless(&NodeId::remote("deleted", "/"), &sites()).expect("pathless");
        assert_eq!(place.name, "that server");
        assert!(place.later);
    }

    #[test]
    fn drive_is_named_and_promises_later() {
        let id = NodeId::cloud(
            neutron_core::namespace::CloudProviderId::GoogleDrive,
            "root",
            "Google Drive",
        );
        let place = pathless(&id, &sites()).expect("pathless");
        assert_eq!(place.name, "Google Drive");
        assert!(place.later);
    }

    /// A folder inside Drive is named for itself. Before the id carried a
    /// name, every one of them reported as "Google Drive".
    #[test]
    fn a_folder_inside_drive_is_named_for_itself() {
        let id = NodeId::cloud(
            neutron_core::namespace::CloudProviderId::GoogleDrive,
            "1a2b3c",
            "Invoices",
        );
        let place = pathless(&id, &sites()).expect("pathless");
        assert_eq!(
            cannot("delete from", &place),
            "Neutron cannot delete from Invoices yet"
        );
    }

    /// The shell namespace is the case where "yet" would be a promise about
    /// something that is not coming: there is no folder to create in Control
    /// Panel.
    #[test]
    fn a_shell_location_does_not_promise_later() {
        let id = NodeId::shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "This PC");
        let place = pathless(&id, &sites()).expect("pathless");
        assert_eq!(place.name, "This PC");
        assert!(!place.later);
    }

    #[test]
    fn the_sentence_reads_for_every_operation() {
        let server = pathless(&NodeId::remote("prod", "/"), &sites()).expect("pathless");
        let this_pc = pathless(
            &NodeId::shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "This PC"),
            &sites(),
        )
        .expect("pathless");

        assert_eq!(
            cannot("delete from", &server),
            "Neutron cannot delete from files.example.com yet"
        );
        assert_eq!(
            cannot("rename anything in", &this_pc),
            "Neutron cannot rename anything in This PC"
        );
        assert_eq!(
            cannot("paste into", &server),
            "Neutron cannot paste into files.example.com yet"
        );
    }
}
