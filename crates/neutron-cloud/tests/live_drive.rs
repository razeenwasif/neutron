//! Live round trip against the real Drive API.
//!
//! `#[ignore]` by default: it needs network access and a credential a CI
//! machine will not have. Run deliberately, on a machine that has signed in:
//!
//! ```text
//! cargo xtask test -p neutron-cloud -- --ignored --nocapture
//! ```
//!
//! Its value is covering the one thing no unit test can: that the token
//! exchange, the silent refresh, the field mask and the response shape all
//! still agree with what Google actually serves.

use neutron_cloud::google::{DriveState, GoogleDrive};

#[test]
#[ignore = "needs network and a stored Google credential"]
fn lists_the_drive_root() {
    let drive = GoogleDrive::new();

    match drive.state() {
        DriveState::SignedIn => {}
        other => panic!("not signed in ({other:?}) — connect Drive in Neutron first"),
    }

    let list = drive
        .list(neutron_cloud::drive::ROOT_ID)
        .expect("listing the Drive root");

    println!("root contains {} entries", list.len());
    for i in 0..list.len().min(15) {
        let (target, _) = list.target(i).unwrap_or(("", false));
        println!(
            "  {:<44} {:>12}  {}",
            list.name(i),
            list.size(i),
            &target[..target.len().min(18)]
        );
    }

    // Every Drive entry must carry a target id: children are addressed by id,
    // never by joining a name to the parent, so a missing one is a row that
    // cannot be opened.
    for i in 0..list.len() {
        let (target, is_path) = list.target(i).unwrap_or(("", true));
        assert!(!target.is_empty(), "{} has no target id", list.name(i));
        assert!(!is_path, "{} was marked as a filesystem path", list.name(i));
    }
}
