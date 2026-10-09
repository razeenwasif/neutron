//! The "Connect to a server" dialog.
//!
//! A server can be added by hand in `sites.json`, and for anything scripted
//! that is the better route. This exists because the password cannot be: it
//! belongs in Credential Manager, not in a file, so *something* has to accept
//! it as typed input. Once there is a password field, asking for the host
//! beside it is free.
//!
//! The same dialog edits a saved server ([`ConnectDialog::edit`]). That is
//! where a missing password gets typed in, and where an FTPS certificate gets
//! trusted, so it has to write back to the *same* site: a new id would leave
//! the old entry behind, still without its password, beside a duplicate.
//!
//! # What it does on submit
//!
//! Three things, in an order chosen so a failure leaves nothing half-done:
//! validate the fields, merge the new site into `sites.json` (re-reading it
//! first, so a host key recorded by the loader thread since startup is not
//! clobbered), then store the password. A password saved against a site that
//! failed to save would be an orphan in the user's credential list.

use std::cell::Cell;

use egui::{Align2, Color32, Rect, Sense, Ui, pos2, vec2};
use neutron_remote::site::{Auth, Protocol, Site, Sites};
use neutron_ui::theme::{Palette, RADIUS_CARD, RADIUS_CONTROL};

/// One editable field. An enum rather than six named `String`s so the draw
/// loop, the focus order and the change action are all one thing each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Label,
    Host,
    Port,
    Username,
    Password,
    /// The one row whose meaning follows the protocol: the private key file
    /// for SFTP, the certificate fingerprint to trust for FTPS. One row rather
    /// than two because no site ever needs both, and a dialog that grows a row
    /// per protocol is a dialog mostly full of rows that do not apply. The two
    /// values are still kept apart, so switching protocol and back loses
    /// neither.
    KeyOrCertificate,
    Path,
}

/// Every field, in the order they are drawn and tabbed through.
pub const FIELDS: &[Field] = &[
    Field::Label,
    Field::Host,
    Field::Port,
    Field::Username,
    Field::Password,
    Field::KeyOrCertificate,
    Field::Path,
];

/// Where the FTPS certificate is kept, past the end of the per-field slots, so
/// it and the SFTP key path never overwrite each other.
const CERTIFICATE_SLOT: usize = 7;

/// How an SFTP site signs in, as the dialog offers it. Separate from
/// [`Auth`] because the key path lives in a text field until submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthChoice {
    #[default]
    Password,
    Key,
    Agent,
}

impl AuthChoice {
    fn label(self) -> &'static str {
        match self {
            AuthChoice::Password => "Password",
            AuthChoice::Key => "Key file",
            AuthChoice::Agent => "ssh-agent",
        }
    }

    fn of(auth: &Auth) -> AuthChoice {
        match auth {
            Auth::Password => AuthChoice::Password,
            Auth::Key { .. } => AuthChoice::Key,
            Auth::Agent => AuthChoice::Agent,
        }
    }
}

impl Field {
    fn index(self) -> usize {
        FIELDS.iter().position(|&f| f == self).unwrap_or(0)
    }

    fn label(self) -> &'static str {
        match self {
            Field::Label => "Name",
            Field::Host => "Host",
            Field::Port => "Port",
            Field::Username => "Username",
            Field::Password => "Password",
            Field::KeyOrCertificate => "Key file",
            Field::Path => "Start in",
        }
    }

    /// Shown in the empty field. These carry the defaults, so the dialog
    /// answers "what happens if I leave this blank?" without a help text.
    fn hint(self, protocol: Protocol) -> String {
        match self {
            Field::Label => "the host, if you leave this blank".to_owned(),
            Field::Host => "ftp.example.com".to_owned(),
            Field::Port => protocol.default_port().to_string(),
            Field::Username => match protocol {
                Protocol::Sftp => "required".to_owned(),
                _ => "anonymous".to_owned(),
            },
            Field::Password => match protocol {
                Protocol::Sftp => "required".to_owned(),
                _ => "none, for anonymous".to_owned(),
            },
            Field::KeyOrCertificate => match protocol {
                Protocol::Sftp => "only for key-file sign-in".to_owned(),
                Protocol::Ftps => "a fingerprint, to trust a self-signed certificate".to_owned(),
                Protocol::Ftp => "not used with plain FTP".to_owned(),
            },
            Field::Path => "/".to_owned(),
        }
    }

    fn is_secret(self) -> bool {
        matches!(self, Field::Password)
    }
}

/// State of the dialog, owned by the app.
pub struct ConnectDialog {
    pub open: bool,
    pub protocol: Protocol,
    values: [String; 8],
    /// How an SFTP site signs in.
    ///
    /// A `Cell` because the chips that change it are drawn by [`show`], which
    /// takes the dialog by shared reference like everything else drawn here.
    /// Routing it through a [`ConnectAction`] would be the usual shape, but
    /// this is state only the dialog reads, and an action would add a variant
    /// every caller has to forward for no decision of theirs.
    auth: Cell<AuthChoice>,
    /// The saved site being edited, when this is not a new one. Its id is
    /// what the result is saved under, and its host key is kept unless the
    /// edit points the site at a different server.
    editing: Option<Site>,
    /// Why the last submit did not go through.
    pub error: Option<String>,
}

impl Default for ConnectDialog {
    fn default() -> Self {
        ConnectDialog {
            open: false,
            // The safe one of the three, so a user who does not change it does
            // not send a password across the network in the clear.
            protocol: Protocol::Sftp,
            values: Default::default(),
            auth: Cell::new(AuthChoice::Password),
            editing: None,
            error: None,
        }
    }
}

impl ConnectDialog {
    /// Which stored string a field shows. Only the shared row has a choice to
    /// make.
    fn slot(&self, field: Field) -> usize {
        match field {
            Field::KeyOrCertificate if self.protocol == Protocol::Ftps => CERTIFICATE_SLOT,
            _ => field.index(),
        }
    }

    pub fn value(&self, field: Field) -> &str {
        &self.values[self.slot(field)]
    }

    pub fn set(&mut self, field: Field, text: String) {
        let slot = self.slot(field);
        self.values[slot] = text;
        // Any edit clears the complaint about the previous attempt; leaving it
        // up makes a fixed field look still-broken.
        self.error = None;
    }

    pub fn auth(&self) -> AuthChoice {
        self.auth.get()
    }

    #[cfg(test)]
    pub fn set_auth(&mut self, choice: AuthChoice) {
        self.auth.set(choice);
        self.error = None;
    }

    pub fn open(&mut self) {
        *self = ConnectDialog {
            open: true,
            ..ConnectDialog::default()
        };
    }

    /// Opens the dialog on a saved site, to change it in place.
    ///
    /// Everything but the password is filled in. The password field starts
    /// empty and is left alone on submit if it stays empty, so editing a
    /// server's name does not mean retyping its password — and typing one in
    /// is how a site with none saved gets one.
    pub fn edit(&mut self, site: &Site) {
        self.open();
        self.protocol = site.protocol;
        self.auth.set(AuthChoice::of(&site.auth));
        self.values[Field::Label.index()] = site.label.clone();
        self.values[Field::Host.index()] = site.host.clone();
        self.values[Field::Port.index()] = site.port.map(|p| p.to_string()).unwrap_or_default();
        self.values[Field::Username.index()] = site.username.clone();
        if let Auth::Key { path } = &site.auth {
            self.values[Field::KeyOrCertificate.index()] = path.clone();
        }
        self.values[CERTIFICATE_SLOT] = site.trusted_cert.clone().unwrap_or_default();
        // The root is what an empty field already means, and the hint says so.
        self.values[Field::Path.index()] = site
            .path
            .clone()
            .filter(|p| p != "/")
            .unwrap_or_default();
        self.editing = Some(site.clone());
    }

    /// Fills in an FTPS certificate fingerprint to trust — for the app to call
    /// after [`ConnectDialog::edit`] when a connection was refused with
    /// [`neutron_remote::RemoteError::UntrustedCertificate`], so the user only
    /// has to confirm it.
    pub fn trust_certificate(&mut self, fingerprint: &str) {
        self.values[CERTIFICATE_SLOT] = fingerprint.to_owned();
    }

    /// The id of the site being edited, or `None` for a new one.
    #[cfg(test)]
    pub fn editing(&self) -> Option<&str> {
        self.editing.as_ref().map(|s| s.id.as_str())
    }

    pub fn close(&mut self) {
        self.open = false;
        self.editing = None;
        // The password does not linger in memory after the dialog closes.
        for value in &mut self.values {
            value.clear();
        }
    }

    /// What a field is called, given the protocol and sign-in method.
    fn label(&self, field: Field) -> &'static str {
        match (field, self.protocol) {
            (Field::Password, Protocol::Sftp) if self.auth() == AuthChoice::Key => "Passphrase",
            (Field::KeyOrCertificate, Protocol::Ftps) => "Certificate",
            (Field::KeyOrCertificate, Protocol::Ftp) => "",
            _ => field.label(),
        }
    }

    /// The hint drawn in an empty field, knowing everything [`hint`] does not:
    /// the sign-in method, and whether a password is already saved.
    pub fn hint(&self, field: Field) -> String {
        let sftp = self.protocol == Protocol::Sftp;
        match field {
            Field::Password if sftp && self.auth() == AuthChoice::Agent => {
                "not needed with ssh-agent".to_owned()
            }
            Field::Password if self.editing.is_some() => {
                "leave blank to keep the saved one".to_owned()
            }
            Field::Password if sftp && self.auth() == AuthChoice::Key => {
                "only if the key is encrypted".to_owned()
            }
            Field::KeyOrCertificate if sftp && self.auth() == AuthChoice::Key => {
                "~/.ssh/id_ed25519".to_owned()
            }
            Field::KeyOrCertificate if self.protocol == Protocol::Ftps => {
                "SHA-256 fingerprint, to trust a self-signed certificate".to_owned()
            }
            _ => field.hint(self.protocol),
        }
    }

    /// The site the fields describe, or why they do not describe one.
    ///
    /// `existing` is what is already saved, so the generated id can avoid
    /// colliding with one.
    pub fn to_site(&self, existing: &Sites) -> Result<Site, String> {
        let host = self.value(Field::Host).trim();
        if host.is_empty() {
            return Err("A host is needed — a name or an address".to_owned());
        }

        let port = match self.value(Field::Port).trim() {
            "" => None,
            text => match text.parse::<u16>() {
                Ok(0) | Err(_) => {
                    return Err(format!("{text:?} is not a port number"));
                }
                Ok(port) => Some(port),
            },
        };

        let username = self.value(Field::Username).trim();
        if self.protocol == Protocol::Sftp && username.is_empty() {
            // Saying so here is better than a failed login: SSH has no
            // anonymous mode, so this can never work.
            return Err("SFTP needs a username — it has no anonymous mode".to_owned());
        }

        // FTP has only passwords, so whatever was chosen while SFTP was
        // selected does not follow the site to another protocol.
        let auth = match (self.protocol, self.auth()) {
            (Protocol::Sftp, AuthChoice::Key) => {
                let path = self.values[Field::KeyOrCertificate.index()].trim();
                if path.is_empty() {
                    return Err("Key-file sign-in needs the path to a private key".to_owned());
                }
                Auth::Key { path: path.to_owned() }
            }
            (Protocol::Sftp, AuthChoice::Agent) => Auth::Agent,
            _ => Auth::Password,
        };

        let trusted_cert = match self.protocol {
            Protocol::Ftps => Some(self.values[CERTIFICATE_SLOT].trim().to_owned())
                .filter(|text| !text.is_empty()),
            _ => None,
        };

        let label = self.value(Field::Label).trim();
        let id = match &self.editing {
            Some(original) => original.id.clone(),
            None => unique_id(if label.is_empty() { host } else { label }, existing),
        };
        let mut site = Site {
            id,
            label: label.to_owned(),
            protocol: self.protocol,
            host: host.to_owned(),
            port,
            username: username.to_owned(),
            path: Some(self.value(Field::Path).trim().to_owned()).filter(|p| !p.is_empty()),
            fingerprint: None,
            auth,
            trusted_cert,
        };

        // The recorded host key belongs to a server, not to a site entry. An
        // edit that keeps pointing at the same one keeps it — dropping it would
        // turn the next connection into a first contact that accepts whatever
        // key is presented. An edit that points somewhere else drops it, or
        // the new server would be refused as an impostor.
        if let Some(original) = &self.editing {
            if same_server(original, &site) {
                site.fingerprint = original.fingerprint.clone();
            }
        }

        neutron_remote::site::validate(&mut site).map_err(|e| e.to_string())?;
        Ok(site)
    }

    pub fn password(&self) -> &str {
        self.value(Field::Password)
    }
}

/// Turns a name into a site id that is not already taken.
///
/// Ids are how a saved tab and a stored password find their site again, so they
/// have to be stable and unique. Deriving one from the label means the user
/// never has to think about it, and the numeric suffix means two servers called
/// "backup" both work.
pub fn unique_id(from: &str, existing: &Sites) -> String {
    let base: String = from
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse the runs a substitution leaves behind, and trim the edges, so
    // "My NAS (home)" becomes "my-nas-home" rather than "my-nas--home-".
    let mut base = base
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    base.truncate(32);
    if base.is_empty() {
        base = "server".to_owned();
    }

    if existing.get(&base).is_none() {
        return base;
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if existing.get(&candidate).is_none() {
            return candidate;
        }
    }
    base
}

/// Whether two versions of a site point at the same server, which is what an
/// SSH host key is a fact about.
fn same_server(a: &Site, b: &Site) -> bool {
    a.protocol == b.protocol && a.host.eq_ignore_ascii_case(b.host.trim()) && a.port() == b.port()
}

/// Saves a site and its password.
///
/// Re-reads `sites.json` before writing rather than saving a copy held in
/// memory: the loader thread records SSH host keys into that same file, and
/// writing a stale copy over the top would silently un-learn one.
///
/// An empty password leaves whatever is saved alone. For a new site that means
/// none (anonymous FTP, an agent, an unencrypted key); for an edited one it
/// means the one already there.
pub fn save(site: &Site, password: &str) -> Result<(), String> {
    let mut sites = neutron_remote::site::load()?;
    merge(&mut sites, site.clone());
    neutron_remote::site::save(&sites)?;

    // After the site, so a failure here leaves a saved server the user can
    // retry rather than a password belonging to nothing.
    if !password.is_empty() {
        neutron_remote::remember_password(site, password)?;
    }
    Ok(())
}

/// Puts `site` into `sites`: in place of the entry with its id, or at the end.
///
/// In place, so editing a server does not move it to the bottom of the
/// sidebar. And a host key the loader thread recorded while the dialog was
/// open is kept when the edit still points at the same server — the dialog's
/// copy of the site predates it.
fn merge(sites: &mut Sites, mut site: Site) {
    match sites.sites.iter_mut().find(|s| s.id == site.id) {
        Some(existing) => {
            if site.fingerprint.is_none() && same_server(existing, &site) {
                site.fingerprint = existing.fingerprint.clone();
            }
            *existing = site;
        }
        None => sites.sites.push(site),
    }
}

/// What the dialog is asking the app to do.
///
/// No field-edit variant: the text fields are drawn by the app, because a
/// `TextEdit` needs `&mut String` and every drawing function here takes `&self`.
/// It reports its own edits.
pub enum ConnectAction {
    Close,
    Submit,
    SetProtocol(Protocol),
}

const WIDTH: f32 = 460.0;
const ROW_H: f32 = 46.0;
const TOP: f32 = 96.0;
/// Where the first field row starts inside the card: below the title, the
/// protocol chips, and the row that holds the sign-in method.
const FIELDS_TOP: f32 = 124.0;

/// The dialog's outer rectangle.
pub fn card_rect(screen: Rect) -> Rect {
    let height = FIELDS_TOP + FIELDS.len() as f32 * ROW_H + 64.0;
    Rect::from_min_size(
        pos2(screen.center().x - WIDTH / 2.0, screen.top() + TOP),
        vec2(WIDTH, height.min(screen.height() - TOP - 24.0)),
    )
}

/// Where the app draws the text field for `field`.
pub fn field_rect(screen: Rect, field: Field) -> Rect {
    let card = card_rect(screen);
    let top = card.top() + FIELDS_TOP + field.index() as f32 * ROW_H;
    Rect::from_min_size(
        pos2(card.left() + 130.0, top + 4.0),
        vec2(card.width() - 154.0, 26.0),
    )
}

/// Draws everything except the text fields themselves, which the app owns
/// because a `TextEdit` needs `&mut String`.
pub fn show(ui: &mut Ui, p: &Palette, dialog: &ConnectDialog) -> Option<ConnectAction> {
    let screen = ui.ctx().content_rect();
    let card = card_rect(screen);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("neutron-connect"),
    ));

    // Everything behind is dimmed and unclickable while this is up: it is a
    // question, and answering it somewhere else would leave the dialog stale.
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(120));
    let dismiss = ui.interact(
        screen,
        egui::Id::new("neutron-connect-dismiss"),
        Sense::click(),
    );

    painter.rect_filled(card, RADIUS_CARD, p.card);
    painter.rect_stroke(
        card,
        RADIUS_CARD,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );

    painter.text(
        pos2(card.left() + 22.0, card.top() + 26.0),
        Align2::LEFT_CENTER,
        if dialog.editing.is_some() {
            "Edit server"
        } else {
            "Connect to a server"
        },
        egui::FontId::proportional(17.0),
        p.text,
    );

    let mut action = None;

    // Protocol, as three buttons rather than a dropdown: there are exactly
    // three, and which one is chosen changes what the fields below mean.
    let mut x = card.left() + 22.0;
    for protocol in [Protocol::Sftp, Protocol::Ftps, Protocol::Ftp] {
        let rect = Rect::from_min_size(pos2(x, card.top() + 48.0), vec2(74.0, 26.0));
        let id = egui::Id::new(("neutron-connect-protocol", protocol.label()));
        if chip(ui, &painter, p, rect, id, protocol.label(), dialog.protocol == protocol) {
            action = Some(ConnectAction::SetProtocol(protocol));
        }
        x += 80.0;
    }

    // The second row says how this protocol proves who is on each end: how
    // SFTP signs in, or how FTPS decides to trust the server.
    let row_y = card.top() + 84.0;
    match dialog.protocol {
        Protocol::Sftp => {
            painter.text(
                pos2(card.left() + 22.0, row_y + 13.0),
                Align2::LEFT_CENTER,
                "Sign in with",
                egui::FontId::proportional(13.0),
                p.text_muted,
            );
            let mut x = card.left() + 130.0;
            for choice in [AuthChoice::Password, AuthChoice::Key, AuthChoice::Agent] {
                let rect = Rect::from_min_size(pos2(x, row_y), vec2(90.0, 26.0));
                let id = egui::Id::new(("neutron-connect-auth", choice.label()));
                // Set directly rather than reported: see `ConnectDialog::auth`.
                if chip(ui, &painter, p, rect, id, choice.label(), dialog.auth() == choice) {
                    dialog.auth.set(choice);
                }
                x += 96.0;
            }
        }
        Protocol::Ftps => {
            painter.text(
                pos2(card.left() + 22.0, row_y + 13.0),
                Align2::LEFT_CENTER,
                "A self-signed certificate is refused until its fingerprint is trusted below",
                egui::FontId::proportional(11.5),
                p.text_faint,
            );
        }
        Protocol::Ftp => {}
    }

    // The one honest warning this dialog owes the user.
    if dialog.protocol.is_cleartext() {
        painter.text(
            pos2(card.right() - 22.0, card.top() + 61.0),
            Align2::RIGHT_CENTER,
            "password sent in the clear",
            egui::FontId::proportional(11.0),
            p.warning,
        );
    }

    for &field in FIELDS {
        let rect = field_rect(screen, field);
        painter.text(
            pos2(card.left() + 22.0, rect.center().y),
            Align2::LEFT_CENTER,
            dialog.label(field),
            egui::FontId::proportional(13.0),
            p.text_muted,
        );
        painter.rect_filled(rect.expand2(vec2(6.0, 4.0)), RADIUS_CONTROL, p.inset);
    }

    let buttons_y = card.bottom() - 34.0;
    let connect = Rect::from_min_size(pos2(card.right() - 118.0, buttons_y), vec2(96.0, 30.0));
    let cancel = Rect::from_min_size(pos2(card.right() - 224.0, buttons_y), vec2(96.0, 30.0));

    for (rect, text, primary) in [(connect, "Connect", true), (cancel, "Cancel", false)] {
        let response = ui.interact(
            rect,
            egui::Id::new(("neutron-connect-button", text)),
            Sense::click(),
        );
        painter.rect_filled(
            rect,
            RADIUS_CONTROL,
            if primary {
                if response.hovered() { p.accent } else { p.selection }
            } else if response.hovered() {
                p.hover
            } else {
                Color32::TRANSPARENT
            },
        );
        painter.rect_stroke(
            rect,
            RADIUS_CONTROL,
            egui::Stroke::new(1.0, if primary { p.accent } else { p.border }),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(13.0),
            if primary && response.hovered() {
                p.ground
            } else if primary {
                p.accent
            } else {
                p.text_muted
            },
        );
        if response.clicked() {
            action = Some(if primary {
                ConnectAction::Submit
            } else {
                ConnectAction::Close
            });
        }
    }

    if let Some(error) = &dialog.error {
        painter.text(
            pos2(card.left() + 22.0, buttons_y + 15.0),
            Align2::LEFT_CENTER,
            error,
            egui::FontId::proportional(11.5),
            p.danger,
        );
    }

    // A click on the dimmed background closes, which is what every dialog does
    // and what a user tries first.
    if action.is_none() && dismiss.clicked() && !card.contains(dismiss.interact_pointer_pos()?) {
        action = Some(ConnectAction::Close);
    }

    action
}

/// One toggle button. Returns whether it was clicked.
fn chip(
    ui: &mut Ui,
    painter: &egui::Painter,
    p: &Palette,
    rect: Rect,
    id: egui::Id,
    text: &str,
    chosen: bool,
) -> bool {
    let response = ui.interact(rect, id, Sense::click());
    painter.rect_filled(
        rect,
        RADIUS_CONTROL,
        if chosen {
            p.selection
        } else if response.hovered() {
            p.hover
        } else {
            Color32::TRANSPARENT
        },
    );
    painter.rect_stroke(
        rect,
        RADIUS_CONTROL,
        egui::Stroke::new(1.0, if chosen { p.accent } else { p.border }),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        if chosen { p.accent } else { p.text_muted },
    );
    response.clicked()
}

/// The hint drawn in an empty field, from the protocol alone.
/// [`ConnectDialog::hint`] knows more and is the better one to draw.
/// Whether the field holds a secret and should be masked.
pub fn is_secret(field: Field) -> bool {
    field.is_secret()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(host: &str, protocol: Protocol) -> ConnectDialog {
        let mut d = ConnectDialog {
            open: true,
            protocol,
            ..ConnectDialog::default()
        };
        d.set(Field::Host, host.to_owned());
        d.set(Field::Username, "razeen".to_owned());
        d
    }

    #[test]
    fn a_filled_dialog_describes_a_site() {
        let site = dialog("nas.local", Protocol::Sftp)
            .to_site(&Sites::default())
            .expect("should be valid");
        assert_eq!(site.host, "nas.local");
        assert_eq!(site.protocol, Protocol::Sftp);
        assert_eq!(site.port(), 22);
        assert_eq!(site.start_path(), "/");
    }

    #[test]
    fn a_missing_host_is_refused_with_a_sentence() {
        let error = ConnectDialog::default()
            .to_site(&Sites::default())
            .expect_err("should refuse");
        assert!(error.to_lowercase().contains("host"), "{error}");
    }

    #[test]
    fn sftp_without_a_username_is_refused_before_anything_is_dialled() {
        // SSH has no anonymous mode, so this can never work; saying so now is
        // better than a failed login later.
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set(Field::Username, String::new());
        let error = d.to_site(&Sites::default()).expect_err("should refuse");
        assert!(error.contains("anonymous"), "{error}");
    }

    #[test]
    fn ftp_without_a_username_is_allowed_because_anonymous_ftp_exists() {
        let mut d = dialog("ftp.example.com", Protocol::Ftp);
        d.set(Field::Username, String::new());
        assert!(d.to_site(&Sites::default()).is_ok());
    }

    #[test]
    fn a_port_that_is_not_a_number_is_refused() {
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set(Field::Port, "twenty two".to_owned());
        assert!(d.to_site(&Sites::default()).is_err());
        d.set(Field::Port, "0".to_owned());
        assert!(d.to_site(&Sites::default()).is_err());
        d.set(Field::Port, "2222".to_owned());
        assert_eq!(d.to_site(&Sites::default()).unwrap().port(), 2222);
    }

    #[test]
    fn an_empty_port_means_the_protocols_default() {
        let mut d = dialog("ftp.example.com", Protocol::Ftp);
        d.set(Field::Port, String::new());
        assert_eq!(d.to_site(&Sites::default()).unwrap().port(), 21);
    }

    #[test]
    fn an_id_is_derived_from_the_name_so_the_user_never_sees_one() {
        let id = unique_id("My NAS (home)", &Sites::default());
        assert_eq!(id, "my-nas-home");
    }

    #[test]
    fn ids_do_not_collide() {
        let mut existing = Sites::default();
        let first = unique_id("backup", &existing);
        existing.sites.push(Site {
            id: first.clone(),
            label: String::new(),
            protocol: Protocol::Ftp,
            host: "a".into(),
            port: None,
            username: String::new(),
            path: None,
            fingerprint: None,
            auth: Auth::Password,
            trusted_cert: None,
        });

        let second = unique_id("backup", &existing);
        assert_ne!(first, second);
        assert_eq!(second, "backup-2");
    }

    #[test]
    fn a_name_with_nothing_usable_in_it_still_yields_an_id() {
        // A credential target and a saved tab both key off this, so it can
        // never be empty.
        assert_eq!(unique_id("!!!", &Sites::default()), "server");
        assert_eq!(unique_id("", &Sites::default()), "server");
    }

    #[test]
    fn the_default_protocol_is_the_encrypted_one() {
        // A user who does not touch the control should not end up sending a
        // password across the network in the clear.
        assert!(!ConnectDialog::default().protocol.is_cleartext());
    }

    #[test]
    fn closing_clears_the_password() {
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set(Field::Password, "hunter2".to_owned());
        d.close();
        assert!(d.password().is_empty());
    }

    #[test]
    fn editing_a_field_clears_the_previous_complaint() {
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.error = Some("something".to_owned());
        d.set(Field::Host, "other.local".to_owned());
        assert_eq!(d.error, None);
    }

    #[test]
    fn only_the_password_field_is_masked() {
        for &field in FIELDS {
            assert_eq!(is_secret(field), field == Field::Password, "{field:?}");
        }
    }

    fn saved(id: &str) -> Site {
        Site {
            id: id.to_owned(),
            label: "Home NAS".to_owned(),
            protocol: Protocol::Sftp,
            host: "nas.local".to_owned(),
            port: Some(2222),
            username: "razeen".to_owned(),
            path: Some("/volume1".to_owned()),
            fingerprint: Some("SHA256:abc".to_owned()),
            auth: Auth::Key { path: "~/.ssh/id_ed25519".to_owned() },
            trusted_cert: None,
        }
    }

    #[test]
    fn key_file_sign_in_carries_the_path() {
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set_auth(AuthChoice::Key);
        assert!(d.to_site(&Sites::default()).is_err(), "a key login with no key");

        d.set(Field::KeyOrCertificate, " ~/.ssh/id_ed25519 ".to_owned());
        let site = d.to_site(&Sites::default()).unwrap();
        assert_eq!(site.auth, Auth::Key { path: "~/.ssh/id_ed25519".to_owned() });
    }

    #[test]
    fn agent_sign_in_needs_nothing_typed() {
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set_auth(AuthChoice::Agent);
        assert_eq!(d.to_site(&Sites::default()).unwrap().auth, Auth::Agent);
    }

    #[test]
    fn an_ssh_sign_in_method_does_not_follow_the_site_to_ftp() {
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set_auth(AuthChoice::Agent);
        d.protocol = Protocol::Ftps;
        assert_eq!(d.to_site(&Sites::default()).unwrap().auth, Auth::Password);
    }

    #[test]
    fn a_trusted_certificate_is_stored_for_ftps_only() {
        let mut d = dialog("nas.local", Protocol::Ftps);
        d.set(Field::KeyOrCertificate, format!("SHA256:{}", "0f".repeat(32)));
        let site = d.to_site(&Sites::default()).unwrap();
        assert_eq!(site.trusted_cert, Some(vec!["0F"; 32].join(":")));

        d.set(Field::KeyOrCertificate, "nonsense".to_owned());
        assert!(d.to_site(&Sites::default()).is_err());

        d.protocol = Protocol::Ftp;
        assert_eq!(d.to_site(&Sites::default()).unwrap().trusted_cert, None);
    }

    #[test]
    fn the_key_path_and_the_certificate_are_kept_apart() {
        // One row on screen, two values: switching protocol must not turn a
        // key path into a fingerprint or the other way round.
        let mut d = dialog("nas.local", Protocol::Sftp);
        d.set(Field::KeyOrCertificate, "~/.ssh/id_rsa".to_owned());
        d.protocol = Protocol::Ftps;
        assert_eq!(d.value(Field::KeyOrCertificate), "");
        d.set(Field::KeyOrCertificate, "AB".to_owned());
        d.protocol = Protocol::Sftp;
        assert_eq!(d.value(Field::KeyOrCertificate), "~/.ssh/id_rsa");
    }

    #[test]
    fn editing_saves_back_to_the_same_site() {
        // The whole point: a new id here duplicates the server, and the old
        // entry keeps its missing password.
        let original = saved("nas");
        let existing = Sites { sites: vec![original.clone()] };
        let mut d = ConnectDialog::default();
        d.edit(&original);

        assert!(d.open);
        assert_eq!(d.editing(), Some("nas"));
        assert!(d.password().is_empty());
        let site = d.to_site(&existing).unwrap();
        assert_eq!(site, original);
    }

    #[test]
    fn an_edit_that_changes_the_server_forgets_its_host_key() {
        let original = saved("nas");
        let mut d = ConnectDialog::default();
        d.edit(&original);
        d.set(Field::Label, "Renamed".to_owned());
        assert_eq!(d.to_site(&Sites::default()).unwrap().fingerprint, original.fingerprint);

        d.set(Field::Host, "other.local".to_owned());
        assert_eq!(d.to_site(&Sites::default()).unwrap().fingerprint, None);
    }

    #[test]
    fn an_untrusted_certificate_can_be_offered_for_confirmation() {
        let mut site = saved("nas");
        site.protocol = Protocol::Ftps;
        site.auth = Auth::Password;
        let mut d = ConnectDialog::default();
        d.edit(&site);
        d.trust_certificate(&"cd".repeat(32));
        assert_eq!(d.to_site(&Sites::default()).unwrap().trusted_cert, Some(vec!["CD"; 32].join(":")));
    }

    #[test]
    fn closing_forgets_which_site_was_being_edited() {
        let mut d = ConnectDialog::default();
        d.edit(&saved("nas"));
        d.close();
        assert_eq!(d.editing(), None);
        d.open();
        assert_eq!(d.editing(), None);
    }

    #[test]
    fn a_merged_edit_stays_in_place_and_keeps_a_host_key_learned_meanwhile() {
        let mut sites = Sites { sites: vec![saved("a"), saved("b"), saved("c")] };
        sites.sites[1].fingerprint = Some("SHA256:learned".to_owned());

        let mut edited = saved("b");
        edited.label = "Renamed".to_owned();
        edited.fingerprint = None;
        merge(&mut sites, edited);

        let ids: Vec<&str> = sites.sites.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(sites.sites[1].label, "Renamed");
        assert_eq!(sites.sites[1].fingerprint.as_deref(), Some("SHA256:learned"));

        merge(&mut sites, saved("d"));
        assert_eq!(sites.sites.len(), 4);
    }
}
