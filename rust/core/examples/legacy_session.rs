//! Live driver for Wand's legacy (`trainerlib`) trainer backend.
//!
//! This is the integration harness for the first real trainer session. It does
//! not simulate anything: it creates the pipe servers, injects the installed
//! `TrainerLib` into the running game process, waits for the trainer to report
//! `Ready`, then sends real modification values and reports the trainer's own
//! responses.
//!
//! Usage:
//!   cargo run --release --example legacy_session -- --probe
//!   cargo run --release --example legacy_session -- --toggle god_mode
//!   cargo run --release --example legacy_session -- --hold 60
//!
//! Flags:
//!   --probe            send every known variable a value of 0 and report the
//!                      trainer's result code (0 = variable accepted)
//!   --toggle <target>  set <target> to its activation value for 3 seconds,
//!                      then back to 0
//!   --hold <seconds>   keep the session open for N seconds (default 5)
//!   --dry-run          resolve everything but do not inject
//!   --game <exe>       game executable name (default "Stick It To The Stickman.exe")
//!   --trainer <id>     trainer id (default 50696)

use std::env;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};
use wand_rebuild_core::process_monitor;
use wand_rebuild_core::trainer_blueprint::{self, TrainerBlueprint};
use wand_rebuild_core::trainer_cache;
use wand_rebuild_core::trainer_session::{
    SessionConfig, SessionEvent, TrainerSession, Architecture,
};

fn main() {
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

struct Options {
    probe: bool,
    toggles: Vec<String>,
    hold: Duration,
    dry_run: bool,
    game: String,
    trainer_id: u64,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        probe: false,
        toggles: Vec::new(),
        hold: Duration::from_secs(5),
        dry_run: false,
        game: "Stick It To The Stickman.exe".to_owned(),
        trainer_id: 50696,
    };
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--probe" => options.probe = true,
            "--dry-run" => options.dry_run = true,
            "--toggle" => {
                options
                    .toggles
                    .push(arguments.next().ok_or("--toggle needs a target")?);
            }
            "--hold" => {
                let seconds: u64 = arguments
                    .next()
                    .ok_or("--hold needs a number")?
                    .parse()
                    .map_err(|_| "--hold needs a number")?;
                options.hold = Duration::from_secs(seconds.min(600));
            }
            "--game" => options.game = arguments.next().ok_or("--game needs a name")?,
            "--trainer" => {
                options.trainer_id = arguments
                    .next()
                    .ok_or("--trainer needs an id")?
                    .parse()
                    .map_err(|_| "--trainer needs a numeric id")?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

fn run() -> Result<(), String> {
    let options = parse_options()?;
    let app_data = env::var("APPDATA").map_err(|_| "APPDATA is not set")?;
    let local_app_data = env::var("LOCALAPPDATA").map_err(|_| "LOCALAPPDATA is not set")?;

    let runtime = wand_rebuild_core::backend_store::resolve_runtime(&PathBuf::from(&local_app_data))
        .map_err(|error| format!("could not inspect installed Wand runtime: {error}"))?
        .ok_or("no complete Wand runtime was found")?;
    println!(
        "runtime        : {}",
        runtime
            .version
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    );
    println!("trainerlib dir : {}", runtime.trainer_directory.display());

    let trainer_root =
        wand_rebuild_core::backend_store::resolve_trainers_dir(
            &PathBuf::from(&app_data),
            &PathBuf::from(&local_app_data),
        )
        .ok_or("no trainer cache directory was found")?;
    let cached = trainer_cache::find(&trainer_root, options.trainer_id)
        .map_err(|error| format!("could not search the trainer cache: {error}"))?
        .ok_or_else(|| {
            format!(
                "trainer {} is not cached in {}",
                options.trainer_id,
                trainer_root.display()
            )
        })?;
    println!("trainer dll    : {}", cached.path.display());
    println!("trainer sha256 : {}", cached.sha256);

    let storage = wand_rebuild_core::backend_store::resolve_blueprints_dir(
        &PathBuf::from(&app_data),
        &PathBuf::from(&local_app_data),
    );
    let blueprint = match trainer_blueprint::load_cached(&storage, &options.trainer_id.to_string()) {
        Ok(Some(blueprint)) => {
            println!("blueprint      : Wand cache ({} cheats)", blueprint.cheats.len());
            blueprint
        }
        Ok(None) => {
            println!("blueprint      : Wand cache miss, using validated candidate");
            candidate_for(options.trainer_id)?
        }
        Err(error) => {
            println!("blueprint      : cache unreadable ({error}), using candidate");
            candidate_for(options.trainer_id)?
        }
    };
    println!("backend        : {}", blueprint.backend.as_str());
    for cheat in &blueprint.cheats {
        println!(
            "  - {:<28} target={:<28} type={:?}",
            cheat.name, cheat.target, cheat.kind
        );
    }

    let process = process_monitor::find_process(&options.game).ok_or_else(|| {
        format!(
            "{} is not running; start the game first",
            options.game
        )
    })?;
    println!(
        "game process   : {} (pid {})",
        process.executable, process.process_id
    );

    let game_version = process_monitor::cached_game_version(&storage, &blueprint.game_id)
        .or_else(|| blueprint.preferred_game_version())
        .unwrap_or_default();
    println!("game version   : {game_version}");

    let architecture = Architecture::of_process(process.process_id);
    println!("architecture   : {architecture:?}");
    let variables = blueprint.variables();
    println!("variables      : {variables:?}");

    if options.dry_run {
        let config = SessionConfig {
            trainer_library_directory: runtime.trainer_directory.clone(),
            trainer_dll: cached.path.clone(),
            target_process_id: process.process_id,
            game_version,
            architecture,
            flags: 0,
            variables,
        };
        println!("dry run        : would start session with {config:?}");
        return Ok(());
    }

    let config = SessionConfig {
        trainer_library_directory: runtime.trainer_directory.clone(),
        trainer_dll: cached.path.clone(),
        target_process_id: process.process_id,
        game_version,
        architecture,
        flags: 0,
        variables,
    };

    println!("\nstarting session...");
    let mut session = TrainerSession::start(blueprint.clone(), config)
        .map_err(|error| format!("could not start the session: {error}"))?;

    let started = Instant::now();
    let mut ready = false;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        for event in session.poll() {
            match &event {
                SessionEvent::PipeConnected => println!("[{:>6.2}s] pipes connected", started.elapsed().as_secs_f32()),
                SessionEvent::Ready => {
                    println!("[{:>6.2}s] trainer READY", started.elapsed().as_secs_f32());
                    ready = true;
                }
                SessionEvent::Value { name, value } => {
                    println!("[{:>6.2}s] value {name} = {value}", started.elapsed().as_secs_f32())
                }
                SessionEvent::ValueResult { request_id, result } => {
                    println!(
                        "[{:>6.2}s] value result req={request_id} result={result}",
                        started.elapsed().as_secs_f32()
                    )
                }
                SessionEvent::Log(message) => println!(
                    "[{:>6.2}s] log[{}] {}",
                    started.elapsed().as_secs_f32(),
                    message.level,
                    message.message.trim_end()
                ),
                SessionEvent::Ended(reason) => {
                    println!("[{:>6.2}s] ended: {reason}", started.elapsed().as_secs_f32());
                    return Ok(());
                }
                SessionEvent::Failed(reason) => {
                    println!("[{:>6.2}s] FAILED: {reason}", started.elapsed().as_secs_f32());
                    return Err(reason.clone());
                }
            }
        }
        if ready {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    if !ready {
        session.stop();
        return Err("trainer never became ready".to_owned());
    }

    if options.probe {
        println!("\nprobing variables (result 0 means the trainer accepted the name)...");
        let mut probes = Vec::new();
        for (index, variable) in blueprint.variables().iter().enumerate() {
            let request_id = session
                .set_value(variable, 0.0)
                .map_err(|error| error.to_string())?;
            probes.push((request_id, variable.clone()));
            if index % 8 == 7 {
                sleep(Duration::from_millis(50));
            }
        }
        let probe_deadline = Instant::now() + Duration::from_secs(10);
        let mut resolved = 0_usize;
        let mut results = std::collections::HashMap::new();
        while Instant::now() < probe_deadline && resolved < probes.len() {
            for event in session.poll() {
                if let SessionEvent::ValueResult { request_id, result } = event {
                    results.insert(request_id, result);
                    resolved += 1;
                }
            }
            sleep(Duration::from_millis(20));
        }
        for (request_id, variable) in &probes {
            match results.get(request_id) {
                Some(0) => println!("  accepted  {variable}"),
                Some(code) => println!("  rejected  {variable} (result {code})"),
                None => println!("  no reply  {variable}"),
            }
        }
    }

    for target in &options.toggles {
        let Some(cheat) = blueprint.cheat_for_target(target) else {
            println!("unknown target {target}");
            continue;
        };
        let value = cheat.enabled_value();
        println!("\nenabling {target} = {value}");
        session
            .set_value(target, value)
            .map_err(|error| error.to_string())?;
        sleep(Duration::from_secs(3));
        println!("disabling {target} = 0");
        session
            .set_value(target, 0.0)
            .map_err(|error| error.to_string())?;
        sleep(Duration::from_millis(500));
    }

    println!("\nholding session for {:?}", options.hold);
    let hold_until = Instant::now() + options.hold;
    while Instant::now() < hold_until {
        for event in session.poll() {
            match event {
                SessionEvent::Value { name, value } => {
                    println!("[{:>6.2}s] value {name} = {value}", started.elapsed().as_secs_f32())
                }
                SessionEvent::Log(message) => println!(
                    "[{:>6.2}s] log[{}] {}",
                    started.elapsed().as_secs_f32(),
                    message.level,
                    message.message.trim_end()
                ),
                SessionEvent::Ended(reason) => {
                    println!("ended: {reason}");
                    return Ok(());
                }
                SessionEvent::Failed(reason) => {
                    println!("FAILED: {reason}");
                    return Err(reason);
                }
                _ => {}
            }
        }
        sleep(Duration::from_millis(50));
    }

    println!("stopping session");
    session.stop();
    println!("done");
    Ok(())
}

fn candidate_for(trainer_id: u64) -> Result<TrainerBlueprint, String> {
    if trainer_id == 50696 {
        Ok(trainer_blueprint::stickman_candidate())
    } else {
        Err(format!(
            "no blueprint is cached for trainer {trainer_id} and no candidate exists"
        ))
    }
}
