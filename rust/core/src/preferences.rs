use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub favorites: BTreeSet<String>,
    pub sort_descending: bool,
    #[serde(default)]
    pub hotkeys: std::collections::BTreeMap<String, String>,
}

impl Preferences {
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            // Tolerate a UTF-8 BOM: editors and PowerShell's `Set-Content` add one,
            // and `serde_json` rejects it. Without this the whole preferences file
            // (favorites, sort order, hotkeys) would be silently discarded.
            Ok(bytes) => serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes)
    }

    pub fn toggle_favorite(&mut self, id: &str) -> bool {
        if self.favorites.remove(id) {
            false
        } else {
            self.favorites.insert(id.to_owned());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerates_a_utf8_bom_in_the_preferences_file() {
        let path = std::env::temp_dir().join(format!(
            "wand-rust-preferences-bom-{}.json",
            std::process::id()
        ));
        let body = br#"{"favorites":["steam:1"],"sort_descending":true,"hotkeys":{"god_mode":"Ctrl+F9"}}"#;
        let mut with_bom = b"\xEF\xBB\xBF".to_vec();
        with_bom.extend_from_slice(body);
        fs::write(&path, with_bom).unwrap();
        let preferences = Preferences::load(&path).unwrap();
        assert!(preferences.favorites.contains("steam:1"));
        assert!(preferences.sort_descending);
        assert_eq!(
            preferences.hotkeys.get("god_mode").map(String::as_str),
            Some("Ctrl+F9")
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn persists_favorites_and_sort_order() {
        let path =
            std::env::temp_dir().join(format!("wand-rust-preferences-{}.json", std::process::id()));
        assert_eq!(Preferences::load(&path).unwrap(), Preferences::default());
        let mut preferences = Preferences::default();
        assert!(preferences.toggle_favorite("steam:2085540"));
        preferences.sort_descending = true;
        preferences.save(&path).unwrap();
        assert_eq!(Preferences::load(&path).unwrap(), preferences);
        assert!(!preferences.toggle_favorite("steam:2085540"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_old_preferences_without_hotkeys() {
        let path = std::env::temp_dir().join(format!(
            "wand-rust-preferences-old-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, r#"{"favorites":["steam:2085540"],"sort_descending":true}"#).unwrap();
        let preferences = Preferences::load(&path).unwrap();
        assert!(preferences.hotkeys.is_empty());
        assert!(preferences.favorites.contains("steam:2085540"));
        assert!(preferences.sort_descending);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn hotkeys_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "wand-rust-preferences-hotkeys-{}.json",
            std::process::id()
        ));
        let mut preferences = Preferences::default();
        preferences
            .hotkeys
            .insert("god_mode".to_owned(), "Ctrl+F1".to_owned());
        preferences.save(&path).unwrap();
        assert_eq!(Preferences::load(&path).unwrap(), preferences);
        std::fs::remove_file(path).unwrap();
    }
}
