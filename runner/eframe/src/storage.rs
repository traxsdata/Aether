//! storage.rs - eframe's `Storage` in a file: a RON map of strings, as real
//! eframe writes it (`app.ron`).
//!
//! On æther an app sees only its private folder and names files relative to
//! it, so the store is `./app.ron`. Headless runs use a temporary folder
//! (or `AETHER_STORAGE_DIR`), never the host user's real app data.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The folder where the app with this id keeps its eframe state.
pub fn storage_dir(app_id: &str) -> Option<PathBuf> {
    if aether_host::on_aether() {
        return Some(PathBuf::from("."));
    }
    if let Some(dir) = std::env::var_os("AETHER_STORAGE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let id: String = app_id
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    Some(std::env::temp_dir().join("aether-runner").join(id))
}

pub struct FileStorage {
    path: PathBuf,
    kv: BTreeMap<String, String>,
    dirty: bool,
}

impl FileStorage {
    pub fn from_path(path: PathBuf) -> Self {
        let kv = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str(&text).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable {}: {e}", path.display());
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        FileStorage { path, kv, dirty: false }
    }

    pub fn from_app_id(app_id: &str) -> Option<Self> {
        let dir = storage_dir(app_id)?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            // Apps with no private folder (Calculator, Files) still run fine;
            // they just don't keep eframe state.
            log::debug!("saving disabled: can't create {}: {e}", dir.display());
            return None;
        }
        Some(Self::from_path(dir.join("app.ron")))
    }
}

impl crate::Storage for FileStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.kv.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        if self.kv.get(key) != Some(&value) {
            self.kv.insert(key.to_owned(), value);
            self.dirty = true;
        }
    }

    fn remove_string(&mut self, key: &str) {
        if self.kv.remove(key).is_some() {
            self.dirty = true;
        }
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let text = match ron::ser::to_string_pretty(&self.kv, ron::ser::PrettyConfig::default()) {
            Ok(t) => t,
            Err(e) => return log::error!("can't encode storage: {e}"),
        };
        // Write a sibling and rename it over, so a crash never leaves half a file.
        let tmp = self.path.with_extension("ron.tmp");
        match std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &self.path)) {
            Ok(()) => self.dirty = false,
            Err(e) => log::warn!("can't save {}: {e}", self.path.display()),
        }
    }
}
