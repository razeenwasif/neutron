//! Finding what is installed, without running any of it.
//!
//! One directory per plugin under `%APPDATA%\Neutron\plugins`, each holding a
//! `plugin.json` and the program it names. Discovery reads the manifests, checks
//! that the program is actually there, and stops. Nothing is executed.
//!
//! # Failures are kept, not dropped
//!
//! A directory that does not yield a plugin produces a [`LoadError`] carrying
//! the reason, and the application shows those alongside the plugins that
//! loaded. This is the difference between a plugin system somebody can write
//! for and one where a missing entry is an unanswerable question.
//!
//! # Ordering
//!
//! Plugins are sorted by id and each keeps its manifest's command order. The
//! palette shows commands with an empty query, so the order is *read*, and a
//! list that reshuffles because a directory was rewritten is disorienting.

use std::path::{Path, PathBuf};

use crate::manifest::{self, CommandDef, MANIFEST_NAME, Manifest};

/// Most plugin directories examined.
///
/// A stop on unbounded work in a directory the application does not control —
/// the plugins folder is on disk where anything can write to it, and startup
/// reads every manifest in it.
pub const MAX_PLUGINS: usize = 64;

/// Largest manifest read. Generous for the format; small enough that sixty-four
/// of them cannot matter.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// A plugin that loaded.
#[derive(Debug, Clone)]
pub struct Installed {
    /// The plugin's own directory. The program lives here and nowhere else.
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Installed {
    /// The program to run. Safe to join because
    /// [`manifest::program_name`] has already refused anything that is not a
    /// plain file name.
    pub fn program(&self) -> PathBuf {
        self.dir.join(&self.manifest.run)
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn command(&self, id: &str) -> Option<&CommandDef> {
        self.manifest.commands.iter().find(|c| c.id == id)
    }
}

/// A directory that did not produce a plugin, and why.
#[derive(Debug, Clone)]
pub struct LoadError {
    /// Shown to the user, so they can find the folder to fix.
    pub dir: PathBuf,
    /// One sentence, already fit to display.
    pub reason: String,
}

/// Everything installed, and everything that failed to install.
#[derive(Debug, Clone, Default)]
pub struct PluginSet {
    pub plugins: Vec<Installed>,
    pub errors: Vec<LoadError>,
}

/// `%APPDATA%\Neutron\plugins`.
///
/// The fallbacks match the rest of the application's state handling, and are
/// what let this be exercised on Linux.
pub fn plugins_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("Neutron").join("plugins"))
}

impl PluginSet {
    /// Reads the plugins folder, if there is one.
    ///
    /// A missing folder is the normal case for someone who has installed no
    /// plugins, and is not an error.
    pub fn load() -> PluginSet {
        match plugins_dir() {
            Some(dir) => PluginSet::load_from(&dir),
            None => PluginSet::default(),
        }
    }

    /// Reads a specific folder. Separate from [`PluginSet::load`] so the whole
    /// of discovery is testable.
    pub fn load_from(root: &Path) -> PluginSet {
        let mut set = PluginSet::default();

        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return set,
            Err(e) => {
                set.errors.push(LoadError {
                    dir: root.to_path_buf(),
                    reason: format!("could not be read: {e}"),
                });
                return set;
            }
        };

        // Collected and sorted before parsing, so which plugins survive the cap
        // does not depend on the order the filesystem happened to return.
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        dirs.sort();

        for dir in dirs.into_iter().take(MAX_PLUGINS) {
            match read_plugin(&dir) {
                Ok(installed) => {
                    // Two plugins claiming one id would make a command
                    // reference ambiguous, and the loser would be a command
                    // that runs the wrong program.
                    if let Some(other) = set.plugins.iter().find(|p| p.id() == installed.id()) {
                        let other = other.dir.clone();
                        set.errors.push(LoadError {
                            dir,
                            reason: format!(
                                "id {:?} is already used by {}",
                                installed.id(),
                                other.display()
                            ),
                        });
                        continue;
                    }
                    set.plugins.push(installed);
                }
                Err(reason) => set.errors.push(LoadError { dir, reason }),
            }
        }

        set.plugins.sort_by(|a, b| a.id().cmp(b.id()));
        set
    }

    /// Every command from every plugin, in display order.
    pub fn commands(&self) -> impl Iterator<Item = (&Installed, &CommandDef)> {
        self.plugins
            .iter()
            .flat_map(|p| p.manifest.commands.iter().map(move |c| (p, c)))
    }

    /// Resolves a command reference back to what runs it.
    ///
    /// By id rather than by index, because the set can be reloaded while the
    /// palette is open, and an index into the old list would then run the wrong
    /// command rather than reporting that this one is gone.
    pub fn find(&self, plugin: &str, command: &str) -> Option<(&Installed, &CommandDef)> {
        let installed = self.plugins.iter().find(|p| p.id() == plugin)?;
        Some((installed, installed.command(command)?))
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

/// Reads one plugin directory.
fn read_plugin(dir: &Path) -> Result<Installed, String> {
    let path = dir.join(MANIFEST_NAME);

    let size = std::fs::metadata(&path)
        .map_err(|e| match e.kind() {
            // The overwhelmingly common case: a stray folder someone dropped in
            // the plugins directory. Worth naming the file it wants.
            std::io::ErrorKind::NotFound => format!("has no {MANIFEST_NAME}"),
            _ => format!("{MANIFEST_NAME} could not be read: {e}"),
        })?
        .len();
    if size > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST_NAME} is larger than {MAX_MANIFEST_BYTES} bytes"));
    }

    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{MANIFEST_NAME} could not be read: {e}"))?;
    let manifest = manifest::parse(&text).map_err(|e| format!("{MANIFEST_NAME}: {e}"))?;

    let installed = Installed { dir: dir.to_path_buf(), manifest };

    // Checked now rather than at invocation, so a half-installed plugin is a
    // visible load error instead of a menu entry that fails when used.
    let program = installed.program();
    if !program.is_file() {
        return Err(format!("{:?} does not exist", installed.manifest.run));
    }

    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugins root under the system temp directory, emptied first so a
    /// previous run cannot make this one pass.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neutron-plugin-discover-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a plugin directory: manifest plus an empty file for the program.
    fn install(root: &Path, dir: &str, manifest: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MANIFEST_NAME), manifest).unwrap();
        std::fs::write(d.join("prog"), b"").unwrap();
    }

    fn manifest_json(id: &str) -> String {
        format!(
            r#"{{"protocol":1,"id":"{id}","name":"{id}","run":"prog",
                 "commands":[{{"id":"go","title":"Go {id}","needs":"folder"}}]}}"#
        )
    }

    #[test]
    fn a_missing_plugins_folder_is_not_an_error() {
        let set = PluginSet::load_from(Path::new("/nonexistent/neutron/plugins"));
        assert!(set.plugins.is_empty());
        assert!(set.errors.is_empty(), "{:?}", set.errors);
    }

    #[test]
    fn plugins_load_and_sort_by_id() {
        let root = scratch("sorted");
        install(&root, "zeta", &manifest_json("zeta"));
        install(&root, "alpha", &manifest_json("alpha"));

        let set = PluginSet::load_from(&root);
        assert!(set.errors.is_empty(), "{:?}", set.errors);
        let ids: Vec<&str> = set.plugins.iter().map(|p| p.id()).collect();
        assert_eq!(ids, ["alpha", "zeta"]);
    }

    #[test]
    fn a_folder_without_a_manifest_is_reported_not_skipped() {
        let root = scratch("nomanifest");
        std::fs::create_dir_all(root.join("junk")).unwrap();

        let set = PluginSet::load_from(&root);
        assert!(set.plugins.is_empty());
        assert_eq!(set.errors.len(), 1);
        assert!(set.errors[0].reason.contains(MANIFEST_NAME), "{:?}", set.errors[0]);
    }

    #[test]
    fn a_broken_manifest_reports_why() {
        let root = scratch("broken");
        let d = root.join("bad");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MANIFEST_NAME), "{ not json").unwrap();

        let set = PluginSet::load_from(&root);
        assert_eq!(set.errors.len(), 1);
        assert!(!set.errors[0].reason.is_empty());
    }

    #[test]
    fn a_manifest_naming_a_program_that_is_not_there_is_a_load_error() {
        // Rather than a command in the palette that fails when chosen.
        let root = scratch("noprog");
        let d = root.join("half");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MANIFEST_NAME), manifest_json("half")).unwrap();

        let set = PluginSet::load_from(&root);
        assert!(set.plugins.is_empty());
        assert!(set.errors[0].reason.contains("prog"), "{:?}", set.errors[0]);
    }

    #[test]
    fn two_plugins_may_not_share_an_id() {
        let root = scratch("dupid");
        install(&root, "first", &manifest_json("same"));
        install(&root, "second", &manifest_json("same"));

        let set = PluginSet::load_from(&root);
        assert_eq!(set.plugins.len(), 1);
        assert_eq!(set.errors.len(), 1);
        assert!(set.errors[0].reason.contains("already used"), "{:?}", set.errors[0]);
    }

    #[test]
    fn a_command_is_found_by_id_and_a_removed_one_is_not() {
        let root = scratch("find");
        install(&root, "p", &manifest_json("p"));

        let set = PluginSet::load_from(&root);
        assert!(set.find("p", "go").is_some());
        assert!(set.find("p", "gone").is_none());
        assert!(set.find("nobody", "go").is_none());
    }

    #[test]
    fn the_program_path_stays_inside_the_plugin_folder() {
        let root = scratch("contained");
        install(&root, "p", &manifest_json("p"));

        let set = PluginSet::load_from(&root);
        let program = set.plugins[0].program();
        assert!(program.starts_with(root.join("p")), "{}", program.display());
    }

    #[test]
    fn commands_are_listed_in_manifest_order_across_plugins() {
        let root = scratch("order");
        install(
            &root,
            "b",
            r#"{"protocol":1,"id":"b","name":"B","run":"prog","commands":[
                {"id":"two","title":"Two","needs":"folder"},
                {"id":"one","title":"One","needs":"folder"}]}"#,
        );
        install(&root, "a", &manifest_json("a"));

        let set = PluginSet::load_from(&root);
        let titles: Vec<&str> = set.commands().map(|(_, c)| c.title.as_str()).collect();
        // Plugin a before plugin b; inside b, the manifest's own order.
        assert_eq!(titles, ["Go a", "Two", "One"]);
    }

    #[test]
    fn a_file_in_the_plugins_folder_is_ignored() {
        // Not an error: a readme or a stray download does not deserve a
        // complaint in the UI.
        let root = scratch("stray");
        std::fs::write(root.join("readme.txt"), b"hello").unwrap();
        let set = PluginSet::load_from(&root);
        assert!(set.plugins.is_empty());
        assert!(set.errors.is_empty(), "{:?}", set.errors);
    }
}
