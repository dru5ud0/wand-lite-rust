//! Windows named-pipe integration test for the legacy trainer transport.
//!
//! This exercises the real Win32 pipe implementation against a *simulated*
//! trainer: it verifies the `Out` greeting, the `In`/`Out` direction split, the
//! `Loading`/`Ready` handshake, request/response correlation and the log channel.
//!
//! It does not inject anything and does not prove a live trainer session; the
//! live-session evidence lives in `BACKEND_MAP.md`. What it does prove is that
//! the transport layer behaves correctly on real Windows IPC.

#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;
use wand_rebuild_core::legacy_pipe::{Frame, LegacyPipeCodec, LogCodec, LOG_INFO, encode_log};
use wand_rebuild_core::named_pipe::{PipeServer, out_greeting, session_pipe_names};

/// Connects to an existing named-pipe server as a client.
fn connect(name: &str) -> std::fs::File {
    // The server is created before the client connects, so a short retry loop is
    // enough; `ERROR_PIPE_BUSY` cannot occur because the server is idle.
    for _ in 0..200 {
        match OpenOptions::new().read(true).write(true).open(name) {
            Ok(file) => return file,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("could not connect to {name}");
}

#[test]
fn legacy_transport_handshake_and_value_roundtrip() {
    let (in_name, out_name, log_name) = session_pipe_names(1_794_600_000_123);

    // Client side (what our session does): three pipe servers.
    let input = PipeServer::create(&in_name).expect("create In");
    let output = PipeServer::create(&out_name).expect("create Out");
    let log = PipeServer::create(&log_name).expect("create Log");

    // --- Simulated trainer: connect to Out, read the greeting, then use In. ---
    let trainer = thread::spawn({
        let in_name = in_name.clone();
        move || {
            let mut out = connect(&out_name);
            // 1. Read the greeting and confirm it names the In pipe.
            let mut greeting = vec![0_u8; 512];
            let count = out.read(&mut greeting).expect("read greeting");
            greeting.truncate(count);
            let units: Vec<u16> = greeting
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let text = String::from_utf16_lossy(&units);
            let text = text.trim_end_matches('\0').to_owned();
            assert_eq!(text, in_name, "greeting must carry the In pipe path");

            // 2. Open the event channel and report Loading then Ready.
            let mut events = connect(&in_name);
            events.write_all(&0_u32.to_le_bytes()).unwrap(); // Loading
            events.write_all(&0_u32.to_le_bytes()).unwrap();
            events.flush().unwrap();
            events.write_all(&1_u32.to_le_bytes()).unwrap(); // Ready
            events.write_all(&0_u32.to_le_bytes()).unwrap();
            events.flush().unwrap();

            // 3. Receive a Value frame from the command channel.
            let mut frame = vec![0_u8; 56];
            out.read_exact(&mut frame).expect("read value frame");
            let mut codec = LegacyPipeCodec::default();
            let decoded = codec.push(&frame).expect("decode value frame");
            let Frame::Value { name, value } = &decoded[0] else {
                panic!("expected a Value frame, got {decoded:?}");
            };
            assert_eq!(name, "god_mode");
            assert!((*value - 1.0).abs() < f64::EPSILON);
            let request_id = u32::from_le_bytes(frame[8..12].try_into().unwrap());
            assert_eq!(request_id, 7);

            // 4. Answer with a correlated ValueResponse on the event channel.
            let mut response = [0_u8; 24];
            response[0..4].copy_from_slice(&3_u32.to_le_bytes());
            response[8..12].copy_from_slice(&request_id.to_le_bytes());
            response[16..20].copy_from_slice(&0_u32.to_le_bytes());
            events.write_all(&response).unwrap();
            events.flush().unwrap();

            // 5. Emit a log line as well.
            let mut logs = connect(&log_name);
            logs.write_all(&encode_log(LOG_INFO, "god_mode set to 1 by client."))
                .unwrap();
            logs.flush().unwrap();
            thread::sleep(Duration::from_millis(50));
        }
    });

    // --- Client side: accept the Out connection and greet the trainer. ---
    output.accept().expect("accept Out");
    output
        .write(&out_greeting(&in_name))
        .expect("write greeting");
    output.flush().expect("flush greeting");

    // --- Client side: accept In and decode the handshake. ---
    input.accept().expect("accept In");
    let mut frames = LegacyPipeCodec::default();
    let mut buffer = [0_u8; 256];
    let mut seen: Vec<Frame> = Vec::new();
    while seen.len() < 2 {
        let count = input.read(&mut buffer).expect("read events");
        assert!(count > 0, "event pipe closed early");
        seen.extend(frames.push(&buffer[..count]).expect("decode events"));
    }
    assert_eq!(seen[0], Frame::Loading);
    assert_eq!(seen[1], Frame::Ready);

    // --- Client side: send a value change and match the response. ---
    let frame = LegacyPipeCodec::encode_set_value(7, "god_mode", 1.0).unwrap();
    output.write(&frame).expect("write value");
    output.flush().expect("flush value");

    let mut response = [0_u8; 24];
    let mut filled = 0;
    while filled < response.len() {
        let count = input.read(&mut buffer).expect("read response");
        assert!(count > 0, "event pipe closed before the response");
        let take = count.min(response.len() - filled);
        response[filled..filled + take].copy_from_slice(&buffer[..take]);
        filled += take;
    }
    let mut responses = LegacyPipeCodec::default();
    let decoded = responses.push(&response).expect("decode response");
    assert_eq!(
        decoded[0],
        Frame::ValueResponse {
            request_id: 7,
            result: 0
        }
    );

    // --- Client side: decode the log channel. ---
    log.accept().expect("accept Log");
    let mut log_codec = LogCodec::default();
    let mut messages = Vec::new();
    let mut log_buffer = [0_u8; 512];
    for _ in 0..40 {
        let count = log.read(&mut log_buffer).expect("read log");
        messages.extend(log_codec.push(&log_buffer[..count]));
        if !messages.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].level, LOG_INFO);
    assert_eq!(messages[0].message, "god_mode set to 1 by client.");

    trainer.join().expect("simulated trainer thread");
}

#[test]
fn two_sessions_use_distinct_pipe_names() {
    let (a_in, a_out, a_log) = session_pipe_names(1);
    let (b_in, b_out, b_log) = session_pipe_names(2);
    for left in [&a_in, &a_out, &a_log] {
        for right in [&b_in, &b_out, &b_log] {
            assert_ne!(left, right);
        }
    }
    // Distinct names must be independently creatable (no sharing violation).
    let first = PipeServer::create(&a_in).expect("first");
    let second = PipeServer::create(&b_in).expect("second");
    drop(first);
    drop(second);
}
