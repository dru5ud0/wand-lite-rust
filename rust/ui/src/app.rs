use crate::art;
use crate::trainer::TrainerPanel;
use slint::{Image, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wand_rebuild_core::catalog::{Catalog, CatalogTitle};
use wand_rebuild_core::preferences::Preferences;
use wand_rebuild_core::runtime::WandRuntime;
use wand_rebuild_core::steam;
use wand_rebuild_core::trainer_cache;

slint::include_modules!();

#[derive(Clone)]
struct LibraryGame {
    id: String,
    name: String,
    platform: &'static str,
    art_id: u64,
    launch_uri: String,
    last_played: u64,
}

struct AppState {
    games: Vec<LibraryGame>,
    catalog: Mutex<Option<Catalog>>,
    catalog_loading: AtomicBool,
    preferences: Mutex<Preferences>,
    preferences_path: PathBuf,
    catalog_path: PathBuf,
    art_cache: PathBuf,
    steam_art: PathBuf,
    steam_root: Option<PathBuf>,
    backend: Option<WandRuntime>,
}

pub fn run() -> Result<(), slint::PlatformError> {
    // `--open-game <steam_app_id>` preselects a game (also useful as a deep
    // link), and `--auto-load-trainer` starts its trainer once it is startable.
    let mut open_game: Option<u64> = None;
    let mut auto_load_trainer = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--open-game" => {
                open_game = arguments.next().and_then(|value| value.parse().ok());
            }
            "--auto-load-trainer" => auto_load_trainer = true,
            _ => {}
        }
    }
    let ui = AppWindow::new()?;
    ui.window().set_maximized(true);
    let state = Arc::new(load_state());
    let (app_data, local_app_data) = TrainerPanel::paths();
    let trainer_panel = Rc::new(RefCell::new(TrainerPanel::default()));
    // Restore saved key bindings before anything is rendered.
    let saved_hotkeys = state
        .preferences
        .lock()
        .map(|preferences| preferences.hotkeys.clone())
        .unwrap_or_default();
    trainer_panel.borrow_mut().set_hotkeys(saved_hotkeys);
    ui.set_trainer_session_status("Trainer not loaded".into());
    ui.set_trainer_mods(ModelRc::from(Rc::new(VecModel::from(
        Vec::<TrainerMod>::new(),
    ))));
    let backend_status = state.backend.as_ref().map_or_else(
        || "Wand backend not found".to_owned(),
        |runtime| {
            format!(
                "Installed Wand backend {}",
                runtime
                    .version
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(".")
            )
        },
    );
    ui.set_backend_status(backend_status.into());
    ui.set_sort_descending(
        state
            .preferences
            .lock()
            .map(|p| p.sort_descending)
            .unwrap_or(false),
    );
    ui.set_catalog_ready(state.catalog.lock().is_ok_and(|catalog| catalog.is_some()));
    refresh_view(&ui, &state);

    let weak = ui.as_weak();
    ui.on_minimize_requested(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().set_minimized(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_maximize_requested(move || {
        if let Some(ui) = weak.upgrade() {
            let window = ui.window();
            window.set_maximized(!window.is_maximized());
        }
    });
    let weak = ui.as_weak();
    let state_for_search = Arc::clone(&state);
    ui.on_search_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            refresh_view(&ui, &state_for_search);
        }
    });
    let weak = ui.as_weak();
    let state_for_find = Arc::clone(&state);
    ui.on_find_opened(move || {
        if let Some(ui) = weak.upgrade() {
            let catalog_count = state_for_find
                .catalog
                .lock()
                .ok()
                .and_then(|c| c.as_ref().map(Catalog::len));
            ui.set_search_status(match catalog_count {
                Some(count) => format!("Wand catalog · {count} titles").into(),
                None => "Loading Wand catalog...".into(),
            });
            refresh_view(&ui, &state_for_find);
            if catalog_count.is_none() {
                refresh_catalog_async(&ui, Arc::clone(&state_for_find));
            }
        }
    });
    let weak = ui.as_weak();
    let state_for_view = Arc::clone(&state);
    ui.on_view_changed(move || {
        if let Some(ui) = weak.upgrade() {
            if let Ok(mut prefs) = state_for_view.preferences.lock() {
                prefs.sort_descending = ui.get_sort_descending();
                if let Err(error) = prefs.save(&state_for_view.preferences_path) {
                    eprintln!("Could not save preferences: {error}");
                }
            }
            refresh_view(&ui, &state_for_view);
        }
    });
    let weak = ui.as_weak();
    let state_for_favorite = Arc::clone(&state);
    ui.on_favorite_toggled(move |id| {
        if let Some(ui) = weak.upgrade() {
            let aliases = favorite_aliases(&state_for_favorite, id.as_str());
            if let Ok(mut prefs) = state_for_favorite.preferences.lock() {
                let new_value = !aliases.iter().any(|alias| prefs.favorites.contains(alias));
                for alias in aliases {
                    if new_value {
                        prefs.favorites.insert(alias);
                    } else {
                        prefs.favorites.remove(&alias);
                    }
                }
                if ui.get_selected_id() == id {
                    ui.set_selected_favorite(new_value);
                }
                if let Err(error) = prefs.save(&state_for_favorite.preferences_path) {
                    eprintln!("Could not save preferences: {error}");
                }
            }
            refresh_view(&ui, &state_for_favorite);
        }
    });
    let weak = ui.as_weak();
    let state_for_selection = Arc::clone(&state);
    let panel_for_selection = Rc::clone(&trainer_panel);
    let app_data_for_selection = app_data.clone();
    let local_app_data_for_selection = local_app_data.clone();
    ui.on_game_selected(move |id| {
        if let Some(ui) = weak.upgrade() {
            ui.set_action_status("".into());
            if let Some(game) = state_for_selection
                .games
                .iter()
                .find(|game| game.id == id.as_str())
            {
                ui.set_selected_installed(true);
                ui.set_selected_detail(format!("{} · installed", game.platform).into());
                ui.set_trainer_status(if game.art_id == 2085540 {
                    stickman_cache_status().into()
                } else {
                    "Open the installed Wand app to load this game's trainer.".into()
                });
                let title_id = state_for_selection
                    .catalog
                    .lock()
                    .ok()
                    .and_then(|catalog| {
                        catalog
                            .as_ref()?
                            .find_by_steam_id(game.art_id)
                            .map(|title| title.id.clone())
                    });
                panel_for_selection.borrow_mut().resolve(
                    game.art_id,
                    title_id.as_deref(),
                    state_for_selection.steam_root.as_deref(),
                    &app_data_for_selection,
                    &local_app_data_for_selection,
                );
                push_trainer_ui(&ui, &panel_for_selection.borrow());
            } else {
                ui.set_selected_installed(false);
                // A catalog-only title has no local install or trainer session.
                panel_for_selection.borrow_mut().reset();
                push_trainer_ui(&ui, &panel_for_selection.borrow());
                let detail = state_for_selection
                    .catalog
                    .lock()
                    .ok()
                    .and_then(|catalog| {
                        catalog
                            .as_ref()?
                            .find_by_id(id.as_str().strip_prefix("title:")?)
                            .map(|title| {
                                if title.has_trainer {
                                    "Trainer listed in Wand catalog"
                                } else {
                                    "No trainer listed in Wand catalog"
                                }
                            })
                    })
                    .unwrap_or("Catalog title unavailable");
                ui.set_selected_detail(detail.into());
                ui.set_trainer_status(
                    "Open the installed Wand app to view trainer controls.".into(),
                );
            }
        }
    });
    let weak = ui.as_weak();
    let state_for_launch = Arc::clone(&state);
    ui.on_play_requested(move |id| {
        if let Some(ui) = weak.upgrade() {
            let result = state_for_launch
                .games
                .iter()
                .find(|game| game.id == id.as_str())
                .ok_or_else(|| "This game is not installed in the detected library.".to_owned())
                .and_then(|game| open_shell_target(&game.launch_uri));
            ui.set_action_status(match result {
                Ok(()) => "Launch request sent to the game store.".into(),
                Err(error) => error.into(),
            });
        }
    });
    let weak = ui.as_weak();
    let state_for_wand = Arc::clone(&state);
    ui.on_open_wand_requested(move || {
        if let Some(ui) = weak.upgrade() {
            let result = state_for_wand
                .backend
                .as_ref()
                .ok_or_else(|| "An installed Wand runtime was not found.".to_owned())
                .and_then(|runtime| {
                    std::process::Command::new(runtime.app_directory.join("Wand.exe"))
                        .spawn()
                        .map(|_| ())
                        .map_err(|error| format!("Could not open Wand: {error}"))
                });
            ui.set_action_status(match result {
                Ok(()) => "Wand opened. Select this game there to run its trainer.".into(),
                Err(error) => error.into(),
            });
        }
    });
    let weak = ui.as_weak();
    let panel_for_start = Rc::clone(&trainer_panel);
    let app_data_for_start = app_data.clone();
    let local_app_data_for_start = local_app_data.clone();
    ui.on_trainer_start_requested(move || {
        if let Some(ui) = weak.upgrade() {
            let result = panel_for_start
                .borrow_mut()
                .start(&app_data_for_start, &local_app_data_for_start);
            if let Err(error) = result {
                panel_for_start.borrow_mut().stop();
                ui.set_action_status(error.into());
            }
            push_trainer_ui(&ui, &panel_for_start.borrow());
        }
    });
    let weak = ui.as_weak();
    let panel_for_stop = Rc::clone(&trainer_panel);
    ui.on_trainer_stop_requested(move || {
        if let Some(ui) = weak.upgrade() {
            panel_for_stop.borrow_mut().stop();
            push_trainer_ui(&ui, &panel_for_stop.borrow());
        }
    });
    let weak = ui.as_weak();
    let panel_for_toggle = Rc::clone(&trainer_panel);
    ui.on_mod_toggled(move |target, enabled| {
        if let Some(ui) = weak.upgrade() {
            panel_for_toggle
                .borrow_mut()
                .toggle(target.as_str(), enabled);
            push_trainer_ui(&ui, &panel_for_toggle.borrow());
        }
    });
    let weak = ui.as_weak();
    let panel_for_value = Rc::clone(&trainer_panel);
    ui.on_mod_value_changed(move |target, value| {
        if let Some(ui) = weak.upgrade() {
            panel_for_value
                .borrow_mut()
                .set_value(target.as_str(), value as f64);
            push_trainer_ui(&ui, &panel_for_value.borrow());
        }
    });

    let weak = ui.as_weak();
    let panel_for_hotkey = Rc::clone(&trainer_panel);
    ui.on_hotkey_edit_requested(move |target| {
        if let Some(ui) = weak.upgrade() {
            panel_for_hotkey
                .borrow_mut()
                .begin_hotkey_capture(target.as_str());
            push_trainer_ui(&ui, &panel_for_hotkey.borrow());
        }
    });
    let weak = ui.as_weak();
    let panel_for_cancel = Rc::clone(&trainer_panel);
    ui.on_hotkey_cancel_requested(move || {
        if let Some(ui) = weak.upgrade() {
            panel_for_cancel.borrow_mut().cancel_hotkey_capture();
            push_trainer_ui(&ui, &panel_for_cancel.borrow());
        }
    });
    let weak = ui.as_weak();
    let panel_for_filter = Rc::clone(&trainer_panel);
    ui.on_mod_filter_changed(move |filter| {
        if let Some(ui) = weak.upgrade() {
            panel_for_filter.borrow_mut().set_filter(filter.as_str());
            push_trainer_ui(&ui, &panel_for_filter.borrow());
        }
    });
    let weak = ui.as_weak();
    let panel_for_group = Rc::clone(&trainer_panel);
    ui.on_mod_group_changed(move |group| {
        if let Some(ui) = weak.upgrade() {
            panel_for_group.borrow_mut().set_group(group.as_str());
            push_trainer_ui(&ui, &panel_for_group.borrow());
        }
    });

    // Drain trainer events off the UI thread; the session itself runs on its
    // own thread, so this timer only copies state.
    let weak = ui.as_weak();
    let panel_for_timer = Rc::clone(&trainer_panel);
    let state_for_hotkeys = Arc::clone(&state);
    let trainer_timer = slint::Timer::default();
    let mut refresh_tick: u32 = 0;
    trainer_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(120),
        move || {
            if let Some(ui) = weak.upgrade() {
                let mut changed = panel_for_timer.borrow_mut().poll();
                // Re-detect the game process about once a second so the Load
                // trainer button enables when the user launches the game after
                // selecting it.
                refresh_tick = refresh_tick.wrapping_add(1);
                if refresh_tick % 8 == 0 && panel_for_timer.borrow_mut().refresh_process() {
                    changed = true;
                }
                if changed {
                    push_trainer_ui(&ui, &panel_for_timer.borrow());
                }
                // Persist bindings changed by capture mode.
                if panel_for_timer.borrow_mut().take_hotkeys_dirty() {
                    let hotkeys = panel_for_timer.borrow().hotkeys();
                    if let Ok(mut preferences) = state_for_hotkeys.preferences.lock() {
                        preferences.hotkeys = hotkeys;
                        if let Err(error) =
                            preferences.save(&state_for_hotkeys.preferences_path)
                        {
                            eprintln!("Could not save hotkeys: {error}");
                        }
                    }
                }
            }
        },
    );

    refresh_catalog_async(&ui, Arc::clone(&state));

    // Optional deep link / preview: select a game and optionally start its trainer.
    if let Some(app_id) = open_game {
        if let Some(game) = state.games.iter().find(|game| game.art_id == app_id) {
            ui.set_selected_id(game.id.clone().into());
            ui.set_selected_game(game.name.clone().into());
            ui.set_selected_favorite(
                state
                    .preferences
                    .lock()
                    .map(|preferences| preferences.favorites.contains(&game.id))
                    .unwrap_or(false),
            );
            ui.set_show_detail(true);
            ui.invoke_game_selected(game.id.clone().into());
        }
    }
    // Kept alive for the lifetime of the event loop.
    let mut auto_timer_guard: Option<slint::Timer> = None;
    if auto_load_trainer {
        let auto = Rc::new(std::cell::Cell::new(true));
        let weak = ui.as_weak();
        let panel_for_auto = Rc::clone(&trainer_panel);
        let auto_timer = slint::Timer::default();
        auto_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            move || {
                if !auto.get() {
                    return;
                }
                if panel_for_auto.borrow().can_start() {
                    auto.set(false);
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_trainer_start_requested();
                    }
                }
            },
        );
        auto_timer_guard = Some(auto_timer);
    }
    let _ = &auto_timer_guard;
    ui.run()
}

fn push_trainer_ui(ui: &AppWindow, panel: &TrainerPanel) {
    ui.set_trainer_session_status(panel.status().into());
    ui.set_trainer_can_start(panel.can_start());
    ui.set_trainer_loading(panel.loading());
    ui.set_trainer_active(panel.is_active());
    ui.set_trainer_mods(ModelRc::from(Rc::new(VecModel::from(panel.model()))));
    ui.set_mod_groups(ModelRc::from(Rc::new(VecModel::from(
        panel
            .groups()
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))));
    ui.set_mod_filter(panel.filter().into());
    ui.set_mod_group(panel.group().into());
    ui.set_active_mod_count(panel.active_count() as i32);
    ui.set_hotkey_capturing(panel.is_capturing());
    ui.set_hotkey_capture_target(panel.capture_target().into());
}

/// Headless exercise of the exact panel path the UI uses: resolve, start, wait
/// for ready, toggle a modification, unload. Exits 0 only if the trainer
/// reported ready and every modification request was acknowledged.
pub fn trainer_self_test() -> i32 {
    use std::time::{Duration, Instant};

    // The self-test synthesises keystrokes, which the global hook filters by
    // default. Opt in before the service starts.
    #[cfg(windows)]
    unsafe {
        std::env::set_var("WAND_HOTKEYS_ALLOW_INJECTED", "1");
    }

    let (app_data, local_app_data) = TrainerPanel::paths();
    let mut panel = TrainerPanel::default();
    let steam_root = std::env::var_os("PROGRAMFILES(X86)")
        .or_else(|| std::env::var_os("PROGRAMFILES"))
        .map(|folder| PathBuf::from(folder).join("Steam"));
    panel.resolve(
        2085540,
        Some("97315"),
        steam_root.as_deref(),
        &app_data,
        &local_app_data,
    );
    println!("resolve: {}", panel.status());
    if !panel.can_start() {
        println!("cannot start: {}", panel.status());
        return 2;
    }
    println!(
        "blueprint: trainer {:?} source {:?} mods {}",
        panel.trainer_id(),
        panel.blueprint_source(),
        panel.rows().len()
    );
    if let Err(error) = panel.start(&app_data, &local_app_data) {
        println!("start failed: {error}");
        return 3;
    }

    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline && !panel.is_ready() {
        panel.poll();
        // Stop as soon as the session reports failure instead of waiting out the
        // full deadline; `start` cleared the session and the loading flag.
        if !panel.is_active() && !panel.loading() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !panel.is_ready() {
        println!("trainer never became ready: {}", panel.status());
        panel.stop();
        return 4;
    }
    println!("ready: {}", panel.status());
    println!("summary: {}", panel.summary());
    for row in panel.rows() {
        println!(
            "  mod {:<28} target={:<28} kind={:<7} hotkey={:<10} pill={}",
            row.name,
            row.target,
            row.kind,
            row.hotkey,
            crate::mod_view::status_pill(&row.status)
        );
    }
    // Optional settle window so a benchmark can sample memory with the session
    // live and the trainer idle.
    if let Ok(hold) = std::env::var("WANDR_SELFTEST_HOLD_MS") {
        if let Ok(milliseconds) = hold.parse::<u64>() {
            let until = Instant::now() + Duration::from_millis(milliseconds.min(120_000));
            while Instant::now() < until {
                panel.poll();
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    let targets: Vec<String> = panel.rows().iter().map(|row| row.target.clone()).collect();
    let mut failures = 0;
    for target in &targets {
        panel.toggle(target, true);
        std::thread::sleep(Duration::from_millis(250));
        panel.poll();
        panel.toggle(target, false);
        std::thread::sleep(Duration::from_millis(250));
        panel.poll();
        let confirmed = panel
            .rows()
            .iter()
            .find(|row| &row.target == target)
            .is_some_and(|row| row.status == "confirmed");
        println!(
            "  toggle {target:<28} {}",
            if confirmed { "confirmed" } else { "NOT CONFIRMED" }
        );
        if !confirmed {
            failures += 1;
        }
    }
    panel.stop();
    println!("stopped: {}", panel.status());

    // Hotkey rebinding: capture a synthesised Ctrl+F9 for the first mod and
    // confirm the panel stored it. Runs after the session stops so it also
    // covers capture outside an active session.
    let hotkey_bound = {
        let target = targets.first().cloned().unwrap_or_else(|| "god_mode".to_owned());
        panel.begin_hotkey_capture(&target);
        std::thread::sleep(Duration::from_millis(400));
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::keybd_event;
            // SAFETY: documented user32 input injection; no pointers involved.
            unsafe {
                keybd_event(0x11, 0, 0, 0);
                std::thread::sleep(Duration::from_millis(60));
                keybd_event(0x78, 0, 0, 0);
                std::thread::sleep(Duration::from_millis(60));
                keybd_event(0x78, 0, 2, 0);
                std::thread::sleep(Duration::from_millis(60));
                keybd_event(0x11, 0, 2, 0);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            panel.poll();
            if panel.hotkeys().get(&target).map(String::as_str) == Some("Ctrl+F9") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let bound = panel.hotkeys().get(&target).map(String::as_str) == Some("Ctrl+F9");
        println!(
            "  hotkey {target:<28} {}",
            if bound { "rebound to Ctrl+F9" } else { "REBIND FAILED" }
        );
        panel.cancel_hotkey_capture();
        bound
    };

    if failures == 0 && hotkey_bound {
        println!("trainer self-test PASSED");
        0
    } else if failures == 0 {
        println!("trainer self-test FAILED: hotkey rebinding did not take effect");
        6
    } else {
        println!("trainer self-test FAILED: {failures} unconfirmed toggles");
        5
    }
}

pub fn self_check() -> i32 {
    let state = load_state();
    println!("Wand Lite Rust self-check");
    println!("Installed games: {}", state.games.len());
    println!(
        "Wand runtime: {}",
        state
            .backend
            .as_ref()
            .map(|runtime| runtime.app_directory.display().to_string())
            .unwrap_or_else(|| "unavailable".to_owned())
    );
    match std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        Some(local) => match wand_rebuild_core::backend_store::find(&local) {
            Some(adopted) => println!(
                "Backend source: adopted {} ({} trainers cached)",
                adopted
                    .version
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("."),
                adopted.trainer_count()
            ),
            None => println!("Backend source: installed Wand application"),
        },
        None => println!("Backend source: unknown (LOCALAPPDATA is not set)"),
    }
    let cached_count = state
        .catalog
        .lock()
        .ok()
        .and_then(|catalog| catalog.as_ref().map(Catalog::len));
    println!(
        "Cached catalog titles: {}",
        cached_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "none".to_owned())
    );
    match Catalog::fetch_live_bytes()
        .and_then(|bytes| Catalog::parse(&bytes).map_err(|error| error.to_string()))
    {
        Ok(catalog) => {
            println!("Live catalog titles: {}", catalog.len());
            let installed_supported = state
                .games
                .iter()
                .filter(|game| {
                    catalog
                        .find_by_steam_id(game.art_id)
                        .is_some_and(|title| title.has_trainer)
                })
                .count();
            println!("Installed games with listed trainers: {installed_supported}");
            if let Some(stickman) = catalog.find_by_steam_id(2085540) {
                println!(
                    "Stickman title: {} (trainer listed: {})",
                    stickman.id, stickman.has_trainer
                );
                if let Some(thumbnail) = stickman.thumbnail.as_deref() {
                    match art::fetch_to_cache(&state.art_cache, 2085540, thumbnail) {
                        Ok(_) => println!("Stickman artwork: cached"),
                        Err(error) => println!("Stickman artwork: {error}"),
                    }
                }
            }
            println!("{}", trainer_capability_line());
            0
        }
        Err(error) => {
            println!("Live catalog: {error}");
            println!("{}", trainer_capability_line());
            if cached_count.is_some() { 0 } else { 1 }
        }
    }
}

/// One-line capability summary used by `--self-check`, derived from the live
/// state rather than hardcoded, so it cannot drift from the implementation.
fn trainer_capability_line() -> String {
    let (app_data, local) = TrainerPanel::paths();
    let storage = wand_rebuild_core::backend_store::resolve_blueprints_dir(&app_data, &local);
    let cached = wand_rebuild_core::trainer_blueprint::load_cached(&storage, "50696")
        .ok()
        .flatten()
        .is_some();
    format!(
        "Trainer activation: legacy (trainerlib) backend implemented; Stickman blueprint {}; \
         run --trainer-self-test with the game running for a live session",
        if cached {
            "found in Wand cache"
        } else {
            "from validated candidate set"
        }
    )
}

fn load_state() -> AppState {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let data = local.join("WandRebuild");
    let steam_root = std::env::var_os("PROGRAMFILES(X86)")
        .or_else(|| std::env::var_os("PROGRAMFILES"))
        .map(|folder| PathBuf::from(folder).join("Steam"));
    let steam_art = steam_root
        .as_deref()
        .map(|root| root.join("appcache/librarycache"))
        .unwrap_or_default();
    let recent = steam_root
        .as_deref()
        .map(steam::last_played)
        .unwrap_or_default();
    let mut games = steam_root
        .as_deref()
        .and_then(|root| steam::discover(root).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|game| LibraryGame {
            id: format!("steam:{}", game.app_id),
            name: game.name,
            platform: "Steam",
            art_id: game.app_id,
            launch_uri: game.launch_uri,
            last_played: recent.get(&game.app_id).copied().unwrap_or(0),
        })
        .collect::<Vec<_>>();
    if local
        .join("Packages/HelloGames.NoMansSky_bs190hzg1sesy")
        .is_dir()
    {
        games.push(LibraryGame {
            id: "store:HelloGames.NoMansSky".to_owned(),
            name: "No Man's Sky".to_owned(),
            platform: "Microsoft Store",
            art_id: 275850,
            launch_uri: "shell:AppsFolder\\HelloGames.NoMansSky_bs190hzg1sesy!NoMansSky".to_owned(),
            last_played: 0,
        });
    }
    let catalog_path = data.join("catalog.json");
    let catalog = std::fs::read(&catalog_path)
        .ok()
        .and_then(|bytes| Catalog::parse(&bytes).ok());
    let preferences_path = data.join("preferences.json");
    let preferences = Preferences::load(&preferences_path).unwrap_or_default();
    let backend = wand_rebuild_core::backend_store::resolve_runtime(&local)
        .ok()
        .flatten();
    AppState {
        games,
        catalog: Mutex::new(catalog),
        catalog_loading: AtomicBool::new(false),
        preferences: Mutex::new(preferences),
        preferences_path,
        catalog_path,
        art_cache: data.join("art"),
        steam_art,
        steam_root,
        backend,
    }
}

fn refresh_catalog_async(ui: &AppWindow, state: Arc<AppState>) {
    if state.catalog_loading.swap(true, Ordering::AcqRel) {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = Catalog::fetch_live_bytes().and_then(|bytes| {
            let catalog = Catalog::parse(&bytes)
                .map_err(|error| format!("Catalog format changed: {error}"))?;
            if let Some(parent) = state.catalog_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(error) = std::fs::write(&state.catalog_path, &bytes) {
                eprintln!("Could not cache Wand catalog: {error}");
            }
            Ok(catalog)
        });
        match result {
            Ok(catalog) => {
                let jobs = state
                    .games
                    .iter()
                    .filter_map(|game| {
                        let title = catalog.find_by_steam_id(game.art_id)?;
                        Some((game.art_id, title.thumbnail.clone()?))
                    })
                    .collect::<Vec<_>>();
                let count = catalog.len();
                if let Ok(mut guard) = state.catalog.lock() {
                    *guard = Some(catalog);
                }
                let callback_state = Arc::clone(&state);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_catalog_ready(true);
                    ui.set_search_status(format!("Wand catalog · {count} titles").into());
                    refresh_view(&ui, &callback_state);
                });
                let mut updated = false;
                for (app_id, path) in jobs {
                    match art::fetch_to_cache(&state.art_cache, app_id, &path) {
                        Ok(new_art) => updated |= new_art,
                        Err(error) => eprintln!("Could not cache art for {app_id}: {error}"),
                    }
                }
                if updated {
                    let callback_state = Arc::clone(&state);
                    let _ =
                        weak.upgrade_in_event_loop(move |ui| refresh_view(&ui, &callback_state));
                }
            }
            Err(error) => {
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_search_status(error.into()));
            }
        }
        state.catalog_loading.store(false, Ordering::Release);
    });
}

fn refresh_view(ui: &AppWindow, state: &Arc<AppState>) {
    let Ok(catalog) = state.catalog.lock() else {
        return;
    };
    let Ok(preferences) = state.preferences.lock() else {
        return;
    };
    let art_requests = if ui.get_searching() {
        set_catalog_results(ui, state, catalog.as_ref(), &preferences)
    } else {
        set_games(ui, state, catalog.as_ref(), &preferences);
        Vec::new()
    };
    drop(preferences);
    drop(catalog);
    spawn_art_fetch(ui, Arc::clone(state), art_requests);
}

/// Downloads artwork for catalog titles that are currently on screen.
///
/// Library games already have Steam artwork, but catalog-only titles do not, so
/// their cards rendered empty. Fetches run on a background thread and the view
/// re-renders once per batch; in-flight ids are tracked so a re-render cannot
/// start the same download again.
fn spawn_art_fetch(ui: &AppWindow, state: Arc<AppState>, requests: Vec<(u64, String)>) {
    if requests.is_empty() {
        return;
    }
    static IN_FLIGHT: std::sync::OnceLock<Mutex<std::collections::HashSet<u64>>> =
        std::sync::OnceLock::new();
    let in_flight = IN_FLIGHT.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let pending: Vec<(u64, String)> = {
        let Ok(mut guard) = in_flight.lock() else {
            return;
        };
        requests
            .into_iter()
            .filter(|(app_id, _)| guard.insert(*app_id))
            .take(16)
            .collect()
    };
    if pending.is_empty() {
        return;
    }
    let weak = ui.as_weak();
    let art_cache = state.art_cache.clone();
    std::thread::spawn(move || {
        let mut fetched = 0_usize;
        for (app_id, thumbnail) in &pending {
            if matches!(art::fetch_to_cache(&art_cache, *app_id, thumbnail), Ok(true)) {
                fetched += 1;
            }
        }
        if let Ok(mut guard) = in_flight.lock() {
            for (app_id, _) in &pending {
                guard.remove(app_id);
            }
        }
        if fetched == 0 {
            return;
        }
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                refresh_view(&ui, &state);
            }
        });
    });
}

fn set_games(
    ui: &AppWindow,
    state: &AppState,
    catalog: Option<&Catalog>,
    preferences: &Preferences,
) {
    let query = ui.get_search_query().to_lowercase();
    let mut games = state
        .games
        .iter()
        .filter(|game| {
            catalog.is_none_or(|catalog| {
                catalog
                    .find_by_steam_id(game.art_id)
                    .is_some_and(|title| title.has_trainer)
            })
        })
        .filter(|game| query.is_empty() || game.name.to_lowercase().contains(&query))
        .filter(|game| !ui.get_favorite_only() || preferences.favorites.contains(&game.id))
        .collect::<Vec<_>>();
    if ui.get_sort_by_recent() && query.is_empty() {
        games.sort_by(|a, b| {
            b.last_played
                .cmp(&a.last_played)
                .then_with(|| a.name.cmp(&b.name))
        });
    } else {
        games.sort_by_key(|game| game.name.to_lowercase());
        if ui.get_sort_descending() {
            games.reverse();
        }
    }
    let results = games
        .into_iter()
        .map(|game| GameInfo {
            id: game.id.clone().into(),
            name: game.name.clone().into(),
            detail: game.platform.into(),
            favorite: preferences.favorites.contains(&game.id),
            cover: image_for_app(state, game.art_id),
        })
        .collect::<Vec<_>>();
    ui.set_game_count(results.len() as i32);
    ui.set_games(ModelRc::from(Rc::new(VecModel::from(results))));
}

fn set_catalog_results(
    ui: &AppWindow,
    state: &AppState,
    catalog: Option<&Catalog>,
    preferences: &Preferences,
) -> Vec<(u64, String)> {
    let Some(catalog) = catalog else {
        ui.set_game_count(0);
        ui.set_games(ModelRc::from(Rc::new(VecModel::<GameInfo>::default())));
        return Vec::new();
    };
    let query = ui.get_search_query();
    let trimmed = query.trim();
    let titles = if trimmed.is_empty() {
        // Nothing typed yet: browse the catalog instead of showing an empty page.
        // (Favorites-only still filters below.)
        catalog.all().iter().collect::<Vec<_>>()
    } else {
        catalog.search(trimmed, 100)
    };
    let mut titles = titles
        .into_iter()
        .filter(|title| !ui.get_favorite_only() || title_is_favorite(title, preferences))
        .take(48)
        .collect::<Vec<_>>();
    titles.sort_by_key(|title| title.name.to_lowercase());
    if ui.get_sort_descending() {
        titles.reverse();
    }
    // Catalog-only titles have no Steam artwork; ask the caller to download it.
    let art_requests = titles
        .iter()
        .filter_map(|title| {
            let app_id = *title.steam_app_ids.first()?;
            let thumbnail = title.thumbnail.as_deref()?;
            (!art::cached_path(&state.art_cache, app_id).is_file())
                .then(|| (app_id, thumbnail.to_owned()))
        })
        .collect::<Vec<_>>();
    let results = titles
        .into_iter()
        .map(|title| GameInfo {
            id: format!("title:{}", title.id).into(),
            name: title.name.clone().into(),
            detail: if title.has_trainer {
                "Wand trainer".into()
            } else {
                "Wand catalog".into()
            },
            favorite: title_is_favorite(title, preferences),
            cover: title
                .steam_app_ids
                .iter()
                .find_map(|app_id| {
                    let image = image_for_app(state, *app_id);
                    (image.size().width > 0).then_some(image)
                })
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    ui.set_game_count(results.len() as i32);
    ui.set_games(ModelRc::from(Rc::new(VecModel::from(results))));
    art_requests
}

fn image_for_app(state: &AppState, app_id: u64) -> Image {
    Image::load_from_path(&art::cached_path(&state.art_cache, app_id))
        .ok()
        .or_else(|| {
            find_header_art(&state.steam_art, app_id)
                .and_then(|path| Image::load_from_path(&path).ok())
        })
        .unwrap_or_default()
}

fn find_header_art(root: &Path, app_id: u64) -> Option<PathBuf> {
    std::fs::read_dir(root.join(app_id.to_string()))
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("library_header.jpg"))
        .find(|path| path.is_file())
}

fn favorite_aliases(state: &AppState, id: &str) -> Vec<String> {
    let mut aliases = vec![id.to_owned()];
    if let Ok(guard) = state.catalog.lock() {
        if let Some(catalog) = guard.as_ref() {
            let title = id
                .strip_prefix("steam:")
                .and_then(|app| app.parse::<u64>().ok())
                .and_then(|app| catalog.find_by_steam_id(app))
                .or_else(|| {
                    id.strip_prefix("title:")
                        .and_then(|title| catalog.find_by_id(title))
                })
                .or_else(|| {
                    (id == "store:HelloGames.NoMansSky")
                        .then(|| catalog.find_by_steam_id(275850))
                        .flatten()
                });
            if let Some(title) = title {
                aliases.push(format!("title:{}", title.id));
                aliases.extend(title.steam_app_ids.iter().map(|app| format!("steam:{app}")));
                if title.steam_app_ids.contains(&275850) {
                    aliases.push("store:HelloGames.NoMansSky".to_owned());
                }
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

fn title_is_favorite(title: &CatalogTitle, preferences: &Preferences) -> bool {
    preferences
        .favorites
        .contains(&format!("title:{}", title.id))
        || title
            .steam_app_ids
            .iter()
            .any(|app| preferences.favorites.contains(&format!("steam:{app}")))
        || (title.steam_app_ids.contains(&275850)
            && preferences.favorites.contains("store:HelloGames.NoMansSky"))
}

fn stickman_cache_status() -> &'static str {
    const INSPECTED_SHA256: &str =
        "f0de8ff90c929f987f4724e9e53c251f9670f1103882ed312cb768b3201e6c6b";
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let path = match (app_data, local_app_data) {
        (Some(app_data), Some(local)) => {
            wand_rebuild_core::backend_store::resolve_trainers_dir(&app_data, &local)
        }
        _ => None,
    };
    match path.as_deref().map(|path| trainer_cache::find(path, 50696)) {
        Some(Ok(Some(binary))) if binary.sha256 == INSPECTED_SHA256 => {
            "Stickman trainer is cached and ready."
        }
        Some(Ok(Some(_))) => {
            "Stickman trainer cache differs from the inspected version."
        }
        Some(Ok(None)) => "Stickman trainer is not cached; open Wand once to download it.",
        _ => "No trainer cache found; install Wand once or adopt a backend.",
    }
}

#[cfg(windows)]
fn open_shell_target(target: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let operation = std::ffi::OsStr::new("open")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if code as isize <= 32 {
        Err(format!(
            "Windows could not open the game (ShellExecute code {}).",
            code as isize
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn open_shell_target(_target: &str) -> Result<(), String> {
    Err("Game launching is available on Windows only.".to_owned())
}
