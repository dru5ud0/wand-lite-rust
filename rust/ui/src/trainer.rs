//! Native trainer panel: resolves a game's trainer, owns the live session and
//! projects its state into the Slint model.

use crate::app::TrainerMod;
use crate::mod_view;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Instant;
use wand_rebuild_core::game_locator::{self, GameInstall, RunningProcess};
use wand_rebuild_core::hotkeys::{HotkeyBinding, HotkeyEvent, HotkeyService};
use wand_rebuild_core::process_monitor;
use wand_rebuild_core::runtime::WandRuntime;
use wand_rebuild_core::trainer_blueprint::{self, BlueprintSource, TrainerBlueprint};
use wand_rebuild_core::trainer_cache;
use wand_rebuild_core::trainer_session::{SessionConfig, SessionEvent, TrainerSession};

/// Appends one line to `%LOCALAPPDATA%\WandRebuild\trainer.log`.
///
/// Kept deliberately simple: the panel produces a handful of events per session,
/// so opening the file per event is cheaper than owning a writer. This exists so
/// a manual test run leaves a reviewable trace.
fn append_session_log(line: &str) {
    use std::io::Write;
    let Ok(local) = std::env::var("LOCALAPPDATA") else {
        return;
    };
    let directory = PathBuf::from(local).join("WandRebuild");
    let _ = std::fs::create_dir_all(&directory);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("trainer.log"))
    {
        let _ = writeln!(file, "{stamp} {line}");
    }
}

/// A title whose trainer process mapping is verified end-to-end.
struct VerifiedTitle {
    steam_app_id: u64,
    trainer_id: u64,
    executable: &'static str,
}

/// Only titles proven to work are listed here. Everything else is resolved from
/// Wand's own cached blueprint, so the architecture stays blueprint-driven
/// rather than hardcoding game behaviour.
const VERIFIED_TITLES: &[VerifiedTitle] = &[VerifiedTitle {
    steam_app_id: 2085540,
    trainer_id: 50696,
    executable: "Stick It To The Stickman.exe",
}];

#[derive(Debug, Clone)]
pub struct ModRow {
    pub name: String,
    pub category: String,
    pub target: String,
    pub enabled: bool,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub kind: String,
    pub status: String,
    pub hotkey: String,
}

pub struct TrainerPanel {
    session: Option<TrainerSession>,
    blueprint: Option<TrainerBlueprint>,
    mods: Vec<ModRow>,
    status: String,
    can_start: bool,
    loading: bool,
    ready_at: Option<Instant>,
    resolved_for: Option<u64>,
    install: Option<GameInstall>,
    process: Option<RunningProcess>,
    /// Inputs of the last `resolve`, so process detection can be refreshed
    /// without re-reading Wand's Local Storage.
    request: Option<(u64, Option<String>)>,
    /// User bindings, `trainer target -> "Ctrl+F1"`.
    hotkeys: BTreeMap<String, String>,
    hotkey_service: Option<HotkeyService>,
    hotkey_events: Option<Receiver<HotkeyEvent>>,
    capturing: Option<String>,
    hotkeys_dirty: bool,
    filter: String,
    group: String,
}

impl Default for TrainerPanel {
    fn default() -> Self {
        Self {
            session: None,
            blueprint: None,
            mods: Vec::new(),
            status: "Select an installed game to load its trainer.".to_owned(),
            can_start: false,
            loading: false,
            ready_at: None,
            resolved_for: None,
            install: None,
            process: None,
            request: None,
            hotkeys: BTreeMap::new(),
            hotkey_service: None,
            hotkey_events: None,
            capturing: None,
            hotkeys_dirty: false,
            filter: String::new(),
            group: "All".to_owned(),
        }
    }
}

impl TrainerPanel {
    pub fn status(&self) -> String {
        self.status.clone()
    }

    pub fn can_start(&self) -> bool {
        self.can_start
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    pub fn rows(&self) -> Vec<ModRow> {
        self.visible_rows()
    }

    /// Rows after the filter and group chips, in display order.
    fn visible_rows(&self) -> Vec<ModRow> {
        let group = if self.group.is_empty() || self.group == "All" {
            None
        } else {
            Some(self.group.as_str())
        };
        mod_view::build_list(&self.mods, &self.filter, group)
            .into_iter()
            .filter_map(|entry| match entry {
                mod_view::ModListEntry::Mod { index } => self.mods.get(index).cloned(),
                mod_view::ModListEntry::Group { .. } => None,
            })
            .collect()
    }

    /// Category chips. The panel renders its own "All" chip, so this excludes it.
    pub fn groups(&self) -> Vec<String> {
        mod_view::groups(&self.mods)
    }

    pub fn active_count(&self) -> usize {
        mod_view::active_count(&self.mods)
    }

    pub fn summary(&self) -> String {
        mod_view::summary(&self.mods)
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_owned();
    }

    pub fn set_group(&mut self, group: &str) {
        self.group = group.to_owned();
    }

    pub fn filter(&self) -> String {
        self.filter.clone()
    }

    pub fn group(&self) -> String {
        self.group.clone()
    }

    pub fn set_hotkeys(&mut self, hotkeys: BTreeMap<String, String>) {
        self.hotkeys = hotkeys;
        self.refresh_row_hotkeys();
        self.sync_bindings();
    }

    pub fn hotkeys(&self) -> BTreeMap<String, String> {
        self.hotkeys.clone()
    }

    /// True once after a binding changed, so the caller can persist preferences.
    pub fn take_hotkeys_dirty(&mut self) -> bool {
        std::mem::take(&mut self.hotkeys_dirty)
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing.is_some()
    }

    pub fn capture_target(&self) -> String {
        self.capturing.clone().unwrap_or_default()
    }

    /// Enters capture mode for `target`; the next chord is bound to it.
    pub fn begin_hotkey_capture(&mut self, target: &str) {
        self.ensure_service();
        if let Some(service) = self.hotkey_service.as_ref() {
            service.begin_capture();
            self.capturing = Some(target.to_owned());
            let name = self
                .mods
                .iter()
                .find(|row| row.target == target)
                .map(|row| row.name.clone())
                .unwrap_or_else(|| target.to_owned());
            self.status = format!("Press a key combination for {name}. Escape cancels.");
            append_session_log(&format!("hotkey capture started for {target}"));
        } else {
            self.status = "Global hotkeys are unavailable on this system.".to_owned();
        }
    }

    pub fn cancel_hotkey_capture(&mut self) {
        if let Some(service) = self.hotkey_service.as_ref() {
            service.cancel_capture();
        }
        if self.capturing.take().is_some() {
            self.status = "Hotkey change cancelled.".to_owned();
        }
    }

    fn ensure_service(&mut self) {
        if self.hotkey_service.is_some() {
            return;
        }
        match HotkeyService::start() {
            Ok((service, events)) => {
                self.hotkey_service = Some(service);
                self.hotkey_events = Some(events);
                self.sync_bindings();
            }
            Err(error) => {
                append_session_log(&format!("hotkey service failed to start: {error}"));
            }
        }
    }

    fn sync_bindings(&self) {
        let Some(service) = self.hotkey_service.as_ref() else {
            return;
        };
        let bindings: Vec<(String, HotkeyBinding)> = self
            .hotkeys
            .iter()
            .filter_map(|(target, label)| {
                HotkeyBinding::parse(label).map(|binding| (target.clone(), binding))
            })
            .collect();
        service.set_bindings(bindings);
    }

    fn refresh_row_hotkeys(&mut self) {
        for row in &mut self.mods {
            if let Some(label) = self.hotkeys.get(&row.target) {
                row.hotkey = label.clone();
            }
        }
    }

    /// Binds `binding` to `target`, clearing it from any other target.
    fn assign_hotkey(&mut self, target: &str, binding: HotkeyBinding) {
        if !binding.is_valid() {
            self.status = "That is a modifier-only chord; pick a key to go with it.".to_owned();
            return;
        }
        let label = binding.label();
        let previous_owner = self
            .hotkeys
            .iter()
            .find(|(other, value)| {
                *other != target && HotkeyBinding::parse(value) == Some(binding)
            })
            .map(|(other, _)| other.clone());
        if let Some(owner) = previous_owner {
            self.hotkeys.remove(&owner);
            if let Some(row) = self.mods.iter_mut().find(|row| row.target == owner) {
                row.hotkey.clear();
            }
        }
        self.hotkeys.insert(target.to_owned(), label.clone());
        self.hotkeys_dirty = true;
        if let Some(row) = self.mods.iter_mut().find(|row| row.target == target) {
            row.hotkey = label.clone();
        }
        self.sync_bindings();
        let name = self
            .mods
            .iter()
            .find(|row| row.target == target)
            .map(|row| row.name.clone())
            .unwrap_or_else(|| target.to_owned());
        self.status = format!("{name} is now bound to {label}.");
        append_session_log(&format!("hotkey bound {target} -> {label}"));
    }

    /// Drains global-hotkey events. Returns true when the UI needs refreshing.
    fn poll_hotkeys(&mut self) -> bool {
        let mut changed = false;
        let Some(events) = self.hotkey_events.as_ref() else {
            return false;
        };
        let mut drained = Vec::new();
        while let Ok(event) = events.try_recv() {
            drained.push(event);
        }
        for event in drained {
            changed = true;
            append_session_log(&format!("hotkey event {event:?}"));
            match event {
                HotkeyEvent::Pressed { target } => {
                    let enabled = self
                        .mods
                        .iter()
                        .find(|row| row.target == target)
                        .map(|row| row.enabled)
                        .unwrap_or(false);
                    self.toggle(&target, !enabled);
                }
                HotkeyEvent::Captured { binding } => {
                    if let Some(target) = self.capturing.take() {
                        self.assign_hotkey(&target, binding);
                    }
                }
                HotkeyEvent::CaptureCancelled => {
                    self.capturing = None;
                    self.status = "Hotkey change cancelled.".to_owned();
                }
            }
        }
        changed
    }

    /// Slint model for the current modification list.
    pub fn model(&self) -> Vec<TrainerMod> {
        self.visible_rows()
            .iter()
            .map(|row| TrainerMod {
                name: row.name.clone().into(),
                category: row.category.clone().into(),
                target: row.target.clone().into(),
                enabled: row.enabled,
                value: row.value as f32,
                min: row.min as f32,
                max: row.max as f32,
                step: row.step as f32,
                kind: row.kind.clone().into(),
                status: row.status.clone().into(),
                hotkey: row.hotkey.clone().into(),
            })
            .collect()
    }

    /// Resolves the trainer for `steam_app_id` without starting anything.
    ///
    /// Resolution is generic: the Steam install directory locates the running
    /// process, and Wand's own cached blueprint (matched by `title_id`) supplies
    /// the trainer id, backend and modification list. `title_id` comes from the
    /// public catalog. Only when no cached blueprint exists does the build fall
    /// back to the verified candidate set for the one title proven live.
    pub fn resolve(
        &mut self,
        steam_app_id: u64,
        title_id: Option<&str>,
        steam_root: Option<&Path>,
        app_data: &Path,
        local_app_data: &Path,
    ) {
        if self.resolved_for == Some(steam_app_id) || self.session.is_some() {
            return;
        }
        self.resolved_for = Some(steam_app_id);
        self.request = Some((steam_app_id, title_id.map(str::to_owned)));
        self.blueprint = None;
        self.mods.clear();
        self.can_start = false;
        self.install = None;
        self.process = None;

        // An adopted (debloated) backend wins over a live Wand installation.
        let Some(runtime) = wand_rebuild_core::backend_store::resolve_runtime(local_app_data)
            .ok()
            .flatten()
        else {
            self.status =
                "No Wand runtime was found. Install Wand once, or run the debloat installer to adopt an existing backend."
                    .to_owned();
            return;
        };
        let _ = runtime;
        let storage = wand_rebuild_core::backend_store::resolve_blueprints_dir(app_data, local_app_data);

        // 1. Locate the install directory and the running game process.
        let install = steam_root
            .and_then(|root| {
                game_locator::installed_games(root)
                    .into_iter()
                    .find(|install| install.steam_app_id == steam_app_id)
            });
        let (install, process) = match install {
            Some(install) => {
                let running = game_locator::processes_under(&install.install_dir);
                let picked = game_locator::pick_main_process(&install, &running);
                (Some(install), picked)
            }
            None => {
                // Non-Steam or undetected install: fall back to the verified
                // executable name for the one title proven live.
                let picked = VERIFIED_TITLES
                    .iter()
                    .find(|title| title.steam_app_id == steam_app_id)
                    .and_then(|title| process_monitor::find_process(title.executable))
                    .map(|process| RunningProcess {
                        process_id: process.process_id,
                        executable: process.executable,
                        image_path: process.image_path,
                    });
                (None, picked)
            }
        };
        self.install = install;
        self.process = process;

        // 2. Resolve the blueprint. Wand can cache several trainers for one
        // title (the previous build survives a game update), so select by the
        // installed game version instead of taking the first hit.
        let cached_blueprint = title_id
            .and_then(|title_id| {
                let candidates = trainer_blueprint::load_cached_all_for_title(&storage, title_id)
                    .ok()
                    .unwrap_or_default();
                if candidates.is_empty() {
                    return None;
                }
                // The game version lives on the game record, which needs the game
                // id; take it from any candidate.
                let game_id = candidates[0].game_id.clone();
                let installed = process_monitor::cached_game_version(&storage, &game_id);
                let version = installed
                    .or_else(|| {
                        candidates
                            .iter()
                            .filter_map(TrainerBlueprint::preferred_game_version)
                            .max()
                    })
                    .unwrap_or_default();
                trainer_blueprint::select_for_version(candidates, version)
            })
            .or_else(|| {
                VERIFIED_TITLES
                    .iter()
                    .find(|title| title.steam_app_id == steam_app_id)
                    .and_then(|title| {
                        trainer_blueprint::load_cached(&storage, &title.trainer_id.to_string())
                            .ok()
                            .flatten()
                    })
            })
            .or_else(|| {
                VERIFIED_TITLES
                    .iter()
                    .any(|title| title.steam_app_id == steam_app_id)
                    .then(trainer_blueprint::stickman_candidate)
            });

        let Some(blueprint) = cached_blueprint else {
            self.status =
                "No trainer blueprint available for this game yet. Open it once in Wand to cache it."
                    .to_owned();
            return;
        };
        let trainer_id = blueprint.trainer_id.parse::<u64>().unwrap_or(0);
        let Some(trainer_root) =
            wand_rebuild_core::backend_store::resolve_trainers_dir(app_data, local_app_data)
        else {
            self.status = "No trainer cache was found. Open this game once in Wand so its trainer downloads."
                .to_owned();
            return;
        };
        let cached_trainer = trainer_cache::find(&trainer_root, trainer_id).ok().flatten();
        let source = match blueprint.source {
            BlueprintSource::WandCache => "Wand cache",
            BlueprintSource::Candidate => "validated candidate",
        };
        self.blueprint = Some(blueprint);
        self.can_start = cached_trainer.is_some() && self.process.is_some();
        let executable = self
            .process
            .as_ref()
            .map(|process| process.executable.clone())
            .or_else(|| {
                self.install
                    .as_ref()
                    .and_then(|install| install.executable_hint.clone())
            })
            .unwrap_or_else(|| "the game".to_owned());
        self.status = match (&cached_trainer, &self.process) {
            (None, _) => format!(
                "Trainer {trainer_id} is not cached ({source}). Open Wand once to download it."
            ),
            (Some(_), None) => format!("Start {executable} to load its trainer."),
            (Some(_), Some(process)) => format!(
                "Trainer {trainer_id} ready ({source}) · {} detected (pid {})",
                process.executable, process.process_id
            ),
        };
    }

    /// Starts the legacy trainer session for the resolved game.
    pub fn start(&mut self, app_data: &Path, local_app_data: &Path) -> Result<(), String> {
        append_session_log("start requested");
        if self.session.is_some() {
            append_session_log("start ignored: a session is already active");
            return Ok(());
        }
        let Some(blueprint) = self.blueprint.clone() else {
            append_session_log("start failed: no blueprint resolved");
            return Err(
                "No trainer is resolved for this game yet. Open it once in Wand so its trainer is cached, then reselect it."
                    .to_owned(),
            );
        };
        let runtime: WandRuntime = wand_rebuild_core::backend_store::resolve_runtime(local_app_data)
            .map_err(|error| error.to_string())?
            .ok_or("No Wand runtime was found. Install Wand once, or run the debloat installer to adopt an existing backend.")?;
        let trainer_root = wand_rebuild_core::backend_store::resolve_trainers_dir(app_data, local_app_data)
            .ok_or("No trainer cache was found. Open this game once in Wand so its trainer downloads.")?;
        let cached = trainer_cache::find(&trainer_root, blueprint.trainer_id.parse().unwrap_or(0))
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                append_session_log("start failed: trainer binary not cached");
                format!(
                    "Trainer {} is not downloaded yet. Open Wand once and view this game's trainer, then retry.",
                    blueprint.trainer_id
                )
            })?;

        // Re-detect the process at start time: the user may have launched the
        // game after selecting it in the library.
        let process = match self.detect_process() {
            Some(process) => process,
            None => {
                append_session_log("start failed: game process not found");
                let name = self
                    .install
                    .as_ref()
                    .map(|install| install.name.clone())
                    .or_else(|| {
                        self.request
                            .as_ref()
                            .and_then(|(art_id, _)| {
                                VERIFIED_TITLES
                                    .iter()
                                    .find(|title| title.steam_app_id == *art_id)
                                    .map(|title| title.executable.to_owned())
                            })
                    })
                    .unwrap_or_else(|| "the game".to_owned());
                return Err(format!(
                    "{name} is not running. Start the game and get into gameplay, then press Load trainer again."
                ));
            }
        };
        self.process = Some(process.clone());
        append_session_log(&format!(
            "start: pid={} exe={} trainer={}",
            process.process_id, process.executable, blueprint.trainer_id
        ));
        let storage = wand_rebuild_core::backend_store::resolve_blueprints_dir(app_data, local_app_data);
        let game_version = process_monitor::cached_game_version(&storage, &blueprint.game_id)
            .or_else(|| blueprint.preferred_game_version())
            .unwrap_or_default();

        let config = SessionConfig {
            trainer_library_directory: runtime.trainer_directory,
            trainer_dll: cached.path,
            target_process_id: process.process_id,
            game_version,
            architecture:
                wand_rebuild_core::trainer_session::Architecture::of_process(process.process_id),
            flags: 0,
            variables: blueprint.variables(),
        };
        self.loading = true;
        self.ensure_service();
        self.mods = blueprint
            .cheats
            .iter()
            .map(|cheat| {
                let (min, max, step) = cheat.args.effective_range();
                // A user binding wins over the blueprint default.
                let hotkey = self
                    .hotkeys
                    .get(&cheat.target)
                    .cloned()
                    .or_else(|| {
                        cheat
                            .hotkeys
                            .first()
                            .and_then(|chord| binding_from_chord(chord))
                            .map(|binding| binding.label())
                    })
                    .unwrap_or_default();
                ModRow {
                    name: cheat.name.clone(),
                    category: cheat.category.clone(),
                    target: cheat.target.clone(),
                    enabled: false,
                    value: cheat.initial_value(),
                    min,
                    max,
                    step,
                    kind: format!("{:?}", cheat.kind).to_lowercase(),
                    status: String::new(),
                    hotkey,
                }
            })
            .collect();
        // Seed blueprint defaults into the binding map so they are active and
        // persisted like user bindings.
        for row in &self.mods {
            if !row.hotkey.is_empty() {
                self.hotkeys
                    .entry(row.target.clone())
                    .or_insert_with(|| row.hotkey.clone());
            }
        }
        self.sync_bindings();
        self.status = "Starting trainer session...".to_owned();
        let session = TrainerSession::start(blueprint, config)
            .map_err(|error| format!("Could not start the trainer: {error}"))?;
        self.session = Some(session);
        self.can_start = false;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.stop();
        }
        self.mods.clear();
        self.loading = false;
        self.can_start = false;
        self.ready_at = None;
        self.resolved_for = None;
        self.install = None;
        self.process = None;
        self.status = "Trainer unloaded.".to_owned();
    }

    /// Clears the panel when the selection is not an installed game, so a stale
    /// trainer can never be started for a different title.
    pub fn reset(&mut self) {
        self.stop();
        self.blueprint = None;
        self.resolved_for = None;
        self.request = None;
        self.status = "Select an installed game to load its trainer.".to_owned();
    }

    /// Cheap process detection using the install directory captured by
    /// [`TrainerPanel::resolve`]. Falls back to the verified executable name when
    /// no Steam install directory was resolved.
    fn detect_process(&self) -> Option<RunningProcess> {
        if let Some(install) = self.install.as_ref() {
            let running = game_locator::processes_under(&install.install_dir);
            if let Some(process) = game_locator::pick_main_process(install, &running) {
                return Some(process);
            }
        }
        let (steam_app_id, _) = self.request.as_ref()?;
        VERIFIED_TITLES
            .iter()
            .find(|title| title.steam_app_id == *steam_app_id)
            .and_then(|title| process_monitor::find_process(title.executable))
            .map(|process| RunningProcess {
                process_id: process.process_id,
                executable: process.executable,
                image_path: process.image_path,
            })
    }

    /// Re-detects the game process so the Load button reflects the current
    /// state. Returns true when anything the UI displays changed.
    ///
    /// This exists because a user typically selects a game *before* launching
    /// it; without a refresh the button would stay disabled until they reselect.
    pub fn refresh_process(&mut self) -> bool {
        if self.session.is_some() || self.loading || self.blueprint.is_none() {
            return false;
        }
        let previous = self.process.as_ref().map(|process| process.process_id);
        let detected = self.detect_process();
        let current = detected.as_ref().map(|process| process.process_id);
        if previous == current {
            return false;
        }
        self.process = detected;
        let cached = self
            .blueprint
            .as_ref()
            .map(|blueprint| blueprint.trainer_id.clone())
            .unwrap_or_default();
        self.can_start = self.process.is_some();
        self.status = match &self.process {
            Some(process) => format!(
                "Trainer {cached} ready · {} detected (pid {})",
                process.executable, process.process_id
            ),
            None => {
                let name = self
                    .install
                    .as_ref()
                    .map(|install| install.name.clone())
                    .or_else(|| {
                        self.request.as_ref().and_then(|(art_id, _)| {
                            VERIFIED_TITLES
                                .iter()
                                .find(|title| title.steam_app_id == *art_id)
                                .map(|title| title.executable.to_owned())
                        })
                    })
                    .unwrap_or_else(|| "the game".to_owned());
                format!("Start {name} to load its trainer.")
            }
        };
        append_session_log(&format!("process-detection changed: {current:?}"));
        true
    }

    /// Sends a modification change and updates the local row optimistically; the
    /// trainer's own `ValueResponse`/log confirms or contradicts it.
    pub fn toggle(&mut self, target: &str, enabled: bool) {
        let value = {
            let Some(blueprint) = self.blueprint.as_ref() else {
                return;
            };
            let Some(cheat) = blueprint.cheat_for_target(target) else {
                return;
            };
            if enabled {
                cheat.enabled_value()
            } else {
                0.0
            }
        };
        match self.send_value_request(target, value, enabled) {
            Ok(()) => {
                let name = self
                    .blueprint
                    .as_ref()
                    .and_then(|blueprint| blueprint.cheat_for_target(target))
                    .map(|cheat| cheat.name.clone())
                    .unwrap_or_default();
                self.status = format!(
                    "{} {}",
                    name,
                    if enabled { "enabled" } else { "disabled" }
                );
            }
            Err(_) => {}
        }
    }

    /// Sets a numeric/slider modification to `value`, clamped and snapped to the
    /// cheat's effective range, and updates the local row optimistically.
    pub fn set_value(&mut self, target: &str, value: f64) {
        let Some(cheat) = self
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.cheat_for_target(target))
        else {
            return;
        };
        let (min, max, step) = cheat.args.effective_range();
        let value = clamp_to_step(value, min, max, step);
        let name = cheat.name.clone();
        let enabled = value != 0.0;
        match self.send_value_request(target, value, enabled) {
            Ok(()) => {
                self.status = format!("{} set to {}", name, value);
            }
            Err(_) => {}
        }
    }

    /// Shared request-id/confirmation flow used by both `toggle` and `set_value`.
    fn send_value_request(
        &mut self,
        target: &str,
        value: f64,
        enabled: bool,
    ) -> Result<(), String> {
        let session = self.session.as_mut().ok_or_else(|| "no active session".to_owned())?;
        let blueprint = self.blueprint.as_ref().ok_or_else(|| "no blueprint".to_owned())?;
        let cheat = blueprint
            .cheat_for_target(target)
            .ok_or_else(|| "unknown modification".to_owned())?;
        let cheat_name = cheat.name.clone();
        match session.set_value(target, value) {
            Ok(request_id) => {
                append_session_log(&format!(
                    "user set target={target} value={value} req={request_id}"
                ));
                if let Some(row) = self.mods.iter_mut().find(|row| row.target == target) {
                    row.enabled = enabled;
                    row.value = value;
                    row.status = format!("request {request_id}");
                }
                Ok(())
            }
            Err(error) => {
                self.status = format!("Could not change {}: {error}", cheat_name);
                Err(error.to_string())
            }
        }
    }

    /// Drains session events. Returns true when the UI model needs refreshing.
    pub fn poll(&mut self) -> bool {
        // Global hotkeys are drained even while the panel is idle so capture
        // mode and bindings stay responsive.
        let mut changed = self.poll_hotkeys();
        let Some(session) = self.session.as_mut() else {
            return changed;
        };
        for event in session.poll() {
            changed = true;
            match event {
                SessionEvent::PipeConnected => {
                    self.status = "Trainer runtime connected.".to_owned();
                    append_session_log("event pipe-connected");
                }
                SessionEvent::Ready => {
                    self.loading = false;
                    self.ready_at = Some(Instant::now());
                    self.status = "Trainer active. Modifications are live.".to_owned();
                    append_session_log("event ready");
                }
                SessionEvent::ValueResult { request_id, result } => {
                    append_session_log(&format!("event value-result req={request_id} result={result}"));
                    if let Some(row) = self
                        .mods
                        .iter_mut()
                        .find(|row| row.status == format!("request {request_id}"))
                    {
                        row.status = if result == 0 {
                            "confirmed".to_owned()
                        } else {
                            format!("rejected ({result})")
                        };
                    }
                }
                SessionEvent::Value { name, value } => {
                    append_session_log(&format!("event value name={name} value={value}"));
                    if let Some(row) = self.mods.iter_mut().find(|row| row.target == name) {
                        row.value = value;
                        row.enabled = value != 0.0;
                    }
                }
                SessionEvent::Log(message) => {
                    append_session_log(&format!(
                        "trainer-log level={} {}",
                        message.level,
                        message.message.trim_end()
                    ));
                    if message.is_problem() {
                        self.status = message.message.trim_end().to_owned();
                    }
                }
                SessionEvent::Ended(reason) => {
                    append_session_log(&format!("event ended {reason}"));
                    self.mods.clear();
                    self.session = None;
                    self.loading = false;
                    self.ready_at = None;
                    self.status = format!("Trainer ended: {reason}");
                    break;
                }
                SessionEvent::Failed(reason) => {
                    append_session_log(&format!("event failed {reason}"));
                    self.mods.clear();
                    self.session = None;
                    self.loading = false;
                    self.ready_at = None;
                    self.status = format!("Trainer failed: {reason}");
                    break;
                }
            }
        }
        changed
    }

    /// True once the trainer has reported `Ready`.
    pub fn is_ready(&self) -> bool {
        self.ready_at.is_some()
    }

    pub fn blueprint_source(&self) -> Option<BlueprintSource> {
        self.blueprint.as_ref().map(|blueprint| blueprint.source)
    }

    pub fn trainer_id(&self) -> Option<u64> {
        self.blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.trainer_id.parse().ok())
    }

    /// Converts a single hotkey chord (Windows virtual-key codes) into a human
    /// readable label. Delegates to the module-level helper.
    #[allow(dead_code)]
    pub fn hotkey_label(chord: &[u32]) -> String {
        crate::trainer::hotkey_label(chord)
    }

    /// Clamps `value` to `[min, max]` and snaps it to the nearest `step`.
    #[allow(dead_code)]
    pub fn clamp_to_step(value: f64, min: f64, max: f64, step: f64) -> f64 {
        crate::trainer::clamp_to_step(value, min, max, step)
    }

    pub fn paths() -> (PathBuf, PathBuf) {
        (
            std::env::var("APPDATA").map(PathBuf::from).unwrap_or_default(),
            std::env::var("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_default(),
        )
    }
}

/// Converts a single hotkey chord (Windows virtual-key codes) into a human
/// readable label such as `F1`, `Ctrl+-` or `Shift+F3`.
/// Converts a blueprint hotkey chord (Windows virtual-key codes) into a binding.
///
/// Modifiers are 16 (Shift), 17 (Ctrl), 18 (Alt); the last non-modifier key is
/// the chord's key. Returns `None` for chords with no key.
pub fn binding_from_chord(chord: &[u32]) -> Option<HotkeyBinding> {
    let mut binding = HotkeyBinding {
        ctrl: false,
        shift: false,
        alt: false,
        vk: 0,
    };
    for &vk in chord {
        match vk {
            16 => binding.shift = true,
            17 => binding.ctrl = true,
            18 => binding.alt = true,
            key => binding.vk = key,
        }
    }
    (binding.vk != 0).then_some(binding)
}

pub fn hotkey_label(chord: &[u32]) -> String {
    let mut modifiers = Vec::new();
    let mut keys = Vec::new();
    for &vk in chord {
        match vk {
            16 => modifiers.push("Shift"),
            17 => modifiers.push("Ctrl"),
            18 => modifiers.push("Alt"),
            32 => keys.push("Space".to_owned()),
            0xBB => keys.push("+".to_owned()),
            0xBD => keys.push("-".to_owned()),
            0xDB => keys.push("[".to_owned()),
            0xDD => keys.push("]".to_owned()),
            v if (0x70..=0x7B).contains(&v) => {
                keys.push(format!("F{}", v - 0x70 + 1));
            }
            v if (0x30..=0x39).contains(&v) => {
                keys.push(format!("{}", v as u8 as char));
            }
            v if (0x41..=0x5A).contains(&v) => {
                keys.push(format!("{}", v as u8 as char));
            }
            v => {
                keys.push(format!("VK{}", v));
            }
        }
    }
    // Stable modifier order: Ctrl, Shift, Alt.
    let mut ordered_mods = Vec::new();
    for name in ["Ctrl", "Shift", "Alt"] {
        if modifiers.contains(&name) {
            ordered_mods.push(name.to_owned());
        }
    }
    let mut parts = ordered_mods;
    parts.extend(keys);
    parts.join("+")
}

/// Clamps `value` to `[min, max]` and snaps it to the nearest `step` multiple.
pub fn clamp_to_step(value: f64, min: f64, max: f64, step: f64) -> f64 {
    let clamped = value.clamp(min, max);
    if !step.is_finite() || step <= 0.0 {
        return clamped;
    }
    let steps = ((clamped - min) / step).round();
    let snapped = min + steps * step;
    snapped.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_label_f1() {
        assert_eq!(hotkey_label(&[112]), "F1");
    }

    #[test]
    fn hotkey_label_ctrl_minus() {
        assert_eq!(hotkey_label(&[17, 189]), "Ctrl+-");
    }

    #[test]
    fn hotkey_label_plain_letters() {
        assert_eq!(hotkey_label(&[65, 66]), "A+B");
        assert_eq!(hotkey_label(&[0x5A]), "Z");
    }

    #[test]
    fn hotkey_label_empty() {
        assert_eq!(hotkey_label(&[]), "");
    }

    #[test]
    fn clamp_to_step_basic() {
        assert_eq!(clamp_to_step(5.05, 0.0, 10.0, 1.0), 5.0);
        assert_eq!(clamp_to_step(10.5, 0.0, 10.0, 0.5), 10.0);
        assert_eq!(clamp_to_step(-3.0, 0.0, 10.0, 1.0), 0.0);
        assert!((clamp_to_step(0.15, 0.1, 5.0, 0.1) - 0.1).abs() < 1e-9); 
    }
}
