//! Trainer blueprints: the per-title list of cheats, their runtime variable
//! names and value ranges.
//!
//! The legacy (`trainerlib`) backend needs two things from a blueprint:
//!
//! * `cheats[].target` — the variable names handed to the trainer at start and
//!   used later by `setValue` frames.
//! * `loader` / `loaderArgs` — which backend and which trainer binary to use.
//!
//! Wand caches complete trainer entries (including the full `blueprint`) inside
//! its Chromium Local Storage LevelDB. This module reads those cached entries
//! read-only. It never reads account tokens: entries are located by trainer or
//! game id and only the trainer object is decoded.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Which runtime a title's trainer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `TrainerLib_{arch}.dll` injected into the game process, `WeMod\Trainer_*` pipes.
    TrainerLib,
    /// Local gRPC service, `LoadPlugin`/`ExecuteCommand`.
    Tophat,
    /// Local trainer binary shipped outside the account catalog.
    TrainerLibLocal,
    Unknown,
}

impl Backend {
    pub fn from_loader(loader: Option<&str>) -> Self {
        match loader {
            Some("trainerlib") => Self::TrainerLib,
            Some("tophat") => Self::Tophat,
            Some("trainerlib-local") => Self::TrainerLibLocal,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrainerLib => "trainerlib",
            Self::Tophat => "tophat",
            Self::TrainerLibLocal => "trainerlib-local",
            Self::Unknown => "unknown",
        }
    }
}

/// Control kind shown in the trainer panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheatKind {
    Toggle,
    Button,
    Number,
    Slider,
    Unknown,
}

impl CheatKind {
    fn parse(value: &str) -> Self {
        match value {
            "toggle" => Self::Toggle,
            "button" => Self::Button,
            "number" => Self::Number,
            "slider" => Self::Slider,
            _ => Self::Unknown,
        }
    }
}

/// Numeric bounds for `number`/`slider` cheats.
#[derive(Debug, Clone, PartialEq)]
pub struct CheatArgs {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub value: Option<f64>,
    pub button: Option<String>,
}

impl CheatArgs {
    /// Sensible defaults when a blueprint omits bounds.
    pub fn effective_range(&self) -> (f64, f64, f64) {
        let min = self.min.unwrap_or(0.0);
        let max = self.max.unwrap_or(100.0).max(min + f64::EPSILON);
        let step = self
            .step
            .filter(|step| *step > 0.0)
            .unwrap_or_else(|| ((max - min) / 100.0).max(1.0));
        (min, max, step)
    }

    /// Value a toggle/button sends when activated.
    pub fn activation_value(&self) -> f64 {
        self.value.unwrap_or(1.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cheat {
    pub uuid: String,
    pub name: String,
    pub category: String,
    pub kind: CheatKind,
    pub target: String,
    pub args: CheatArgs,
    pub instructions: Option<String>,
    pub parent: Option<String>,
    /// Raw hotkey chords, e.g. `[[112]]` for F1.
    pub hotkeys: Vec<Vec<u32>>,
}

impl Cheat {
    /// Default value for this cheat when the trainer starts.
    pub fn initial_value(&self) -> f64 {
        match self.kind {
            CheatKind::Number | CheatKind::Slider => self
                .args
                .value
                .or(self.args.min)
                .unwrap_or_else(|| self.args.effective_range().0),
            _ => 0.0,
        }
    }

    /// Value sent when the user activates the cheat.
    pub fn enabled_value(&self) -> f64 {
        match self.kind {
            CheatKind::Number | CheatKind::Slider => self
                .args
                .value
                .unwrap_or_else(|| self.args.effective_range().0),
            _ => self.args.activation_value(),
        }
    }

    pub fn is_toggle(&self) -> bool {
        self.kind == CheatKind::Toggle
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrainerBlueprint {
    pub trainer_id: String,
    pub game_id: String,
    pub title_id: String,
    pub supported_versions: Vec<u32>,
    pub backend: Backend,
    pub binary_url: Option<String>,
    pub binary_hash: Option<String>,
    /// Blueprint-level flags (`blueprint.flags`).
    pub flags: u32,
    pub cheats: Vec<Cheat>,
    /// Game event keys; the trainer subscribes to `wm_event_<key[..22]>`.
    pub game_event_keys: Vec<String>,
    /// Where this blueprint came from, for honest reporting in the UI.
    pub source: BlueprintSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlueprintSource {
    /// Read from Wand's own cached trainer entry.
    WandCache,
    /// Built from the public trainer page / name convention, not yet verified.
    Candidate,
}

impl TrainerBlueprint {
    /// Variable names passed to the legacy trainer at start-up, matching the
    /// reference client exactly: every cheat target plus every game event.
    pub fn variables(&self) -> Vec<String> {
        let mut ordered = BTreeSet::new();
        for cheat in &self.cheats {
            if !cheat.target.is_empty() {
                ordered.insert(cheat.target.clone());
            }
        }
        for key in &self.game_event_keys {
            let truncated: String = key.chars().take(22).collect();
            ordered.insert(format!("wm_event_{truncated}"));
        }
        ordered.into_iter().collect()
    }

    pub fn cheat_for_target(&self, target: &str) -> Option<&Cheat> {
        self.cheats.iter().find(|cheat| cheat.target == target)
    }

    /// True when the trainer lists `game_version` as supported.
    pub fn supports_version(&self, game_version: u32) -> bool {
        self.supported_versions.is_empty() || self.supported_versions.contains(&game_version)
    }

    /// Newest game version this trainer was published for.
    ///
    /// Used only as a fallback when the installed game version cannot be read
    /// from Wand's cache, so the trainer still receives a plausible build id
    /// instead of zero.
    pub fn preferred_game_version(&self) -> Option<u32> {
        self.supported_versions.iter().copied().max()
    }

    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = Vec::new();
        for cheat in &self.cheats {
            let category = if cheat.category.is_empty() {
                "mods".to_owned()
            } else {
                cheat.category.clone()
            };
            if !groups.contains(&category) {
                groups.push(category);
            }
        }
        groups
    }
}

#[derive(Debug)]
pub struct BlueprintError {
    pub message: String,
}

impl std::fmt::Display for BlueprintError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BlueprintError {}

fn blueprint_error(message: impl Into<String>) -> BlueprintError {
    BlueprintError {
        message: message.into(),
    }
}

#[derive(Debug, Deserialize)]
struct RawArgs {
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    button: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCheat {
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    category: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    args: Option<RawArgs>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    hotkeys: Vec<Vec<u32>>,
}

#[derive(Debug, Deserialize)]
struct RawBlueprint {
    #[serde(default)]
    cheats: Vec<RawCheat>,
    #[serde(default)]
    flags: u32,
    #[serde(default)]
    events: Vec<RawEvent>,
}

#[derive(Debug, Deserialize)]
struct RawEvent {
    #[serde(default)]
    key: String,
}

#[derive(Debug, Deserialize)]
struct RawLoaderArgs {
    #[serde(default, rename = "binaryUrl")]
    binary_url: Option<String>,
    #[serde(default, rename = "binaryHash")]
    binary_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawTrainerEntry {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "gameId")]
    game_id: String,
    #[serde(default, rename = "titleId")]
    title_id: String,
    #[serde(default, rename = "supportedVersions")]
    supported_versions: Vec<u32>,
    #[serde(default)]
    loader: Option<String>,
    #[serde(default, rename = "loaderArgs")]
    loader_args: Option<RawLoaderArgs>,
    #[serde(default)]
    blueprint: Option<RawBlueprint>,
}

fn convert(entry: RawTrainerEntry, source: BlueprintSource) -> Result<TrainerBlueprint, BlueprintError> {
    let raw = entry
        .blueprint
        .ok_or_else(|| blueprint_error("trainer entry has no blueprint"))?;
    let cheats = raw
        .cheats
        .into_iter()
        .map(|cheat| Cheat {
            uuid: cheat.uuid,
            name: cheat.name,
            category: cheat.category,
            kind: CheatKind::parse(&cheat.kind),
            target: cheat.target,
            args: cheat.args.map_or(
                CheatArgs {
                    min: None,
                    max: None,
                    step: None,
                    value: None,
                    button: None,
                },
                |args| CheatArgs {
                    min: args.min,
                    max: args.max,
                    step: args.step,
                    value: args.value,
                    button: args.button,
                },
            ),
            instructions: cheat.instructions,
            parent: cheat.parent,
            hotkeys: cheat.hotkeys,
        })
        .collect();
    Ok(TrainerBlueprint {
        trainer_id: entry.id,
        game_id: entry.game_id,
        title_id: entry.title_id,
        supported_versions: entry.supported_versions,
        backend: Backend::from_loader(entry.loader.as_deref()),
        binary_url: entry.loader_args.as_ref().and_then(|args| args.binary_url.clone()),
        binary_hash: entry.loader_args.as_ref().and_then(|args| args.binary_hash.clone()),
        flags: raw.flags,
        cheats,
        game_event_keys: raw.events.into_iter().map(|event| event.key).collect(),
        source,
    })
}

/// Parses a single trainer entry JSON object.
pub fn parse_trainer_entry(json: &str) -> Result<TrainerBlueprint, BlueprintError> {
    let entry: RawTrainerEntry = serde_json::from_str(json)
        .map_err(|error| blueprint_error(format!("invalid trainer entry: {error}")))?;
    convert(entry, BlueprintSource::WandCache)
}

/// Extracts the balanced `{...}` object that contains `"id":"<trainer_id>"`.
///
/// LevelDB pages are not aligned to JSON records, so this walks backwards to
/// each candidate object start, nearest first, and counts braces while
/// respecting string literals and escapes. A candidate is only accepted when
/// the extracted slice is itself a JSON object, which rejects braces that
/// merely appear inside another record's string values.
pub fn extract_object_containing(text: &str, needle: &str) -> Option<String> {
    let needle_index = text.find(needle)?;
    extract_object_at(text, needle_index)
}

/// Extracts the balanced JSON object enclosing the byte at `needle_index`.
pub fn extract_object_at(text: &str, needle_index: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let candidates = (0..needle_index).rev().filter(|index| bytes[*index] == b'{');
    for start in candidates {
        let Some(candidate) = balanced_object(&text[start..]) else {
            continue;
        };
        if serde_json::from_str::<serde_json::Value>(&candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
}

/// Returns the balanced JSON object that begins at the first byte of `text`.
fn balanced_object(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return text.get(..offset + 1).map(str::to_owned);
                }
            }
            _ => {}
        }
    }
    None
}

/// Default LevelDB location for Wand's Chromium Local Storage.
pub fn local_storage_directory(app_data: &Path) -> PathBuf {
    app_data.join("wemod").join("Local Storage").join("leveldb")
}

/// Reads a cached trainer entry for `trainer_id` from Wand's Local Storage.
///
/// Returns `Ok(None)` when Wand simply has not cached that trainer.
pub fn load_cached(
    storage_directory: &Path,
    trainer_id: &str,
) -> Result<Option<TrainerBlueprint>, BlueprintError> {
    load_cached_matching(storage_directory, &format!("\"id\":\"{trainer_id}\""), |blueprint| {
        blueprint.trainer_id == trainer_id
    })
}

/// Reads a cached trainer entry for a Wand *title* id.
///
/// This is how a game resolved from the catalog is mapped to its trainer without
/// a per-title table: Wand caches the complete trainer entry (including the
/// backend `loader` and the cheat blueprint) for titles the user has opened.
pub fn load_cached_for_title(
    storage_directory: &Path,
    title_id: &str,
) -> Result<Option<TrainerBlueprint>, BlueprintError> {
    load_cached_matching(
        storage_directory,
        &format!("\"titleId\":\"{title_id}\""),
        |blueprint| blueprint.title_id == title_id,
    )
}

fn load_cached_matching(
    storage_directory: &Path,
    needle: &str,
    accept: impl Fn(&TrainerBlueprint) -> bool,
) -> Result<Option<TrainerBlueprint>, BlueprintError> {
    Ok(load_cached_all(storage_directory, needle, accept)?.into_iter().next())
}

/// Reads every cached trainer entry matching `title_id`.
///
/// A title can have several cached trainers (Wand keeps the previous build after
/// a game update), so callers must select by game version rather than taking the
/// first hit.
pub fn load_cached_all_for_title(
    storage_directory: &Path,
    title_id: &str,
) -> Result<Vec<TrainerBlueprint>, BlueprintError> {
    load_cached_all(
        storage_directory,
        &format!("\"titleId\":\"{title_id}\""),
        |blueprint| blueprint.title_id == title_id,
    )
}

/// Chooses the trainer that matches `game_version`.
///
/// Preference order: an exact `supportedVersions` match (highest trainer id
/// wins, i.e. the newest published trainer), then the trainer advertising the
/// newest supported version, then the highest trainer id.
pub fn select_for_version(
    candidates: Vec<TrainerBlueprint>,
    game_version: u32,
) -> Option<TrainerBlueprint> {
    if candidates.is_empty() {
        return None;
    }
    let id_of = |blueprint: &TrainerBlueprint| blueprint.trainer_id.parse::<u64>().unwrap_or(0);
    let exact: Vec<&TrainerBlueprint> = candidates
        .iter()
        .filter(|blueprint| blueprint.supported_versions.contains(&game_version))
        .collect();
    if let Some(best) = exact.into_iter().max_by_key(|blueprint| id_of(blueprint)) {
        return Some(best.clone());
    }
    candidates.into_iter().max_by(|left, right| {
        left.preferred_game_version()
            .unwrap_or(0)
            .cmp(&right.preferred_game_version().unwrap_or(0))
            .then_with(|| id_of(left).cmp(&id_of(right)))
    })
}

fn load_cached_all(
    storage_directory: &Path,
    needle: &str,
    accept: impl Fn(&TrainerBlueprint) -> bool,
) -> Result<Vec<TrainerBlueprint>, BlueprintError> {
    let entries = match std::fs::read_dir(storage_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(blueprint_error(format!("cannot list Local Storage: {error}"))),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension == "log" || extension == "ldb")
        })
        .collect();
    // Newest files first: the most recent cache write is the most likely hit.
    paths.sort();
    let mut found: Vec<TrainerBlueprint> = Vec::new();
    for path in paths.into_iter().rev() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = decode_utf16le(&bytes);
        // A page can hold several matching records; try each one.
        let mut search_from = 0;
        while let Some(relative) = text[search_from..].find(needle) {
            let start = search_from + relative;
            search_from = start + needle.len();
            let Some(object) = extract_object_at(&text, start) else {
                continue;
            };
            if let Ok(blueprint) = parse_trainer_entry(&object) {
                if !accept(&blueprint) {
                    continue;
                }
                // The same entry can appear in more than one LevelDB page.
                if !found
                    .iter()
                    .any(|existing| existing.trainer_id == blueprint.trainer_id)
                {
                    found.push(blueprint);
                }
            }
        }
    }
    Ok(found)
}

/// Chromium Local Storage stores strings as UTF-16LE; LevelDB records are
/// interleaved with binary metadata, so decoding lossily is intentional.
fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Builds the Stickman candidate blueprint from the public trainer page.
///
/// The public page confirms the five display names. The `target` identifiers
/// follow Wand's published naming convention and are **verified at runtime**:
/// the legacy trainer answers a `Value` frame with a non-zero
/// `ValueResponse.result` when the variable name is unknown.
pub fn stickman_candidate() -> TrainerBlueprint {
    let cheats = [
        ("God Mode", "god_mode", CheatKind::Toggle, "player"),
        ("Add Money When Spend", "add_money_when_spend", CheatKind::Toggle, "stats"),
        ("Add Reroll Dice When Use", "add_reroll_dice_when_use", CheatKind::Toggle, "stats"),
        ("Ammo Never Decrease", "ammo_never_decrease", CheatKind::Toggle, "weapons"),
        ("Instant Kill", "instant_kill", CheatKind::Toggle, "enemies"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, target, kind, category))| Cheat {
        uuid: format!("candidate-{index}"),
        name: name.to_owned(),
        category: category.to_owned(),
        kind,
        target: target.to_owned(),
        args: CheatArgs {
            min: None,
            max: None,
            step: None,
            value: None,
            button: None,
        },
        instructions: None,
        parent: None,
        hotkeys: Vec::new(),
    })
    .collect();
    TrainerBlueprint {
        trainer_id: "50696".to_owned(),
        game_id: "105784".to_owned(),
        title_id: "97315".to_owned(),
        supported_versions: Vec::new(),
        backend: Backend::TrainerLib,
        binary_url: None,
        binary_hash: None,
        flags: 0,
        cheats,
        game_event_keys: Vec::new(),
        source: BlueprintSource::Candidate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STICKMAN_ENTRY: &str = r#"{"id":"50696","gameId":"105784","titleId":"97315","releasedAt":"2026-10-22T00:00:00Z","supportedVersions":[2639440150,2419326222],"brokenCheatUuids":null,"blueprint":{"cheats":[{"uuid":"aaa","name":"God Mode","flags":1,"category":"player","type":"toggle","target":"god_mode","hotkeys":[[112]]},{"uuid":"bbb","name":"Game Speed","flags":1,"category":"game","type":"slider","target":"game_speed","hotkeys":[[17,189]],"args":{"min":0.1,"max":5,"step":0.1,"value":1}}],"flags":10,"events":[{"key":"on_death"}]},"loader":"trainerlib","loaderArgs":{"trainerId":"50696","binaryUrl":"https://storage-cdn.wemod.com/trainers/bin/x","binaryHash":"abc"}}"#;

    #[test]
    fn parses_a_cached_trainer_entry_with_backend_and_variables() {
        let blueprint = parse_trainer_entry(STICKMAN_ENTRY).unwrap();
        assert_eq!(blueprint.trainer_id, "50696");
        assert_eq!(blueprint.game_id, "105784");
        assert_eq!(blueprint.title_id, "97315");
        assert_eq!(blueprint.backend, Backend::TrainerLib);
        assert_eq!(blueprint.cheats.len(), 2);
        assert_eq!(blueprint.cheats[0].target, "god_mode");
        assert_eq!(blueprint.cheats[1].kind, CheatKind::Slider);
        assert_eq!(
            blueprint.cheats[1].args.effective_range(),
            (0.1, 5.0, 0.1)
        );
        assert_eq!(
            blueprint.variables(),
            vec!["game_speed".to_owned(), "god_mode".to_owned(), "wm_event_on_death".to_owned()]
        );
        assert!(blueprint.supports_version(2639440150));
        assert!(!blueprint.supports_version(1));
        assert_eq!(blueprint.groups(), vec!["player".to_owned(), "game".to_owned()]);
        assert_eq!(blueprint.binary_hash.as_deref(), Some("abc"));
    }

    #[test]
    fn extracts_balanced_object_from_leveldb_noise() {
        let text = format!("prefix\u{1}\u{2}noise{{{}{}suffix", "x", STICKMAN_ENTRY);
        let object = extract_object_containing(&text, "\"id\":\"50696\"").unwrap();
        assert!(object.starts_with('{'));
        assert!(object.ends_with('}'));
        let blueprint = parse_trainer_entry(&object).unwrap();
        assert_eq!(blueprint.trainer_id, "50696");
    }

    #[test]
    fn extract_handles_braces_and_quotes_inside_strings() {
        let text = r#"{"a":"}{ not structural","id":"7","b":{"c":1}}"#;
        let object = extract_object_containing(text, "\"id\":\"7\"").unwrap();
        assert_eq!(object, text);
    }

    #[test]
    fn candidate_blueprint_is_labelled_and_complete() {
        let blueprint = stickman_candidate();
        assert_eq!(blueprint.source, BlueprintSource::Candidate);
        assert_eq!(blueprint.backend, Backend::TrainerLib);
        assert_eq!(blueprint.cheats.len(), 5);
        assert_eq!(
            blueprint.variables(),
            vec![
                "add_money_when_spend".to_owned(),
                "add_reroll_dice_when_use".to_owned(),
                "ammo_never_decrease".to_owned(),
                "god_mode".to_owned(),
                "instant_kill".to_owned(),
            ]
        );
    }

    #[test]
    fn rejects_entries_without_a_blueprint() {
        assert!(parse_trainer_entry(r#"{"id":"1","gameId":"2","titleId":"3"}"#).is_err());
        assert!(parse_trainer_entry("not json").is_err());
    }

    #[test]
    fn missing_cache_directory_is_not_an_error() {
        let missing = std::env::temp_dir().join("wand-rebuild-no-such-leveldb");
        assert!(load_cached(&missing, "50696").unwrap().is_none());
        assert!(
            load_cached_all_for_title(&missing, "97315")
                .unwrap()
                .is_empty()
        );
    }

    fn versioned(trainer_id: &str, supported: &[u32]) -> TrainerBlueprint {
        let mut blueprint = stickman_candidate();
        blueprint.trainer_id = trainer_id.to_owned();
        blueprint.supported_versions = supported.to_vec();
        blueprint
    }

    #[test]
    fn selects_the_trainer_matching_the_installed_game_version() {
        // Real shape observed on this machine: an older cached trainer for build
        // 2419326222 and the current one for 2639440150.
        let candidates = vec![
            versioned("50380", &[2419326222]),
            versioned("50696", &[2639440150, 2419326222]),
        ];
        let chosen = select_for_version(candidates.clone(), 2639440150).unwrap();
        assert_eq!(chosen.trainer_id, "50696");
        // A build only the old trainer lists must select the old trainer.
        let chosen = select_for_version(candidates, 2419326222).unwrap();
        assert_eq!(chosen.trainer_id, "50696", "newest trainer supporting the build wins");
    }

    #[test]
    fn falls_back_to_the_newest_supported_version_then_newest_id() {
        let candidates = vec![
            versioned("100", &[10]),
            versioned("200", &[30]),
            versioned("300", &[20]),
        ];
        assert_eq!(
            select_for_version(candidates, 999).unwrap().trainer_id,
            "200"
        );
        assert!(select_for_version(Vec::new(), 1).is_none());
    }
}
