use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInstall {
    pub steam_app_id: u64,
    pub name: String,
    pub install_dir: PathBuf,
    pub executable_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub process_id: u32,
    pub executable: String,
    pub image_path: PathBuf,
}

pub fn installed_games(steam_root: &Path) -> Vec<GameInstall> {
    if !steam_root.is_dir() {
        return Vec::new();
    }
    let mut libraries = vec![steam_root.to_path_buf()];
    let config = steam_root.join("steamapps").join("libraryfolders.vdf");
    if let Ok(contents) = fs::read_to_string(&config) {
        for line in contents.lines() {
            if let Some(path) = value_of(line, "path") {
                libraries.push(PathBuf::from(path));
            }
        }
    }

    let mut seen_libraries = HashSet::new();
    let mut seen_games = HashSet::new();
    let mut games = Vec::new();
    for library in libraries {
        let key = normalize_path(&library)
            .to_string_lossy()
            .to_lowercase();
        if !seen_libraries.insert(key) {
            continue;
        }
        let steamapps = library.join("steamapps");
        let entries = match fs::read_dir(&steamapps) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                continue;
            }
            let Ok(contents) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Some(id_str) = contents.lines().find_map(|line| value_of(line, "appid")) else {
                continue;
            };
            let Ok(app_id) = id_str.parse::<u64>() else {
                continue;
            };
            let Some(game_name) = contents.lines().find_map(|line| value_of(line, "name")) else {
                continue;
            };
            if game_name.trim().is_empty() || game_name == "Steamworks Common Redistributables" {
                continue;
            }
            let Some(install_dir_name) =
                contents.lines().find_map(|line| value_of(line, "installdir"))
            else {
                continue;
            };
            let common = steamapps.join("common");
            let location = common.join(&install_dir_name);
            if !path_is_under(&location, &common) {
                continue;
            }
            if !location.is_dir() {
                continue;
            }
            if !seen_games.insert(app_id) {
                continue;
            }
            let hint = contents
                .lines()
                .find_map(|line| value_of(line, "executable"));
            games.push(GameInstall {
                steam_app_id: app_id,
                name: game_name,
                install_dir: normalize_path(&location),
                executable_hint: hint,
            });
        }
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    games
}

pub fn processes_under(install_dir: &Path) -> Vec<RunningProcess> {
    #[cfg(windows)]
    {
        windows_impl::processes_under(install_dir)
    }
    #[cfg(not(windows))]
    {
        let _ = install_dir;
        Vec::new()
    }
}

pub fn pick_main_process(install: &GameInstall, running: &[RunningProcess]) -> Option<RunningProcess> {
    let mut candidates: Vec<&RunningProcess> = running
        .iter()
        .filter(|process| path_is_under(&process.image_path, &install.install_dir))
        // Helper processes that live in the install directory but are never the
        // game itself (Unity/Unreal crash reporters, anti-cheat shims, launchers).
        .filter(|process| !is_helper_executable(&process.executable))
        .collect();

    if let Some(hint) = &install.executable_hint {
        let hint = hint.to_lowercase();
        let matched: Vec<&RunningProcess> = candidates
            .iter()
            .copied()
            .filter(|process| {
                process.executable.to_lowercase() == hint
                    || process
                        .image_path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_lowercase() == hint)
                        .unwrap_or(false)
            })
            .collect();
        if !matched.is_empty() {
            candidates = matched;
        }
    }

    let mut best: Option<&RunningProcess> = None;
    for candidate in candidates {
        let direct = is_direct_child(&candidate.image_path, &install.install_dir);
        let size = fs::metadata(&candidate.image_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        match best {
            None => best = Some(candidate),
            Some(current) => {
                let current_direct = is_direct_child(&current.image_path, &install.install_dir);
                let current_size = fs::metadata(&current.image_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                if (direct, size, candidate.process_id)
                    > (current_direct, current_size, current.process_id)
                {
                    best = Some(candidate);
                }
            }
        }
    }
    best.cloned()
}

/// Executables that ship inside a game directory but are not the game.
///
/// Matching is on the file stem so versioned names such as
/// `UnityCrashHandler64.exe` and `crashpad_handler` are both covered.
pub fn is_helper_executable(executable: &str) -> bool {
    let stem = executable.trim_end_matches(".exe").to_ascii_lowercase();
    const DENY: &[&str] = &[
        "werfault",
        "unins000",
        "dxsetup",
        "vcredist_x64",
        "vcredist_x86",
        "steamerrorreporter",
        "start_protected_game",
    ];
    if DENY.iter().any(|denied| stem == *denied) {
        return true;
    }
    // Prefix matches cover versioned and derived names.
    stem.starts_with("unitycrashhandler")
        || stem.starts_with("crashpad")
        || stem.starts_with("crashreport")
        || stem.starts_with("easyanticheat")
        || stem.starts_with("battleye")
        || stem.starts_with("unins")
}

fn is_direct_child(image_path: &Path, install_dir: &Path) -> bool {
    let image = normalize_path(image_path);
    let install = normalize_path(install_dir);
    image
        .parent()
        .map(|parent| normalize_path(parent) == install)
        .unwrap_or(false)
}

fn path_is_under(child: &Path, parent: &Path) -> bool {
    let child = normalize_path(child)
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\");
    let parent = normalize_path(parent)
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\");
    if child == parent {
        return true;
    }
    let parent = if parent.ends_with('\\') {
        parent
    } else {
        parent + "\\"
    };
    child.starts_with(&parent)
}

fn normalize_path(path: &Path) -> PathBuf {
    let base = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut text = base.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        text = format!(r"\\{}", rest);
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    text = text.replace('/', "\\");
    while text.len() > 3 && text.ends_with('\\') {
        text.pop();
    }
    PathBuf::from(text)
}

fn value_of(line: &str, key: &str) -> Option<String> {
    let tokens = quoted_tokens(line);
    (tokens.len() >= 2 && tokens[0].eq_ignore_ascii_case(key)).then(|| tokens[1].clone())
}

fn quoted_tokens(line: &str) -> Vec<String> {
    let mut chars = line.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut token = String::new();
        while let Some(next) = chars.next() {
            match next {
                '"' => break,
                '\\' if matches!(chars.peek(), Some('"' | '\\')) => {
                    token.push(chars.next().unwrap());
                }
                other => token.push(other),
            }
        }
        tokens.push(token);
    }
    tokens
}

#[cfg(windows)]
mod windows_impl {
    use super::{normalize_path, path_is_under, RunningProcess};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub fn processes_under(install_dir: &Path) -> Vec<RunningProcess> {
        let mut result = Vec::new();
        // SAFETY: snapshot creation takes no pointers.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return result;
        }
        let normalized_install = normalize_path(install_dir);
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: entry is valid and correctly sized.
        if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
            // SAFETY: close owned snapshot.
            unsafe {
                CloseHandle(snapshot);
            }
            return result;
        }
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(entry.szExeFile.len());
            let toolhelp_name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            let pid = entry.th32ProcessID;
            if pid != 0 {
                // SAFETY: scalar arguments.
                let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
                if !handle.is_null() {
                    let mut buffer = vec![0u16; 32768];
                    let mut size = buffer.len() as u32;
                    // SAFETY: buffer and size are valid.
                    let ok = unsafe {
                        QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size)
                    };
                    if ok != 0 && size > 0 {
                        let image_path = PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize]));
                        let normalized_image = normalize_path(&image_path);
                        if path_is_under(&normalized_image, &normalized_install) {
                            let executable = image_path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| toolhelp_name.clone());
                            result.push(RunningProcess {
                                process_id: pid,
                                executable,
                                image_path: normalized_image,
                            });
                        }
                    }
                    // SAFETY: handle owned by this function.
                    unsafe {
                        CloseHandle(handle);
                    }
                }
            }
            // SAFETY: entry valid.
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }
        // SAFETY: close owned snapshot.
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
    fn normalizes_verbatim_paths() {
        let path = PathBuf::from(r"\\?\C:\Games\Stick\");
        assert_eq!(normalize_path(&path), PathBuf::from(r"C:\Games\Stick"));
    }

    #[test]
    fn helper_executables_are_excluded_from_game_candidates() {
        // Observed live: the Unity crash handler sits directly in the install
        // directory and was picked over the real game before this filter.
        assert!(is_helper_executable("UnityCrashHandler64.exe"));
        assert!(is_helper_executable("crashpad_handler.exe"));
        assert!(is_helper_executable("EasyAntiCheat.exe"));
        assert!(is_helper_executable("vcredist_x64.exe"));
        assert!(!is_helper_executable("Stick It To The Stickman.exe"));
        assert!(!is_helper_executable("Game"));
    }

    #[test]
    fn path_containment_is_case_insensitive() {
        assert!(path_is_under(
            Path::new(r"C:\Games\Stick\bin\game.exe"),
            Path::new(r"c:\games\stick")
        ));
        assert!(!path_is_under(
            Path::new(r"C:\Games\Other\game.exe"),
            Path::new(r"C:\Games\Stick")
        ));
        assert!(path_is_under(
            Path::new(r"C:\Games\Stick"),
            Path::new(r"C:\Games\Stick")
        ));
    }

    #[test]
    fn pick_prefers_direct_then_size_then_pid() {
        let install = GameInstall {
            steam_app_id: 1,
            name: "Game".to_owned(),
            install_dir: PathBuf::from(r"C:\Games\Stick"),
            executable_hint: None,
        };
        let direct = RunningProcess {
            process_id: 10,
            executable: "game.exe".to_owned(),
            image_path: PathBuf::from(r"C:\Games\Stick\game.exe"),
        };
        let nested = RunningProcess {
            process_id: 99,
            executable: "helper.exe".to_owned(),
            image_path: PathBuf::from(r"C:\Games\Stick\bin\helper.exe"),
        };
        let chosen = pick_main_process(&install, &[nested, direct.clone()]);
        assert_eq!(chosen, Some(direct));
    }

    #[test]
    fn pick_uses_executable_hint_when_present() {
        let install = GameInstall {
            steam_app_id: 1,
            name: "Game".to_owned(),
            install_dir: PathBuf::from(r"C:\Games\Stick"),
            executable_hint: Some("target.exe".to_owned()),
        };
        let target = RunningProcess {
            process_id: 1,
            executable: "target.exe".to_owned(),
            image_path: PathBuf::from(r"C:\Games\Stick\sub\target.exe"),
        };
        let other = RunningProcess {
            process_id: 100,
            executable: "game.exe".to_owned(),
            image_path: PathBuf::from(r"C:\Games\Stick\game.exe"),
        };
        let chosen = pick_main_process(&install, &[other, target.clone()]);
        assert_eq!(chosen, Some(target));
    }

    #[test]
    fn installed_games_reads_synthetic_steam_tree() {
        let root = std::env::temp_dir().join(format!("wand-rust-gamelocator-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game_dir = root.join("steamapps/common/Test Game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            "\"path\" \"ignored\"\n",
        )
        .unwrap();
        fs::write(
            root.join("steamapps/appmanifest_770.acf"),
            "\"AppState\"\n{\n\"appid\" \"770\"\n\"name\" \"Test Game\"\n\"installdir\" \"Test Game\"\n\"executable\" \"game.exe\"\n}",
        )
        .unwrap();
        let games = installed_games(&root);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].steam_app_id, 770);
        assert_eq!(games[0].name, "Test Game");
        assert!(games[0].install_dir.ends_with("Test Game"));
        assert_eq!(games[0].executable_hint.as_deref(), Some("game.exe"));
        let _ = fs::remove_dir_all(&root);
    }
}
