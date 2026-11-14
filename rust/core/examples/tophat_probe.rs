//! Read-only probe of a Tophat service already started by the installed Wand app.
//! It does not launch a game, load a plugin, or send trainer values.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wand_rebuild_core::tophat_codec::{decode_heartbeat_response, encode_heartbeat};
use wand_rebuild_core::tophat_registry::find_verified_port;
use wand_rebuild_core::tophat_transport::{Method, unary_local_once};

fn main() -> Result<(), String> {
    let pid = std::env::args()
        .nth(1)
        .ok_or_else(|| "Usage: tophat_probe <trainer-host-or-game-pid>".to_owned())?
        .parse::<u32>()
        .map_err(|_| "PID must be a positive integer".to_owned())?;
    if pid == 0 {
        return Err("PID must be a positive integer".to_owned());
    }
    let registry = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "APPDATA is not defined".to_owned())?
        .join("Wand/service-ports.json");
    let port = find_verified_port(&registry, pid)
        .map_err(|error| format!("Could not verify Wand service registry: {error:?}"))?
        .ok_or_else(|| "No verified live Tophat listener belongs to that PID".to_owned())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis() as u64;
    let request = encode_heartbeat(now, 1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let reply = runtime
        .block_on(unary_local_once(
            port,
            Method::Heartbeat,
            &request,
            Duration::from_secs(5),
        ))
        .map_err(|error| format!("Tophat Heartbeat failed: {error:?}"))?;
    let response = decode_heartbeat_response(&reply)
        .map_err(|error| format!("Invalid Heartbeat reply: {error:?}"))?;
    println!(
        "Tophat service PID {pid}, port {port}: success={}, sequence={}, error_present={}",
        response.success, response.sequence_number, response.has_error
    );
    if response.success && response.sequence_number == 1 {
        Ok(())
    } else {
        Err("Heartbeat did not confirm this request".to_owned())
    }
}
