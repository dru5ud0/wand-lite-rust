use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamGame {
    pub app_id: u64,
    pub name: String,
    pub install_path: PathBuf,
    pub launch_uri: String,
}

pub fn discover(steam_root: &Path) -> io::Result<Vec<SteamGame>> {
    if !steam_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut libraries = vec![steam_root.to_path_buf()];
    let config = steam_root.join("steamapps/libraryfolders.vdf");
    if let Ok(contents) = fs::read_to_string(config) {
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
        let key = library.to_string_lossy().to_lowercase();
        if !seen_libraries.insert(key) {
            continue;
        }
        let steamapps = library.join("steamapps");
        let entries = match fs::read_dir(&steamapps) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(contents) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Some(id) = contents.lines().find_map(|line| value_of(line, "appid")) else {
                continue;
            };
            let Ok(app_id) = id.parse::<u64>() else {
                continue;
            };
            let Some(game_name) = contents.lines().find_map(|line| value_of(line, "name")) else {
                continue;
            };
            if game_name.trim().is_empty() || game_name == "Steamworks Common Redistributables" {
                continue;
            }
            let Some(install_dir) = contents
                .lines()
                .find_map(|line| value_of(line, "installdir"))
            else {
                continue;
            };
            let common = steamapps.join("common");
            let location = common.join(install_dir);
            let (Ok(common_real), Ok(location_real)) =
                (common.canonicalize(), location.canonicalize())
            else {
                continue;
            };
            if !location_real.starts_with(&common_real) || !location_real.is_dir() {
                continue;
            }
            if !seen_games.insert(app_id) {
                continue;
            }
            games.push(SteamGame {
                app_id,
                name: game_name,
                install_path: location_real,
                launch_uri: format!("steam://rungameid/{app_id}"),
            });
        }
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    Ok(games)
}

pub fn last_played(steam_root: &Path) -> HashMap<u64, u64> {
    let mut result = HashMap::new();
    let Ok(users) = fs::read_dir(steam_root.join("userdata")) else {
        return result;
    };
    for user in users.flatten() {
        let path = user.path().join("config/localconfig.vdf");
        if let Ok(contents) = fs::read_to_string(path) {
            for (app_id, timestamp) in parse_last_played(&contents) {
                result
                    .entry(app_id)
                    .and_modify(|old| *old = (*old).max(timestamp))
                    .or_insert(timestamp);
            }
        }
    }
    result
}

fn parse_last_played(contents: &str) -> HashMap<u64, u64> {
    let mut result = HashMap::new();
    let mut depth = 0usize;
    let mut pending_apps = false;
    let mut apps_depth = None;
    let mut pending_app = None;
    let mut current_app = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == "{" {
            depth += 1;
            if pending_apps {
                apps_depth = Some(depth);
                pending_apps = false;
            } else if let Some(app_id) = pending_app.take() {
                current_app = Some((app_id, depth));
            }
            continue;
        }
        if trimmed == "}" {
            if current_app.is_some_and(|(_, app_depth)| app_depth == depth) {
                current_app = None;
            }
            if apps_depth == Some(depth) {
                apps_depth = None;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        let tokens = quoted_tokens(trimmed);
        if tokens.len() == 1 && tokens[0].eq_ignore_ascii_case("apps") {
            pending_apps = true;
        } else if apps_depth == Some(depth) && tokens.len() == 1 {
            pending_app = tokens[0].parse::<u64>().ok();
        } else if let Some((app_id, app_depth)) = current_app {
            if depth == app_depth && tokens.len() >= 2 && tokens[0] == "LastPlayed" {
                if let Ok(timestamp) = tokens[1].parse::<u64>() {
                    result.insert(app_id, timestamp);
                }
            }
        }
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_installed_game_and_rejects_missing_directory() {
        let root = std::env::temp_dir().join(format!("wand-rust-steam-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game = root.join("steamapps/common/Stick It to the Stickman");
        fs::create_dir_all(&game).unwrap();
        fs::write(root.join("steamapps/appmanifest_2085540.acf"),
            "\"AppState\"\n{\n\"appid\" \"2085540\"\n\"name\" \"Stick It to the Stickman\"\n\"installdir\" \"Stick It to the Stickman\"\n}").unwrap();
        fs::write(
            root.join("steamapps/appmanifest_999.acf"),
            "\"appid\" \"999\"\n\"name\" \"Missing Game\"\n\"installdir\" \"Missing\"",
        )
        .unwrap();
        let found = discover(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].app_id, 2085540);
        assert_eq!(found[0].launch_uri, "steam://rungameid/2085540");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unescapes_library_path() {
        assert_eq!(
            value_of(r#""path" "C:\\Steam\\Games""#, "path"),
            Some(r"C:\Steam\Games".into())
        );
    }

    #[test]
    fn reads_recent_play_without_confusing_nested_cloud_data() {
        let localconfig = "\"apps\"\n{\n\"2085540\"\n{\n\"cloud\"\n{\n\"LastPlayed\" \"999\"\n}\n\"LastPlayed\" \"123\"\n}\n\"10090\"\n{\n\"LastPlayed\" \"456\"\n}\n}";
        let times = parse_last_played(localconfig);
        assert_eq!(times.get(&2085540), Some(&123));
        assert_eq!(times.get(&10090), Some(&456));
    }
}
