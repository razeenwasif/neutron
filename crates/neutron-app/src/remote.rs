//! Live connections to saved servers, and the listings they produce.
//!
//! One [`Sessions`] lives on the loader thread and holds one connection per
//! site. Connections are expensive to open — FTP wants a TCP connection, a
//! login and a `FEAT`; SSH wants a key exchange — and cheap to keep, so they
//! are made once and reused for every folder.
//!
//! # Downloads do not share the listing connection
//!
//! FTP cannot interleave: one control connection carries one command at a time,
//! so a listing issued during a transfer waits for the transfer. A download
//! therefore opens its own connection and closes it afterwards, which keeps
//! browsing responsive while a large file is coming down. The cost is one extra
//! login per download, once.
//!
//! # Reconnecting once, and only once
//!
//! Servers close idle connections — five minutes is a common default — and the
//! failure looks like an ordinary error on the next command. So a failed
//! operation drops the session and tries again from a fresh connection exactly
//! once. Retrying forever against a server that is rate-limiting logins is how
//! a client gets an account locked.

use std::collections::HashMap;

use neutron_core::EntryList;
use neutron_remote::{RemoteError, Sites, Transport, site};

/// Connections in use, keyed by site id.
pub struct Sessions {
    sites: Sites,
    /// Why the saved-servers file would not load, if it would not.
    pub sites_error: Option<String>,
    open: HashMap<String, Box<dyn Transport>>,
}

impl Sessions {
    /// Reads the saved servers. Connects to nothing.
    pub fn load() -> Sessions {
        let (sites, sites_error) = match site::load() {
            Ok(sites) => (sites, None),
            Err(e) => {
                tracing::warn!("sites.json: {e}");
                (Sites::default(), Some(e))
            }
        };
        Sessions {
            sites,
            sites_error,
            open: HashMap::new(),
        }
    }

    /// Re-reads the saved servers, and drops every open connection.
    ///
    /// Dropped rather than kept, because the file may have changed the host,
    /// the port or the credentials of a site that is currently connected, and a
    /// session that no longer matches its own definition is a trap.
    pub fn reload(&mut self) {
        let refreshed = Sessions::load();
        self.sites = refreshed.sites;
        self.sites_error = refreshed.sites_error;
        self.open.clear();
    }

    /// Lists a folder on a site.
    pub fn list(&mut self, site_id: &str, path: &str) -> Result<EntryList, String> {
        let mut refused = Vec::new();

        let entries = self.with_session(site_id, |transport| {
            transport.list(path, &mut refused)
        })?;

        if !refused.is_empty() {
            // Logged in full, and not shown: the status bar has one line, and
            // the entries are missing from the listing either way. What matters
            // is that they were *refused* rather than silently dropped.
            for refusal in &refused {
                tracing::warn!(site = site_id, %path, "{refusal}");
            }
        }

        let mut list = EntryList::with_capacity(entries.len());
        for entry in &entries {
            list.push(&entry.to_entry());
        }
        Ok(list)
    }

    /// Runs `work` against the site's connection, dialling or re-dialling as
    /// needed.
    fn with_session<T>(
        &mut self,
        site_id: &str,
        mut work: impl FnMut(&mut Box<dyn Transport>) -> Result<T, RemoteError>,
    ) -> Result<T, String> {
        let site = self
            .sites
            .get(site_id)
            .ok_or_else(|| format!("no saved server called {site_id:?}"))?
            .clone();

        // An existing connection first, because that is the fast path and the
        // common one.
        if let Some(transport) = self.open.get_mut(site_id) {
            match work(transport) {
                Ok(value) => return Ok(value),
                Err(e) => {
                    // Might be a dead connection, might be a real refusal.
                    // Dropping and retrying tells them apart, and costs one
                    // login in the case where it was real.
                    tracing::debug!(site = site_id, "retrying on a fresh connection: {e}");
                    self.open.remove(site_id);
                }
            }
        }

        let mut transport = neutron_remote::connect(&site).map_err(|e| e.to_string())?;

        // A first connection may have learned the server's SSH host key. Saving
        // it is what makes every connection after this one check it.
        if let Some(fingerprint) = transport.new_fingerprint() {
            self.record_fingerprint(site_id, fingerprint);
        }

        let outcome = work(&mut transport).map_err(|e| e.to_string());
        // Kept even on failure: a "no such file" does not mean the connection
        // is bad, and throwing it away would re-login on the next folder.
        self.open.insert(site_id.to_owned(), transport);
        outcome
    }

    /// Writes a newly-seen host key back to `sites.json`.
    ///
    /// Re-reads the file first rather than writing this process's copy of it:
    /// the dialog may have added a site since, and clobbering it would lose a
    /// server the user just created.
    fn record_fingerprint(&mut self, site_id: &str, fingerprint: &str) {
        if let Some(site) = self.sites.sites.iter_mut().find(|s| s.id == site_id) {
            site.fingerprint = Some(fingerprint.to_owned());
        }

        let mut on_disk = match site::load() {
            Ok(sites) => sites,
            Err(e) => {
                tracing::warn!("could not record the host key: {e}");
                return;
            }
        };
        let Some(site) = on_disk.sites.iter_mut().find(|s| s.id == site_id) else {
            return;
        };
        site.fingerprint = Some(fingerprint.to_owned());

        match site::save(&on_disk) {
            Ok(path) => tracing::info!(site = site_id, "host key recorded in {}", path.display()),
            Err(e) => tracing::warn!("could not record the host key: {e}"),
        }
    }
}

/// Downloads a remote file to `destination`, on its own connection.
///
/// A free function rather than a [`Sessions`] method because it runs on a
/// different thread — see the module docs on why a transfer must not share the
/// listing connection.
pub fn download(
    site: &neutron_remote::Site,
    path: &str,
    destination: &std::path::Path,
    on_progress: &mut dyn FnMut(u64) -> neutron_remote::Continue,
) -> Result<u64, String> {
    let mut transport = neutron_remote::connect(site).map_err(|e| e.to_string())?;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(destination).map_err(|e| e.to_string())?;
    let mut sink = std::io::BufWriter::new(file);

    let outcome = transport.download(path, &mut sink, on_progress);

    // Flushed before the result is examined, so a write error on the last
    // buffer is not reported as a successful download.
    use std::io::Write;
    let flushed = sink.flush();

    match outcome {
        Ok(bytes) => {
            flushed.map_err(|e| e.to_string())?;
            Ok(bytes)
        }
        Err(e) => {
            // A partial file is worse than none here, unlike an interrupted
            // archive extraction: this one is about to be handed to whatever
            // application opens that file type, and half a PDF is a crash
            // report rather than a document.
            drop(sink);
            let _ = std::fs::remove_file(destination);
            Err(e.to_string())
        }
    }
}

/// Where a downloaded file is put before it is opened.
///
/// Under the user's temp directory rather than beside anything of theirs: these
/// are copies made to satisfy a double-click, and a file manager that scatters
/// them through the user's folders is one they have to clean up after.
pub fn cache_path(site_id: &str, remote_path: &str) -> Option<std::path::PathBuf> {
    let name = neutron_remote::path::file_name(remote_path);
    // The name has already been through `safe_name` on the way into the
    // listing, so it cannot contain a separator. Checked again here because
    // this function is what turns a remote name into a local path, and the
    // check is one line.
    neutron_remote::path::safe_name(&name).ok()?;

    Some(
        std::env::temp_dir()
            .join("Neutron")
            .join(sanitized_site(site_id))
            .join(name),
    )
}

/// A site id fit to be a directory name.
///
/// Site ids are already validated as slugs, so this changes nothing in
/// practice — it exists so that a hand-edited `sites.json` cannot turn a site
/// id into a path.
fn sanitized_site(site_id: &str) -> String {
    site_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_lands_under_the_temp_directory_and_nowhere_else() {
        let path = cache_path("nas", "/volume1/notes.txt").expect("a path");
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
        assert!(path.ends_with("notes.txt"), "{}", path.display());
    }

    #[test]
    fn a_hostile_site_id_cannot_become_a_path() {
        // Site ids are validated on the way in, so this is about a hand-edited
        // file — the one place a bad id can still arrive from.
        let path = cache_path("../../evil", "/a.txt").expect("a path");
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
        // Not "contains no dots" — a name may legitimately contain them. What
        // must not exist is a component that *means* the parent directory.
        assert!(
            !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "{}",
            path.display()
        );
    }

    #[test]
    fn a_root_path_has_no_file_to_cache() {
        // `/` names a directory, and `file_name` gives "/" for it, which is not
        // a name that can be created.
        assert_eq!(cache_path("nas", "/"), None);
    }

    #[test]
    fn each_site_caches_into_its_own_folder() {
        // Two servers can both have `/backup.tar.gz`, and one must not
        // overwrite the other's download.
        let a = cache_path("one", "/backup.tar.gz").unwrap();
        let b = cache_path("two", "/backup.tar.gz").unwrap();
        assert_ne!(a, b);
    }
}
