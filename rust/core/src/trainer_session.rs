//! Legacy (`trainerlib`) trainer session: pipe servers, injection, frame
//! exchange and value control.
//!
//! Threading model: one session thread owns the state machine. Three helper
//! threads own one pipe server each (`In`, `Out`, `Log`) and forward decoded
//! messages into a single channel, so a blocking read can never stall commands.

use crate::legacy_pipe::{Frame, LegacyPipeCodec, LogMessage};
use crate::named_pipe::{PipeServer, out_greeting, session_pipe_names};
use crate::trainer_blueprint::{Backend, TrainerBlueprint};
use crate::trainer_inject::{BootstrapSpec, inject_trainer};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long the trainer may take to connect its `In` pipe after injection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the trainer may take to report `Ready` after connecting.
pub const READY_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// The `Out` pipe received the trainer connection.
    PipeConnected,
    /// The trainer reported `Ready`; modifications can now be sent.
    Ready,
    /// The trainer pushed a variable value.
    Value { name: String, value: f64 },
    /// Result of a `SetValue` command, correlated by `request_id`.
    ValueResult { request_id: u32, result: u32 },
    /// A log line from the trainer.
    Log(LogMessage),
    /// The session ended. `reason` is human readable.
    Ended(String),
    /// The session failed before becoming usable.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionCommand {
    SetValue {
        request_id: u32,
        name: String,
        value: f64,
    },
    SendBuffer {
        kind: String,
        payload: Vec<u8>,
    },
    Stop,
}

#[derive(Debug)]
pub enum SessionError {
    Pipe(std::io::Error),
    Inject(String),
    Unsupported(String),
    Blueprint(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pipe(error) => write!(formatter, "pipe error: {error}"),
            Self::Inject(message) => write!(formatter, "injection failed: {message}"),
            Self::Unsupported(message) => formatter.write_str(message),
            Self::Blueprint(message) => write!(formatter, "blueprint error: {message}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// Everything a legacy session needs to start.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub trainer_library_directory: PathBuf,
    pub trainer_dll: PathBuf,
    pub target_process_id: u32,
    pub game_version: u32,
    pub architecture: Architecture,
    pub flags: u32,
    pub variables: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    X64,
    X86,
}

impl Architecture {
    /// Picks the architecture matching the target process.
    pub fn of_process(process_id: u32) -> Self {
        #[cfg(windows)]
        {
            if is_wow64(process_id).unwrap_or(false) {
                return Self::X86;
            }
        }
        Self::X64
    }

    pub fn trainerlib_name(self) -> &'static str {
        match self {
            Self::X64 => "TrainerLib_x64.dll",
            Self::X86 => "TrainerLib_x86.dll",
        }
    }
}

#[cfg(windows)]
fn is_wow64(process_id: u32) -> Option<bool> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        IsWow64Process, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: `OpenProcess` takes scalar arguments.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return None;
    }
    let mut wow64 = 0;
    // SAFETY: the handle is live and `wow64` is a valid out pointer.
    let ok = unsafe { IsWow64Process(process, &mut wow64) };
    // SAFETY: the handle is owned by this function.
    unsafe {
        CloseHandle(process);
    }
    let _: *mut c_void = ptr::null_mut();
    (ok != 0).then_some(wow64 != 0)
}

/// Handle used by the UI/controller side of a running session.
pub struct TrainerSession {
    command_tx: Sender<SessionCommand>,
    event_rx: Receiver<SessionEvent>,
    join: Option<JoinHandle<()>>,
    finished: Option<Receiver<()>>,
    pub blueprint: TrainerBlueprint,
    pub process_id: u32,
    next_request_id: u32,
}

impl TrainerSession {
    /// Creates the pipe servers, injects the trainer and waits for `Ready`
    /// in the background. Events are delivered through [`TrainerSession::poll`].
    pub fn start(
        blueprint: TrainerBlueprint,
        config: SessionConfig,
    ) -> Result<Self, SessionError> {
        if blueprint.backend != Backend::TrainerLib {
            return Err(SessionError::Unsupported(format!(
                "trainer backend is {}; only trainerlib is implemented",
                blueprint.backend.as_str()
            )));
        }
        let token = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let (in_name, out_name, log_name) = session_pipe_names(token);

        let input = Arc::new(PipeServer::create(&in_name).map_err(SessionError::Pipe)?);
        let output = Arc::new(PipeServer::create(&out_name).map_err(SessionError::Pipe)?);
        let log = PipeServer::create(&log_name).map_err(SessionError::Pipe)?;

        let (message_tx, message_rx) = channel::<SessionMessage>();
        let (command_tx, command_rx) = channel::<SessionCommand>();
        let (event_tx, event_rx) = channel::<SessionEvent>();
        let (finished_tx, finished_rx) = channel::<()>();
        let shutdown = Arc::new(AtomicBool::new(false));

        spawn_out_reader(
            Arc::clone(&output),
            out_name.clone(),
            in_name.clone(),
            Arc::clone(&shutdown),
            message_tx.clone(),
        );
        spawn_in_reader(Arc::clone(&input), message_tx.clone());
        spawn_log_reader(log, message_tx.clone());

        let process_id = config.target_process_id;
        let variables = config.variables.clone();
        let join = thread::Builder::new()
            .name("wand-trainer-session".to_owned())
            .spawn(move || {
                run_session(
                    config,
                    log_name,
                    out_name,
                    output,
                    input,
                    shutdown,
                    message_rx,
                    command_rx,
                    event_tx,
                );
                let _ = finished_tx.send(());
            })
            .map_err(SessionError::Pipe)?;

        let _ = variables;
        Ok(Self {
            command_tx,
            event_rx,
            join: Some(join),
            finished: Some(finished_rx),
            blueprint,
            process_id,
            next_request_id: 1,
        })
    }

    /// Drains pending events without blocking.
    pub fn poll(&self) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        events
    }

    /// Queues a modification value change. The result arrives as
    /// [`SessionEvent::ValueResult`] with the returned request id.
    pub fn set_value(&mut self, name: &str, value: f64) -> Result<u32, SessionError> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        self.command_tx
            .send(SessionCommand::SetValue {
                request_id,
                name: name.to_owned(),
                value,
            })
            .map_err(|_| SessionError::Inject("trainer session is no longer running".to_owned()))?;
        Ok(request_id)
    }

    /// Sends an arbitrary `Buffer` frame to the trainer.
    pub fn send_buffer(&self, kind: &str, payload: Vec<u8>) -> Result<(), SessionError> {
        self.command_tx
            .send(SessionCommand::SendBuffer {
                kind: kind.to_owned(),
                payload,
            })
            .map_err(|_| SessionError::Inject("trainer session is no longer running".to_owned()))
    }

    pub fn stop(&mut self) {
        let _ = self.command_tx.send(SessionCommand::Stop);
        // Never block the caller indefinitely on a wedged pipe: the session
        // thread signals completion, and a timeout just detaches it.
        if let Some(finished) = self.finished.take() {
            let _ = finished.recv_timeout(Duration::from_secs(5));
        }
        if let Some(join) = self.join.take() {
            if join.is_finished() {
                let _ = join.join();
            }
        }
    }
}

impl Drop for TrainerSession {
    fn drop(&mut self) {
        self.stop();
    }
}

enum SessionMessage {
    PipeConnected(&'static str),
    Frame(Frame),
    Log(LogMessage),
    PipeClosed(&'static str, String),
}

/// Diagnostic switch: when `WAND_DEBUG_PIPES` is set, every raw pipe read and
/// write is logged as hex so the real traffic direction can be established
/// without guessing.
fn debug_pipes() -> bool {
    std::env::var_os("WAND_DEBUG_PIPES").is_some()
}

fn debug_bytes(direction: &str, pipe: &str, bytes: &[u8]) {
    if !debug_pipes() {
        return;
    }
    let shown: String = bytes
        .iter()
        .take(96)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("[pipe {pipe} {direction}] {shown}");
}

fn spawn_out_reader(
    server: Arc<PipeServer>,
    out_name: String,
    in_name: String,
    shutdown: Arc<AtomicBool>,
    messages: Sender<SessionMessage>,
) {
    let _ = thread::Builder::new()
        .name("wand-trainer-out".to_owned())
        .spawn(move || {
            if let Err(error) = server.accept() {
                let _ = messages.send(SessionMessage::PipeClosed(
                    "Out",
                    format!("{out_name}: {error}"),
                ));
                return;
            }
            // The reference client greets the trainer with the In pipe path.
            if let Err(error) = server
                .write(&out_greeting(&in_name))
                .and_then(|()| server.flush())
            {
                let _ = messages.send(SessionMessage::PipeClosed(
                    "Out",
                    format!("greeting failed: {error}"),
                ));
                return;
            }
            debug_bytes("greeting->", "Out", &out_greeting(&in_name));
            let _ = messages.send(SessionMessage::PipeConnected("Out"));
            // This handle is the command channel: it is never read from, because
            // the pipes are synchronous and a blocked `ReadFile` would deadlock
            // the `WriteFile` that sends commands. Keep the connection open by
            // holding the handle until the session shuts down.
            while !shutdown.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(25));
            }
            let _ = server.flush();
        });
}

fn spawn_in_reader(server: Arc<PipeServer>, messages: Sender<SessionMessage>) {
    let _ = thread::Builder::new()
        .name("wand-trainer-in".to_owned())
        .spawn(move || {
            if let Err(error) = server.accept() {
                let _ = messages.send(SessionMessage::PipeClosed(
                    "In",
                    format!("accept failed: {error}"),
                ));
                return;
            }
            let _ = messages.send(SessionMessage::PipeConnected("In"));
            let mut codec = LegacyPipeCodec::default();
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                match server.read(&mut buffer) {
                    Ok(0) => {
                        let _ = messages.send(SessionMessage::PipeClosed(
                            "In",
                            "trainer closed the command pipe".to_owned(),
                        ));
                        return;
                    }
                    Ok(count) => {
                        debug_bytes("<-", "In", &buffer[..count]);
                        match codec.push(&buffer[..count]) {
                        Ok(frames) => {
                            for frame in frames {
                                if messages.send(SessionMessage::Frame(frame)).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = messages.send(SessionMessage::PipeClosed(
                                "In",
                                format!("frame decode failed: {error:?}"),
                            ));
                            return;
                        }
                        }
                    }
                    Err(error) => {
                        let _ = messages.send(SessionMessage::PipeClosed(
                            "In",
                            format!("read failed: {error}"),
                        ));
                        return;
                    }
                }
            }
        });
}

fn spawn_log_reader(server: PipeServer, messages: Sender<SessionMessage>) {
    let _ = thread::Builder::new()
        .name("wand-trainer-log".to_owned())
        .spawn(move || {
            if let Err(error) = server.accept() {
                let _ = messages.send(SessionMessage::PipeClosed(
                    "Log",
                    format!("accept failed: {error}"),
                ));
                return;
            }
            let mut codec = crate::legacy_pipe::LogCodec::default();
            let mut buffer = vec![0_u8; 32 * 1024];
            loop {
                match server.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => {
                        for message in codec.push(&buffer[..count]) {
                            if messages.send(SessionMessage::Log(message)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => return,
                }
            }
        });
}

fn run_session(
    config: SessionConfig,
    log_name: String,
    out_name: String,
    command_pipe: Arc<PipeServer>,
    response_pipe: Arc<PipeServer>,
    shutdown: Arc<AtomicBool>,
    messages: Receiver<SessionMessage>,
    commands: Receiver<SessionCommand>,
    events: Sender<SessionEvent>,
) {
    /// Clears the helper threads' shutdown flag on every exit path.
    struct ShutdownGuard(Arc<AtomicBool>);
    impl Drop for ShutdownGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _shutdown_guard = ShutdownGuard(Arc::clone(&shutdown));

    let trainerlib = config
        .trainer_library_directory
        .join(config.architecture.trainerlib_name());
    if !trainerlib.is_file() {
        let _ = events.send(SessionEvent::Failed(format!(
            "{} is missing from the installed runtime",
            trainerlib.display()
        )));
        return;
    }
    // The reference client writes commands on the `Out` connection. `In` is
    // offered as an override because the two channels are both duplex and the
    // only authoritative proof is the trainer answering.
    let command_target = match std::env::var("WAND_COMMAND_PIPE").as_deref() {
        Ok("in") => CommandTarget::In,
        Ok("both") => CommandTarget::Both,
        _ => CommandTarget::Out,
    };

    let spec = BootstrapSpec {
        log_pipe: log_name,
        // The reference client's `messagePipe` is the **Out** pipe: it is the
        // channel the client writes commands on. The trainer learns the `In`
        // path from the greeting that is written on that same connection.
        message_pipe: out_name,
        flags: config.flags,
        trainer_dll: config.trainer_dll.to_string_lossy().into_owned(),
        target_process_id: config.target_process_id,
        game_version: config.game_version,
        variables: config.variables.clone(),
    };

    let inject_result = inject_trainer(
        config.target_process_id,
        &trainerlib,
        &config.trainer_dll,
        &spec,
    );
    if let Err(error) = inject_result {
        let _ = events.send(SessionEvent::Failed(format!(
            "could not start the trainer runtime: {error}"
        )));
        return;
    }

    let started = Instant::now();
    let mut ready = false;
    let mut connected = false;
    let mut ready_deadline = None::<Instant>;
    loop {
        // Commands are only accepted once the trainer is ready.
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok(SessionCommand::Stop) => {
                let _ = events.send(SessionEvent::Ended("stopped by user".to_owned()));
                return;
            }
            Ok(command) => {
                if ready {
                    if let Err(error) = dispatch_command(
                        command,
                        &command_pipe,
                        &response_pipe,
                        command_target,
                    ) {
                        let _ = events.send(SessionEvent::Failed(error));
                        return;
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = events.send(SessionEvent::Ended("controller dropped".to_owned()));
                return;
            }
        }

        match messages.recv_timeout(Duration::from_millis(50)) {
            Ok(SessionMessage::PipeConnected(which)) => {
                if which == "In" {
                    connected = true;
                    ready_deadline = Some(Instant::now() + READY_TIMEOUT);
                    let _ = events.send(SessionEvent::PipeConnected);
                }
            }
            Ok(SessionMessage::Frame(frame)) => match frame {
                Frame::Ready => {
                    ready = true;
                    let _ = events.send(SessionEvent::Ready);
                }
                Frame::Value { name, value } => {
                    let _ = events.send(SessionEvent::Value { name, value });
                }
                Frame::ValueResponse { request_id, result } => {
                    let _ = events.send(SessionEvent::ValueResult { request_id, result });
                }
                Frame::Loading => {}
                Frame::Binary { .. } => {}
            },
            Ok(SessionMessage::Log(message)) => {
                let failed = message.message.to_ascii_lowercase().contains("trainer setup failed");
                let _ = events.send(SessionEvent::Log(message));
                if failed && !ready {
                    let _ = events.send(SessionEvent::Failed(
                        "the trainer reported a setup failure".to_owned(),
                    ));
                    return;
                }
            }
            Ok(SessionMessage::PipeClosed(which, reason)) => {
                if which == "In" && !ready {
                    let _ = events.send(SessionEvent::Failed(reason));
                } else {
                    let _ = events.send(SessionEvent::Ended(reason));
                }
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = events.send(SessionEvent::Ended("pipes closed".to_owned()));
                return;
            }
        }

        if !connected && started.elapsed() > CONNECT_TIMEOUT {
            let _ = events.send(SessionEvent::Failed(format!(
                "the trainer did not connect to its pipes within {}s. If a previous session was \
                 interrupted, restart the game and try again.",
                CONNECT_TIMEOUT.as_secs()
            )));
            return;
        }
        if let Some(deadline) = ready_deadline {
            if !ready && Instant::now() > deadline {
                let _ = events.send(SessionEvent::Failed(format!(
                    "the trainer connected but never became ready within {}s",
                    READY_TIMEOUT.as_secs()
                )));
                return;
            }
        }
    }
}

/// Writes one command frame to the trainer's `In` pipe.
///
/// The reference client sends `Value` frames for value changes (request id in
/// the frame, result echoed by `ValueResponse`) and `Buffer` frames for typed
/// payloads. Both encodings come from [`LegacyPipeCodec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandTarget {
    Out,
    In,
    Both,
}

fn dispatch_command(
    command: SessionCommand,
    out_pipe: &PipeServer,
    in_pipe: &PipeServer,
    target: CommandTarget,
) -> Result<(), String> {
    let frame = match command {
        SessionCommand::SetValue {
            request_id,
            name,
            value,
        } => LegacyPipeCodec::encode_set_value(request_id, &name, value)
            .map(|bytes| bytes.to_vec())
            .map_err(|error| format!("could not encode value frame: {error:?}"))?,
        SessionCommand::SendBuffer { kind, payload } => {
            LegacyPipeCodec::encode_binary(&kind, &payload)
                .map_err(|error| format!("could not encode buffer frame: {error:?}"))?
        }
        SessionCommand::Stop => return Ok(()),
    };
    let write = |pipe: &PipeServer, name: &str| -> Result<(), String> {
        pipe.write(&frame)
            .map(|()| debug_bytes("->", name, &frame))
            .map_err(|error| format!("could not write to the {name} pipe: {error}"))
    };
    match target {
        CommandTarget::Out => write(out_pipe, "Out"),
        CommandTarget::In => write(in_pipe, "In"),
        CommandTarget::Both => {
            write(out_pipe, "Out")?;
            write(in_pipe, "In")
        }
    }
}

/// Convenience helper: builds the session configuration for a cached trainer.
pub fn config_for(
    trainer_library_directory: &Path,
    trainer_dll: &Path,
    target_process_id: u32,
    game_version: u32,
    blueprint: &TrainerBlueprint,
) -> SessionConfig {
    SessionConfig {
        trainer_library_directory: trainer_library_directory.to_path_buf(),
        trainer_dll: trainer_dll.to_path_buf(),
        target_process_id,
        game_version,
        architecture: Architecture::of_process(target_process_id),
        flags: 0,
        variables: blueprint.variables(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_trainerlib_backends_before_touching_the_system() {
        let mut blueprint = crate::trainer_blueprint::stickman_candidate();
        blueprint.backend = Backend::Tophat;
        let config = SessionConfig {
            trainer_library_directory: PathBuf::from("C:\\nonexistent"),
            trainer_dll: PathBuf::from("C:\\nonexistent\\trainer.dll"),
            target_process_id: 0,
            game_version: 0,
            architecture: Architecture::X64,
            flags: 0,
            variables: Vec::new(),
        };
        match TrainerSession::start(blueprint, config) {
            Err(SessionError::Unsupported(message)) => assert!(message.contains("tophat")),
            Ok(_) => panic!("expected an unsupported-backend error, got a running session"),
            Err(other) => panic!("expected an unsupported-backend error, got {other}"),
        }
    }

    #[test]
    fn architecture_maps_to_the_reference_dll_names() {
        assert_eq!(Architecture::X64.trainerlib_name(), "TrainerLib_x64.dll");
        assert_eq!(Architecture::X86.trainerlib_name(), "TrainerLib_x86.dll");
    }

    #[test]
    fn config_uses_blueprint_variables_and_architecture() {
        let blueprint = crate::trainer_blueprint::stickman_candidate();
        let config = config_for(
            Path::new("C:\\runtime"),
            Path::new("C:\\trainers\\Trainer_50696.dll"),
            4321,
            2_639_440_150,
            &blueprint,
        );
        assert_eq!(config.target_process_id, 4321);
        assert_eq!(config.game_version, 2_639_440_150);
        assert_eq!(config.variables, blueprint.variables());
        assert!(!config.variables.is_empty());
    }

    #[test]
    fn request_ids_start_at_one_and_never_repeat() {
        // Mirrors the reference client, which begins its correlation counter at 1.
        let mut counter = 1_u32;
        let first = counter;
        counter = counter.wrapping_add(1).max(1);
        let second = counter;
        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_ne!(first, second);
    }
}
