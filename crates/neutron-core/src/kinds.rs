//! What each kind of file is called, as the system names it — "Markdown
//! Document", "PNG image" — for the Kind column.
//!
//! Filled by the platform's listing code, once per extension, off the paint
//! thread; read by the views. A kind not yet named here is described by its
//! extension until it is.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

static NAMES: LazyLock<RwLock<HashMap<String, String>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// The system's name for files with extension `ext`, matched without regard
/// to case, if it has been learned.
pub fn name_of(ext: &str) -> Option<String> {
    NAMES.read().ok()?.get(&ext.to_ascii_lowercase()).cloned()
}

/// Whether `ext` has been looked up already.
pub fn known(ext: &str) -> bool {
    NAMES.read().is_ok_and(|names| names.contains_key(&ext.to_ascii_lowercase()))
}

/// Records the system's name for files with extension `ext`.
pub fn learn(ext: &str, name: String) {
    if let Ok(mut names) = NAMES.write() {
        names.insert(ext.to_ascii_lowercase(), name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_learned_name_is_found_whatever_the_case() {
        assert!(name_of("neutronkindtest").is_none());
        learn("NeutronKindTest", "Test Document".into());
        assert!(known("neutronkindtest"));
        assert_eq!(name_of("NEUTRONKINDTEST").as_deref(), Some("Test Document"));
    }
}
