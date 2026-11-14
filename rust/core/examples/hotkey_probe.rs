//! Diagnostic probe for the global hotkey hook.
//!
//! Starts the service, registers `F9`, synthesises `F9` and reports whether the
//! hook observed it. Run with:
//!
//! ```powershell
//! cargo run --release --example hotkey_probe
//! ```
//!
//! This is a diagnostic tool, not a trainer feature.

use std::time::Duration;
use wand_rebuild_core::hotkeys::{HotkeyBinding, HotkeyEvent, HotkeyService};

fn main() {
    // Synthetic input is filtered in normal operation; allow it for the probe.
    unsafe {
        std::env::set_var("WAND_HOTKEYS_ALLOW_INJECTED", "1");
        std::env::set_var("WAND_HOTKEYS_DEBUG", "1");
    }

    let (service, events) = match HotkeyService::start() {
        Ok(pair) => pair,
        Err(error) => {
            println!("service failed to start: {error}");
            std::process::exit(1);
        }
    };
    let binding = HotkeyBinding::parse("F9").expect("F9 parses");
    service.set_bindings(vec![("probe".to_owned(), binding)]);
    println!("hook installed; binding F9 -> probe; synthesising F9 in 1s");

    std::thread::sleep(Duration::from_secs(1));
    send_key(0x78);
    println!("F9 sent");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut seen = 0;
    while std::time::Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(HotkeyEvent::Pressed { target }) => {
                println!("PRESSED target={target}");
                seen += 1;
                break;
            }
            Ok(other) => println!("event {other:?}"),
            Err(_) => {}
        }
    }

    // Capture path: begin capture, synthesise Ctrl+F10.
    service.begin_capture();
    println!("capture mode: synthesising Ctrl+F10 in 500ms");
    std::thread::sleep(Duration::from_millis(500));
    send_key(0x11);
    send_key(0x79);
    release_key(0x79);
    release_key(0x11);

    let capture_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < capture_deadline {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(HotkeyEvent::Captured { binding }) => {
                println!("CAPTURED {}", binding.label());
                seen += 1;
                break;
            }
            Ok(other) => println!("event {other:?}"),
            Err(_) => {}
        }
    }

    if seen >= 2 {
        println!("hotkey probe PASSED");
    } else {
        println!("hotkey probe FAILED: {seen}/2 events observed");
        std::process::exit(2);
    }
}

fn send_key(vk: u8) {
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::keybd_event(vk, 0, 0, 0);
    }
    std::thread::sleep(Duration::from_millis(60));
}

fn release_key(vk: u8) {
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::keybd_event(vk, 0, 2, 0);
    }
    std::thread::sleep(Duration::from_millis(60));
}
