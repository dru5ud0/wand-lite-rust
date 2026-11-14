//! Windows global hotkey engine using a low-level keyboard hook.

use std::sync::mpsc;

#[cfg(windows)]
use std::collections::HashSet;
#[cfg(windows)]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(windows)]
use std::thread;

/// One key chord: Windows virtual-key code plus modifier state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyBinding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub vk: u32,
}

impl HotkeyBinding {
    /// Parses a stored form such as "Ctrl+Shift+F1" or "F3". Returns None on junk.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut vk: Option<u32> = None;
        for part in text.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" | "menu" => alt = true,
                _ => {
                    if vk.is_some() {
                        return None;
                    }
                    match key_name_to_vk(part) {
                        Some(code) => vk = Some(code),
                        None => return None,
                    }
                }
            }
        }
        Some(Self {
            ctrl,
            shift,
            alt,
            vk: vk.unwrap_or(0),
        })
    }

    /// Canonical stored/display form, e.g. "Ctrl+Shift+F1", "F3", "Ctrl+-".
    pub fn label(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_owned());
        }
        if self.shift {
            parts.push("Shift".to_owned());
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        let key = vk_to_key_name(self.vk);
        if !key.is_empty() {
            parts.push(key);
        }
        if parts.is_empty() {
            "?".to_owned()
        } else {
            parts.join("+")
        }
    }

    /// True when the chord uses at least one non-modifier key.
    pub fn is_valid(self) -> bool {
        self.vk != 0 && !is_modifier_vk(self.vk)
    }

    pub fn matches(binding: HotkeyBinding, ctrl: bool, shift: bool, alt: bool, vk: u32) -> bool {
        matches(binding, ctrl, shift, alt, vk)
    }
}

pub fn matches(binding: HotkeyBinding, ctrl: bool, shift: bool, alt: bool, vk: u32) -> bool {
    binding.ctrl == ctrl && binding.shift == shift && binding.alt == alt && binding.vk == vk
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed { target: String },
    Captured { binding: HotkeyBinding },
    CaptureCancelled,
}

#[cfg(windows)]
struct HookState {
    tx: Option<mpsc::Sender<HotkeyEvent>>,
    bindings: Vec<(String, HotkeyBinding)>,
    capturing: bool,
    pressed: HashSet<u32>,
}

#[cfg(windows)]
static HOOK_STATE: OnceLock<Arc<Mutex<HookState>>> = OnceLock::new();

/// Test-only escape hatch: real user input never sets `LLKHF_INJECTED`, but
/// automated UI tests synthesise keys with `keybd_event`, which does. Honouring
/// injected input is opt-in so normal operation keeps ignoring synthetic keys.
fn allow_injected_input() -> bool {
    static ALLOW: OnceLock<bool> = OnceLock::new();
    *ALLOW.get_or_init(|| std::env::var_os("WAND_HOTKEYS_ALLOW_INJECTED").is_some())
}

/// Diagnostic switch: logs every key the hook observes.
fn debug_hook() -> bool {
    static DEBUG: OnceLock<bool> = OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var_os("WAND_HOTKEYS_DEBUG").is_some())
}

pub struct HotkeyService {
    #[cfg(windows)]
    thread_id: u32,
    #[cfg(windows)]
    join: Option<thread::JoinHandle<()>>,
}

impl HotkeyService {
    pub fn start() -> std::io::Result<(Self, std::sync::mpsc::Receiver<HotkeyEvent>)> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
                WH_KEYBOARD_LL, WM_QUIT,
            };

            let state = HOOK_STATE.get_or_init(|| {
                Arc::new(Mutex::new(HookState {
                    tx: None,
                    bindings: Vec::new(),
                    capturing: false,
                    pressed: HashSet::new(),
                }))
            });

            let (tx, rx) = mpsc::channel();
            {
                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                guard.tx = Some(tx);
                guard.bindings.clear();
                guard.capturing = false;
                guard.pressed.clear();
            }

            let (ready_tx, ready_rx) = mpsc::channel();
            let join = thread::spawn(move || unsafe {
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), std::ptr::null_mut(), 0);
                if hook.is_null() {
                    let _ = ready_tx.send(Err(std::io::Error::last_os_error()));
                    return;
                }
                let tid = windows_sys::Win32::System::Threading::GetCurrentThreadId();
                let _ = ready_tx.send(Ok(tid));

                let mut msg: MSG = std::mem::zeroed();
                loop {
                    let ret = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                    if ret == 0 || ret == -1 {
                        break;
                    }
                    if msg.message == WM_QUIT {
                        break;
                    }
                    DispatchMessageW(&msg);
                }
                UnhookWindowsHookEx(hook);
            });

            let tid = match ready_rx.recv() {
                Ok(Ok(t)) => t,
                Ok(Err(e)) => {
                    let _ = join.join();
                    return Err(e);
                }
                Err(_) => {
                    let _ = join.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "hotkey worker failed to start",
                    ));
                }
            };

            Ok((
                HotkeyService {
                    thread_id: tid,
                    join: Some(join),
                },
                rx,
            ))
        }
        #[cfg(not(windows))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "global hotkeys are only available on Windows",
            ))
        }
    }

    pub fn set_bindings(&self, bindings: Vec<(String, HotkeyBinding)>) {
        #[cfg(windows)]
        {
            if let Some(state) = HOOK_STATE.get() {
                if let Ok(mut guard) = state.lock() {
                    guard.bindings = bindings;
                }
            }
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1,
                    0,
                    0,
                );
            }
        }
        #[cfg(not(windows))]
        {
            let _ = bindings;
        }
    }

    pub fn begin_capture(&self) {
        #[cfg(windows)]
        {
            if let Some(state) = HOOK_STATE.get() {
                if let Ok(mut guard) = state.lock() {
                    guard.capturing = true;
                }
            }
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1,
                    0,
                    0,
                );
            }
        }
    }

    pub fn cancel_capture(&self) {
        #[cfg(windows)]
        {
            if let Some(state) = HOOK_STATE.get() {
                if let Ok(mut guard) = state.lock() {
                    guard.capturing = false;
                }
            }
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1,
                    0,
                    0,
                );
            }
        }
    }

    pub fn is_capturing(&self) -> bool {
        #[cfg(windows)]
        {
            if let Some(state) = HOOK_STATE.get() {
                if let Ok(guard) = state.lock() {
                    return guard.capturing;
                }
            }
            false
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

#[cfg(windows)]
impl Drop for HotkeyService {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                self.thread_id,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT,
                0,
                0,
            );
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    unsafe {
    use windows_sys::Win32::Foundation::LRESULT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP,
    };

    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    if code != HC_ACTION as i32 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let info = &*(lparam as *const KBDLLHOOKSTRUCT);
    if info.flags & LLKHF_INJECTED != 0 && !allow_injected_input() {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let vk = info.vkCode;
    let key_up = info.flags & LLKHF_UP != 0;

    let ctrl = GetAsyncKeyState(VK_CONTROL as i32) < 0;
    let shift = GetAsyncKeyState(VK_SHIFT as i32) < 0;
    let alt = GetAsyncKeyState(VK_MENU as i32) < 0;
    if debug_hook() {
        eprintln!(
            "[hotkey] vk=0x{vk:02x} up={key_up} ctrl={ctrl} shift={shift} alt={alt} injected={}",
            info.flags & LLKHF_INJECTED != 0
        );
    }

    let state = match HOOK_STATE.get() {
        Some(s) => s,
        None => return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam),
    };
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());

    // Auto-repeat handling.
    if !key_up {
        if guard.pressed.contains(&vk) {
            drop(guard);
            return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
        }
        guard.pressed.insert(vk);
    } else {
        guard.pressed.remove(&vk);
    }

    let mut event: Option<HotkeyEvent> = None;
    let mut suppress = false;

    if !key_up {
        if guard.capturing {
            if vk == 0x1B {
                event = Some(HotkeyEvent::CaptureCancelled);
                guard.capturing = false;
                suppress = true;
            } else if !is_modifier_vk(vk) {
                let binding = HotkeyBinding {
                    ctrl,
                    shift,
                    alt,
                    vk,
                };
                event = Some(HotkeyEvent::Captured { binding });
                guard.capturing = false;
                suppress = true;
            }
        } else if let Some((target, _)) =
            guard.bindings.iter().find(|(_, b)| matches(*b, ctrl, shift, alt, vk))
        {
            event = Some(HotkeyEvent::Pressed {
                target: target.clone(),
            });
            suppress = true;
        }
    } else if !guard.capturing
        && guard
            .bindings
            .iter()
            .any(|(_, b)| matches(*b, ctrl, shift, alt, vk))
    {
        suppress = true;
    }

    if let Some(ev) = event {
        if let Some(tx) = guard.tx.as_ref() {
            let _ = tx.send(ev);
        }
    }
    drop(guard);

    if suppress {
        return 1 as LRESULT;
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }
}
fn is_modifier_vk(vk: u32) -> bool {
    matches!(
        vk,
        0x10 | 0x11 | 0x12 | 0xA0 | 0xA1 | 0xA2 | 0xA3 | 0xA4 | 0xA5
    )
}

fn key_name_to_vk(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f') {
        if let Ok(n) = num.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + (n - 1));
            }
        }
    }
    let chars: Vec<char> = name.chars().collect();
    if chars.len() == 1 {
        let c = chars[0];
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
        match c {
            '-' => return Some(0xBD),
            '=' => return Some(0xBB),
            '[' => return Some(0xDB),
            ']' => return Some(0xDD),
            '\\' => return Some(0xDC),
            ';' => return Some(0xBA),
            '\'' => return Some(0xDE),
            ',' => return Some(0xBC),
            '.' => return Some(0xBE),
            '/' => return Some(0xBF),
            '`' => return Some(0xC0),
            ' ' => return Some(0x20),
            _ => {}
        }
    }
    match lower.as_str() {
        "space" => Some(0x20),
        "tab" => Some(0x09),
        "enter" | "return" => Some(0x0D),
        "backspace" => Some(0x08),
        "escape" | "esc" => Some(0x1B),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "left" => Some(0x25),
        "up" => Some(0x26),
        "right" => Some(0x27),
        "down" => Some(0x28),
        "minus" => Some(0xBD),
        "equals" | "equal" => Some(0xBB),
        _ => None,
    }
}

fn vk_to_key_name(vk: u32) -> String {
    if vk == 0 {
        return String::new();
    }
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", vk - 0x70 + 1);
    }
    if (0x41..=0x5A).contains(&vk) {
        return (vk as u8 as char).to_string();
    }
    if (0x30..=0x39).contains(&vk) {
        return (vk as u8 as char).to_string();
    }
    match vk {
        0x08 => "Backspace".to_owned(),
        0x09 => "Tab".to_owned(),
        0x0D => "Enter".to_owned(),
        0x1B => "Escape".to_owned(),
        0x20 => "Space".to_owned(),
        0x21 => "PageUp".to_owned(),
        0x22 => "PageDown".to_owned(),
        0x23 => "End".to_owned(),
        0x24 => "Home".to_owned(),
        0x25 => "Left".to_owned(),
        0x26 => "Up".to_owned(),
        0x27 => "Right".to_owned(),
        0x28 => "Down".to_owned(),
        0x2D => "Insert".to_owned(),
        0x2E => "Delete".to_owned(),
        0xBD => "-".to_owned(),
        0xBB => "=".to_owned(),
        0xDB => "[".to_owned(),
        0xDD => "]".to_owned(),
        0xDC => "\\".to_owned(),
        0xBA => ";".to_owned(),
        0xDE => "'".to_owned(),
        0xBC => ",".to_owned(),
        0xBE => ".".to_owned(),
        0xBF => "/".to_owned(),
        0xC0 => "`".to_owned(),
        _ => format!("VK{}", vk),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_label_roundtrip_simple() {
        let cases = ["F1", "Ctrl+F1", "Ctrl+Shift+F1", "Alt+-", "Ctrl+-", "F12"];
        for c in cases {
            let b = HotkeyBinding::parse(c).expect(c);
            assert_eq!(b.label(), c, "roundtrip failed for {}", c);
        }
    }

    #[test]
    fn parse_rejects_junk() {
        assert!(HotkeyBinding::parse("").is_none());
        assert!(HotkeyBinding::parse("Ctrl+").is_none());
        assert!(HotkeyBinding::parse("NotAKey").is_none());
    }

    #[test]
    fn is_valid_rejects_modifier_only() {
        let b = HotkeyBinding::parse("Ctrl+Shift").unwrap();
        assert!(!b.is_valid());
    }

    #[test]
    fn matches_exact_modifiers() {
        let b = HotkeyBinding {
            ctrl: true,
            shift: false,
            alt: false,
            vk: 0x70,
        };
        assert!(matches(b, true, false, false, 0x70));
        assert!(!matches(b, true, true, false, 0x70));
        assert!(!matches(b, false, false, false, 0x70));
        assert!(!matches(b, true, false, false, 0x71));
    }

    #[cfg(windows)]
    #[test]
    fn service_capture_state_transitions() {
        let (service, _rx) = HotkeyService::start().expect("start");
        assert!(!service.is_capturing());
        service.begin_capture();
        assert!(service.is_capturing());
        service.cancel_capture();
        assert!(!service.is_capturing());
        service.set_bindings(vec![(
            "target".to_owned(),
            HotkeyBinding {
                ctrl: false,
                shift: false,
                alt: false,
                vk: 0x70,
            },
        )]);
        drop(service);
    }
}
