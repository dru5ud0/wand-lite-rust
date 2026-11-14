//! Game process discovery and game-version resolution.
//!
//! The legacy trainer needs a live target PID and the game version the trainer
//! was built for. Wand caches the version it resolved for each installed game in
//! its Local Storage (`installedGameVersions`), keyed by game id.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameProcess {
    pub process_id: u32,
    pub executable: String,
    pub image_path: PathBuf,
}

/// Reads Wand's cached game version for a game id from Local Storage.
///
/// The record looks like
/// `"105784":[{"gameId":"105784","correlationId":"steam:2085540","version":2639440150,...}]`.
pub fn cached_game_version(storage_directory: &Path, game_id: &str) -> Option<u32> {
    // Wand writes the installed-version record as
    // `"<gameId>":[{"gameId":"<id>","correlationId":"steam:…","version":N,…}]`,
    // but field order is not stable across versions and the record may be split
    // across LevelDB pages, so match the id and then look ahead for `"version":`.
    // Trainer entries carry `"supportedVersions":[…]` instead, which this does
    // not match.
    let needle = format!("\"gameId\":\"{game_id}\"");
    let entries = std::fs::read_dir(storage_directory).ok()?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension == "log" || extension == "ldb")
        })
        .collect();
    paths.sort();
    for path in paths.into_iter().rev() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = decode_utf16le(&bytes);
        let mut search_from = 0;
        let mut best: Option<u32> = None;
        while let Some(relative) = text[search_from..].find(&needle) {
            let start = search_from + relative;
            search_from = start + needle.len();
            let tail = &text[start..text.len().min(start + 768)];
            let Some(version_at) = tail.find("\"version\":") else {
                continue;
            };
            let digits: String = tail[version_at + 10..]
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .collect();
            if let Ok(version) = digits.parse::<u32>() {
                if version > 0 {
                    best = Some(best.map_or(version, |current| current.max(version)));
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(windows)]
pub use windows_impl::find_process;

#[cfg(windows)]
mod windows_impl {
    use super::GameProcess;
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };

    /// Finds a running process whose executable name matches `executable`
    /// (case-insensitive, with or without the `.exe` suffix).
    pub fn find_process(executable: &str) -> Option<GameProcess> {
        let wanted = executable.trim_end_matches(".exe");
        // SAFETY: the snapshot API takes no pointers for this call.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let result = (|| {
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            // SAFETY: `entry` is a valid, correctly sized PROCESSENTRY32W.
            if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
                return None;
            }
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                if name
                    .trim_end_matches(".exe")
                    .eq_ignore_ascii_case(wanted)
                {
                    return Some(GameProcess {
                        process_id: entry.th32ProcessID,
                        executable: name,
                        image_path: PathBuf::new(),
                    });
                }
                // SAFETY: same valid entry as above.
                if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                    return None;
                }
            }
        })();
        // SAFETY: the snapshot handle is owned by this function.
        unsafe {
            CloseHandle(snapshot);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cached_game_version_from_utf16_leveldb_page() {
        let directory =
            std::env::temp_dir().join(format!("wand-rebuild-versions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let record = r#""installedGameVersions":{"105784":[{"gameId":"105784","correlationId":"steam:2085540","version":2639440150,"modifiedAt":1791905109}]}"#;
        let mut bytes = Vec::new();
        for unit in record.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(directory.join("000003.log"), bytes).unwrap();
        assert_eq!(
            cached_game_version(&directory, "105784"),
            Some(2_639_440_150)
        );
        assert_eq!(cached_game_version(&directory, "999"), None);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_directory_yields_no_version() {
        let missing = std::env::temp_dir().join("wand-rebuild-versions-missing");
        assert_eq!(cached_game_version(&missing, "105784"), None);
    }

    #[cfg(windows)]
    #[test]
    fn finds_the_current_process_by_executable_name() {
        let current = std::env::current_exe().unwrap();
        let name = current.file_name().unwrap().to_string_lossy().into_owned();
        let found = find_process(&name).expect("current test process must be enumerable");
        assert_eq!(found.process_id, std::process::id());
    }

    #[cfg(windows)]
    #[test]
    fn does_not_find_an_absent_executable() {
        assert!(find_process("definitely-not-a-real-process-xyz.exe").is_none());
    }
}
