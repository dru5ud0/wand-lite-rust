//! Windows named-pipe server used by Wand's legacy (`trainerlib`) trainer backend.
//!
//! Wand's own client is the pipe *server*: it creates
//! `\\.\pipe\WeMod\Trainer_<millis>_{In,Out,Log}`, waits for the injected
//! `TrainerLib_{arch}.dll` to connect, and writes the `In` pipe address to the
//! `Out` connection as UTF-16LE. See `work/research/LEGACY_PROTOCOL.md`.
//!
//! This module only wraps the Win32 calls. It does not know about frames.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

/// How long a single blocking read may wait before the caller can observe a
/// cancellation flag. Wand uses byte-mode blocking pipes.
const READ_CHUNK: usize = 64 * 1024;

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A single-instance blocking pipe server.
#[cfg(windows)]
pub struct PipeServer {
    handle: HANDLE,
    name: String,
}

#[cfg(windows)]
impl PipeServer {
    /// Creates the pipe server and returns immediately; the peer may connect
    /// before or after [`PipeServer::accept`] is called.
    pub fn create(name: &str) -> io::Result<Self> {
        if !name.starts_with("\\\\.\\pipe\\") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "named pipe path must start with \\\\.\\pipe\\",
            ));
        }
        let name_wide = wide(name);
        // SAFETY: `name_wide` is NUL terminated and outlives the call. A null
        // security-attributes pointer requests the default DACL, which permits
        // the same user (the injected trainer runs in the game's user context).
        let handle = unsafe {
            CreateNamedPipeW(
                name_wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                READ_CHUNK as u32,
                READ_CHUNK as u32,
                0,
                ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            name: name.to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Blocks until the trainer connects.
    pub fn accept(&self) -> io::Result<()> {
        // SAFETY: `self.handle` is a live pipe handle owned by this struct.
        let connected = unsafe { ConnectNamedPipe(self.handle, ptr::null_mut()) };
        if connected != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            return Ok(());
        }
        Err(error)
    }

    /// Reads whatever bytes are currently available. Returns `Ok(0)` when the
    /// peer closed the pipe.
    pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0_u32;
        // SAFETY: `buffer` is valid for `buffer.len()` bytes and `read` is a
        // valid out pointer.
        let ok = unsafe {
            ReadFile(
                self.handle,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(read as usize)
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut written = 0_u32;
        // SAFETY: `bytes` is valid for `bytes.len()` bytes.
        let ok = unsafe {
            WriteFile(
                self.handle,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                &mut written,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written as usize != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short write to trainer pipe",
            ));
        }
        Ok(())
    }

    pub fn flush(&self) -> io::Result<()> {
        // SAFETY: live handle owned by this struct.
        if unsafe { FlushFileBuffers(self.handle) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Disconnects the current peer so the handle can be dropped cleanly.
    pub fn disconnect(&self) {
        // SAFETY: live handle owned by this struct; a failure here only means
        // there was no connected peer, which is not actionable.
        unsafe {
            DisconnectNamedPipe(self.handle);
        }
    }
}

// SAFETY: a Windows pipe `HANDLE` may be used from any thread. Reads and writes
// on a duplex pipe are independent operations, which is how the session uses it
// (one reader thread, writes from the controller thread).
#[cfg(windows)]
unsafe impl Send for PipeServer {}
#[cfg(windows)]
unsafe impl Sync for PipeServer {}

#[cfg(windows)]
impl Drop for PipeServer {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is owned by this struct and closed exactly once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Builds the pipe name family Wand uses for one trainer session.
///
/// `token` is the millisecond timestamp the reference client embeds in the
/// name. The three names are returned as `(in, out, log)`.
pub fn session_pipe_names(token: u128) -> (String, String, String) {
    let prefix = format!("\\\\.\\pipe\\WeMod\\Trainer_{token}_");
    (
        format!("{prefix}In"),
        format!("{prefix}Out"),
        format!("{prefix}Log"),
    )
}

/// Encodes the `Out`-pipe greeting: the `In` pipe path as UTF-16LE with a
/// terminating NUL, exactly as the reference client writes it.
pub fn out_greeting(in_pipe_name: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((in_pipe_name.len() + 1) * 2);
    for unit in in_pipe_name.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

/// Re-exported so callers can express access rights without importing
/// `windows-sys` themselves.
pub const PIPE_ACCESS: u32 = GENERIC_READ | GENERIC_WRITE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_match_the_reference_client_layout() {
        let (input, output, log) = session_pipe_names(1_794_597_322_216);
        assert_eq!(input, "\\\\.\\pipe\\WeMod\\Trainer_1794597322216_In");
        assert_eq!(output, "\\\\.\\pipe\\WeMod\\Trainer_1794597322216_Out");
        assert_eq!(log, "\\\\.\\pipe\\WeMod\\Trainer_1794597322216_Log");
    }

    #[test]
    fn greeting_is_utf16le_with_nul_terminator() {
        let greeting = out_greeting("\\\\.\\pipe\\WeMod\\Trainer_1_In");
        assert_eq!(greeting.len() % 2, 0);
        let text: Vec<u16> = greeting
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(text.last(), Some(&0));
        let decoded = String::from_utf16(&text[..text.len() - 1]).unwrap();
        assert_eq!(decoded, "\\\\.\\pipe\\WeMod\\Trainer_1_In");
    }

    #[cfg(windows)]
    #[test]
    fn rejects_names_outside_the_pipe_namespace() {
        assert!(PipeServer::create("C:\\temp\\not-a-pipe").is_err());
    }
}
