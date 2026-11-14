//! Adopted backend: a copy of the Wand trainer runtime, trainer cache and
//! trainer blueprints that Wand Lite owns.
//!
//! Normally the client reads those directly from the installed Wand
//! application. A "debloat" install copies them into
//! `%LOCALAPPDATA%\WandRebuild\backend` so the Electron application can be
//! removed afterwards while Wand Lite keeps working.
//!
//! Layout produced by the debloat installer:
//!
//! ```text
//! %LOCALAPPDATA%\WandRebuild\backend\
//!   runtime\app-12.30.1\trainerlib\   TrainerLib_x64.dll, CELib_x64.dll, ...
//!   trainers\                          Trainer_<id>_<hash>.dll
//!   blueprints\                        copy of Wand's Local Storage leveldb
//!   backend.json                       provenance manifest
//! ```
//!
//! The copies are the user's own licensed files, kept locally; nothing here is
//! redistributed.

use crate::runtime::{WandRuntime, find_newest};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Files that must exist in an adopted runtime for it to be usable.
const REQUIRED_RUNTIME_FILES: &[&str] = &[
    "TrainerLib_x64.dll",
    "TrainerLib_x86.dll",
    "CELib_x64.dll",
    "CELib_x86.dll",
    "TrainerLibPlugin_x64.dll",
    "TrainerLibPlugin_x86.dll",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedBackend {
    pub version: Vec<u32>,
    /// Directory containing the copied `trainerlib` folder's parent.
    pub runtime_dir: PathBuf,
    /// Directory holding the copied trainer DLLs.
    pub trainers_dir: PathBuf,
    /// Directory holding the copied blueprint database.
    pub blueprints_dir: PathBuf,
    /// Provenance manifest path.
    pub manifest_path: PathBuf,
}

impl AdoptedBackend {
    /// The runtime descriptor, so callers can treat it like an installed one.
    pub fn runtime(&self) -> WandRuntime {
        WandRuntime {
            version: self.version.clone(),
            app_directory: self.runtime_dir.clone(),
            trainer_directory: self.runtime_dir.join("trainerlib"),
            archive_path: self.runtime_dir.join("app.asar"),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.trainer_directory_has_runtime()
            && self.trainers_dir.is_dir()
            && self.blueprints_dir.is_dir()
    }

    fn trainer_directory_has_runtime(&self) -> bool {
        let trainerlib = self.runtime_dir.join("trainerlib");
        REQUIRED_RUNTIME_FILES
            .iter()
            .all(|file| trainerlib.join(file).is_file())
    }

    pub fn trainer_count(&self) -> usize {
        fs::read_dir(&self.trainers_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .starts_with("trainer_")
                    })
                    .count()
            })
            .unwrap_or(0)
    }
}

/// Root of the adopted backend for this user.
pub fn root(local_app_data: &Path) -> PathBuf {
    local_app_data.join("WandRebuild").join("backend")
}

/// Finds a complete adopted backend, newest runtime version first.
pub fn find(local_app_data: &Path) -> Option<AdoptedBackend> {
    let root = root(local_app_data);
    let runtime_root = root.join("runtime");
    let entries = fs::read_dir(&runtime_root).ok()?;
    let mut candidates: Vec<(Vec<u32>, PathBuf)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(version) = name
            .strip_prefix("app-")
            .and_then(|rest| {
                rest.split('.')
                    .map(str::parse::<u32>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .filter(|parts| (2..=4).contains(&parts.len()))
        else {
            continue;
        };
        candidates.push((version, entry.path()));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    for (version, runtime_dir) in candidates {
        let backend = AdoptedBackend {
            version,
            runtime_dir,
            trainers_dir: root.join("trainers"),
            blueprints_dir: root.join("blueprints"),
            manifest_path: root.join("backend.json"),
        };
        if backend.is_complete() {
            return Some(backend);
        }
    }
    None
}

/// Runtime to use: an adopted backend wins over a live Wand installation.
pub fn resolve_runtime(local_app_data: &Path) -> io::Result<Option<WandRuntime>> {
    if let Some(adopted) = find(local_app_data) {
        return Ok(Some(adopted.runtime()));
    }
    find_newest(local_app_data)
}

/// True when the runtime came from an adopted backend.
pub fn is_adopted(local_app_data: &Path) -> bool {
    find(local_app_data).is_some()
}

/// Directory holding trainer DLLs: adopted copy first, else Wand's cache.
pub fn resolve_trainers_dir(app_data: &Path, local_app_data: &Path) -> Option<PathBuf> {
    if let Some(adopted) = find(local_app_data) {
        if adopted.trainers_dir.is_dir() {
            return Some(adopted.trainers_dir);
        }
    }
    let installed = app_data.join("wemod").join("App").join("trainers");
    installed.is_dir().then_some(installed)
}

/// Blueprint database directory: adopted copy first, else Wand's Local Storage.
pub fn resolve_blueprints_dir(app_data: &Path, local_app_data: &Path) -> PathBuf {
    if let Some(adopted) = find(local_app_data) {
        if adopted.blueprints_dir.is_dir() {
            return adopted.blueprints_dir;
        }
    }
    crate::trainer_blueprint::local_storage_directory(app_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wand-backend-store-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_runtime(root: &Path, version: &str, complete: bool) {
        let trainerlib = root
            .join("WandRebuild/backend/runtime")
            .join(format!("app-{version}"))
            .join("trainerlib");
        fs::create_dir_all(&trainerlib).unwrap();
        if complete {
            for file in REQUIRED_RUNTIME_FILES {
                fs::write(trainerlib.join(file), b"x").unwrap();
            }
        }
    }

    #[test]
    fn finds_nothing_without_an_adopted_backend() {
        let root = temp_root("empty");
        assert!(find(&root).is_none());
        assert!(!is_adopted(&root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_incomplete_runtimes_and_requires_all_directories() {
        let root = temp_root("incomplete");
        write_runtime(&root, "12.30.1", false);
        fs::create_dir_all(root.join("WandRebuild/backend/trainers")).unwrap();
        fs::create_dir_all(root.join("WandRebuild/backend/blueprints")).unwrap();
        assert!(find(&root).is_none(), "missing DLLs must not count");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_a_complete_backend_and_prefers_the_newest_version() {
        let root = temp_root("complete");
        for version in ["12.30.1", "12.31.0"] {
            write_runtime(&root, version, true);
        }
        fs::create_dir_all(root.join("WandRebuild/backend/trainers")).unwrap();
        fs::create_dir_all(root.join("WandRebuild/backend/blueprints")).unwrap();
        fs::write(
            root.join("WandRebuild/backend/trainers/Trainer_50696_f0de8ff90c.dll"),
            b"x",
        )
        .unwrap();

        let backend = find(&root).expect("complete backend");
        assert_eq!(backend.version, vec![12, 31, 0]);
        assert_eq!(backend.trainer_count(), 1);
        assert!(backend.is_complete());
        let runtime = backend.runtime();
        assert_eq!(runtime.trainer_directory, backend.runtime_dir.join("trainerlib"));

        // Adopted backend wins for every resolver.
        assert!(is_adopted(&root));
        assert_eq!(
            resolve_trainers_dir(&root, &root).unwrap(),
            backend.trainers_dir
        );
        assert_eq!(resolve_blueprints_dir(&root, &root), backend.blueprints_dir);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn falls_back_to_the_installed_layout_when_nothing_is_adopted() {
        let root = temp_root("fallback");
        let app_data = root.join("Roaming");
        let local = root.join("Local");
        let trainers = app_data.join("wemod/App/trainers");
        fs::create_dir_all(&trainers).unwrap();
        fs::create_dir_all(local.join("Wand")).unwrap();
        assert_eq!(
            resolve_trainers_dir(&app_data, &local).unwrap(),
            trainers
        );
        assert_eq!(
            resolve_blueprints_dir(&app_data, &local),
            app_data.join("wemod/Local Storage/leveldb")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
