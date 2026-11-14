//! DLL injection and remote-thread bootstrap for Wand's legacy trainer backend.
//!
//! Recovered contract (see `work/research/LEGACY_PROTOCOL.md`):
//!
//! 1. `LoadLibraryW(TrainerLib_{arch}.dll)` inside the target process.
//! 2. `CreateRemoteThread` at `TrainerLib`'s exported `Run`, passing a fixed
//!    bootstrap struct that carries the pipe paths, trainer DLL path, target
//!    PID, game version and the trainer's variable names.
//!
//! Everything here is Windows-only except the struct encoder and the PE export
//! reader, which are unit tested without touching a live process.

/// Size of the fixed bootstrap header, before the variable array.
pub const BOOTSTRAP_HEADER: usize = 1548;
/// Total buffer length for `variables.len()` variable slots.
pub const BOOTSTRAP_TRAILER: usize = 4;
/// Each variable name occupies a fixed 32-byte ASCII slot.
pub const VARIABLE_SLOT: usize = 32;
/// Maximum variable name length that fits a slot without truncation.
pub const VARIABLE_MAX: usize = VARIABLE_SLOT - 1;

/// Trainer feature flags recovered from the reference client
/// (`$R` in bundle module `47975`).
pub mod flags {
    pub const SUSPEND_PROCESS: u32 = 1;
    pub const RESUME_PROCESS: u32 = 2;
    pub const HIDE_THREADS: u32 = 4;
    pub const MASK_MODULES: u32 = 8;
    pub const USE_EXTERNAL_HOST: u32 = 16;
}

/// Inputs for the bootstrap struct the trainer's `Run` export receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapSpec {
    pub log_pipe: String,
    pub message_pipe: String,
    pub flags: u32,
    pub trainer_dll: String,
    pub target_process_id: u32,
    pub game_version: u32,
    pub variables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapError {
    FieldTooLong(&'static str, usize),
    InvalidVariable(String),
    TooManyVariables(usize),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FieldTooLong(field, length) => {
                write!(formatter, "{field} is {length} bytes, too long for its slot")
            }
            Self::InvalidVariable(name) => {
                write!(
                    formatter,
                    "variable name {name:?} is not a plain ASCII identifier"
                )
            }
            Self::TooManyVariables(count) => write!(formatter, "{count} variables exceed the limit"),
        }
    }
}

impl std::error::Error for BootstrapError {}

/// Maximum variables that keep the bootstrap buffer below one 64 KiB region.
pub const MAX_VARIABLES: usize = 1900;

fn write_utf16_field(
    destination: &mut [u8],
    value: &str,
    field: &'static str,
) -> Result<(), BootstrapError> {
    // Reserve one UTF-16 unit for the NUL terminator.
    let capacity_units = destination.len() / 2 - 1;
    let units: Vec<u16> = value.encode_utf16().collect();
    if units.len() > capacity_units {
        return Err(BootstrapError::FieldTooLong(field, units.len() * 2));
    }
    for (index, unit) in units.iter().enumerate() {
        destination[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

/// Encodes the exact struct layout the reference client passes to
/// `TrainerLib_{arch}.dll!Run`.
pub fn encode_bootstrap(spec: &BootstrapSpec) -> Result<Vec<u8>, BootstrapError> {
    if spec.variables.len() > MAX_VARIABLES {
        return Err(BootstrapError::TooManyVariables(spec.variables.len()));
    }
    for variable in &spec.variables {
        // The reference client writes names as ASCII into fixed 32-byte slots,
        // so anything outside a conservative identifier set is a blueprint bug
        // that would silently corrupt the trainer's variable table.
        let valid = !variable.is_empty()
            && variable.len() <= VARIABLE_MAX
            && variable
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'));
        if !valid {
            return Err(BootstrapError::InvalidVariable(variable.clone()));
        }
    }
    let mut buffer =
        vec![0_u8; BOOTSTRAP_HEADER + BOOTSTRAP_TRAILER + VARIABLE_SLOT * spec.variables.len()];
    write_utf16_field(&mut buffer[0..512], &spec.log_pipe, "log pipe")?;
    write_utf16_field(&mut buffer[512..768], &spec.message_pipe, "message pipe")?;
    buffer[768..772].copy_from_slice(&spec.flags.to_le_bytes());
    // 772..1024 is reserved and stays zero.
    write_utf16_field(&mut buffer[1024..1536], &spec.trainer_dll, "trainer dll path")?;
    buffer[1536..1540].copy_from_slice(&spec.target_process_id.to_le_bytes());
    buffer[1540..1544].copy_from_slice(&spec.game_version.to_le_bytes());
    buffer[1544..1548].copy_from_slice(&(spec.variables.len() as u32).to_le_bytes());
    let mut offset = BOOTSTRAP_HEADER;
    for variable in &spec.variables {
        buffer[offset..offset + variable.len()].copy_from_slice(variable.as_bytes());
        offset += VARIABLE_SLOT;
    }
    Ok(buffer)
}

/// Reads a NUL terminated UTF-16LE field from an encoded bootstrap buffer.
pub fn read_utf16_field(bytes: &[u8]) -> String {
    let mut units = Vec::new();
    for pair in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

/// Minimal PE export reader: returns the RVA of a named export.
///
/// Used to resolve `TrainerLib_{arch}.dll!Run` so the remote thread can start
/// at the right address inside the already-loaded module.
pub fn export_rva(pe: &[u8], name: &str) -> Option<u32> {
    let read_u16 = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(pe.get(at..at + 2)?.try_into().ok()?))
    };
    let read_u32 = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(pe.get(at..at + 4)?.try_into().ok()?))
    };
    if read_u16(0)? != 0x5a4d {
        return None;
    }
    let pe_offset = read_u32(0x3c)? as usize;
    if pe.get(pe_offset..pe_offset + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe_offset + 4;
    let section_count = read_u16(coff + 2)? as usize;
    let optional_size = read_u16(coff + 16)? as usize;
    let optional = coff + 20;
    let magic = read_u16(optional)?;
    // PE32 uses 0x10b, PE32+ uses 0x20b; the export directory is at the same
    // data-directory index in both.
    let data_directory = match magic {
        0x10b => optional + 96,
        0x20b => optional + 112,
        _ => return None,
    };
    let export_rva = read_u32(data_directory)?;
    let export_size = read_u32(data_directory + 4)?;
    if export_rva == 0 || export_size == 0 {
        return None;
    }

    let sections = optional + optional_size;
    let rva_to_offset = |rva: u32| -> Option<usize> {
        for index in 0..section_count {
            let header = sections + index * 40;
            let virtual_size = read_u32(header + 8)?;
            let virtual_address = read_u32(header + 12)?;
            let raw_size = read_u32(header + 16)?;
            let raw_pointer = read_u32(header + 20)?;
            let extent = virtual_size.max(raw_size);
            if rva >= virtual_address && rva < virtual_address.checked_add(extent)? {
                let delta = rva - virtual_address;
                return Some((raw_pointer + delta) as usize);
            }
        }
        None
    };

    let export_offset = rva_to_offset(export_rva)?;
    let name_count = read_u32(export_offset + 24)? as usize;
    let names_rva = read_u32(export_offset + 32)?;
    let ordinals_rva = read_u32(export_offset + 36)?;
    let functions_rva = read_u32(export_offset + 28)?;
    let names_offset = rva_to_offset(names_rva)?;
    let ordinals_offset = rva_to_offset(ordinals_rva)?;
    let functions_offset = rva_to_offset(functions_rva)?;

    for index in 0..name_count.min(4096) {
        let name_pointer = read_u32(names_offset + index * 4)?;
        let name_offset = rva_to_offset(name_pointer)?;
        let end = pe.get(name_offset..)?.iter().position(|byte| *byte == 0)?;
        if pe.get(name_offset..name_offset + end)? == name.as_bytes() {
            let ordinal = read_u16(ordinals_offset + index * 2)? as usize;
            return read_u32(functions_offset + ordinal * 4);
        }
    }
    None
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use super::{BootstrapSpec, encode_bootstrap, export_rva};
    use std::ffi::{OsStr, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE,
        TH32CS_SNAPMODULE32,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleW, GetProcAddress,
    };
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::Threading::{
        CreateRemoteThread, GetExitCodeThread, OpenProcess, PROCESS_CREATE_THREAD,
        PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
        WaitForSingleObject,
    };

    const INJECT_TIMEOUT_MS: u32 = 15_000;

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Owned remote allocation inside the target process.
    struct RemoteAllocation {
        process: HANDLE,
        address: *mut c_void,
        size: usize,
    }

    impl RemoteAllocation {
        fn new(process: HANDLE, size: usize) -> io::Result<Self> {
            // SAFETY: `process` is a live handle with PROCESS_VM_OPERATION.
            let address = unsafe {
                VirtualAllocEx(
                    process,
                    ptr::null(),
                    size,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            if address.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                process,
                address,
                size,
            })
        }

        fn write(&self, bytes: &[u8]) -> io::Result<()> {
            if bytes.len() > self.size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "payload larger than remote allocation",
                ));
            }
            let mut written = 0_usize;
            // SAFETY: the destination is a live remote allocation of `self.size`
            // bytes and `bytes` is a valid local buffer.
            let ok = unsafe {
                WriteProcessMemory(
                    self.process,
                    self.address,
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    &mut written,
                )
            };
            if ok == 0 || written != bytes.len() {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for RemoteAllocation {
        fn drop(&mut self) {
            // SAFETY: the address was returned by VirtualAllocEx on this process
            // handle and is released exactly once here.
            unsafe {
                VirtualFreeEx(self.process, self.address, 0, MEM_RELEASE);
            }
        }
    }

    /// Waits until the module is visible in the target, or the loader thread
    /// exits, whichever happens first.
    ///
    /// The reference client waits for the *module* rather than for the remote
    /// thread to exit: a trainer DLL is free to keep work running from its
    /// `DllMain`, in which case `LoadLibraryW` does not return promptly.
    fn wait_for_module_or_thread(
        process_id: u32,
        module_file_name: &str,
        thread: HANDLE,
    ) -> io::Result<usize> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(INJECT_TIMEOUT_MS as u64);
        while std::time::Instant::now() < deadline {
            if let Ok(base) = module_base(process_id, module_file_name) {
                return Ok(base);
            }
            // SAFETY: `thread` is a live handle for the duration of this call.
            let signaled = unsafe { WaitForSingleObject(thread, 50) } == WAIT_OBJECT_0;
            if signaled {
                // The loader thread finished; the module must be present now.
                return module_base(process_id, module_file_name).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "LoadLibraryW returned but {module_file_name} is not loaded: {error}"
                        ),
                    )
                });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{module_file_name} did not appear in process {process_id} within {}s",
                INJECT_TIMEOUT_MS / 1000
            ),
        ))
    }

    /// Resolves the load address of a module already loaded in `process`.
    fn module_base(process_id: u32, module_file_name: &str) -> io::Result<usize> {
        // SAFETY: the snapshot API takes no pointers here.
        let snapshot =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, process_id) };
        if snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut entry: MODULEENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
            // SAFETY: `entry` is a valid, correctly sized MODULEENTRY32W.
            if unsafe { Module32FirstW(snapshot, &mut entry) } == 0 {
                return Err(io::Error::last_os_error());
            }
            loop {
                let end = entry
                    .szModule
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(entry.szModule.len());
                let name = String::from_utf16_lossy(&entry.szModule[..end]);
                if name.eq_ignore_ascii_case(module_file_name) {
                    return Ok(entry.modBaseAddr as usize);
                }
                // SAFETY: same valid entry as above.
                if unsafe { Module32NextW(snapshot, &mut entry) } == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("module {module_file_name} is not loaded in process {process_id}"),
                    ));
                }
            }
        })();
        // SAFETY: the snapshot handle is owned by this function.
        unsafe {
            CloseHandle(snapshot);
        }
        result
    }

    fn kernel32_export(name: &std::ffi::CStr) -> io::Result<usize> {
        let kernel32 = wide("kernel32.dll");
        // SAFETY: `kernel32` is NUL terminated; the module is always loaded.
        let module = unsafe { GetModuleHandleW(kernel32.as_ptr()) };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the requested export is a documented kernel32 entry point.
        let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) };
        address
            .map(|function| function as usize)
            .ok_or_else(|| io::Error::last_os_error())
    }

    fn load_library_address() -> io::Result<usize> {
        kernel32_export(c"LoadLibraryW")
    }

    /// Best-effort `FreeLibrary` of an already-loaded trainer runtime.
    ///
    /// A client that crashed (or was killed) leaves `TrainerLib` loaded in the
    /// game with a dead pipe messenger, so a new session cannot initialise. If
    /// the module has no live references this unloads it and a fresh injection
    /// works; if it refuses to unload the caller must ask for a game restart.
    fn unload_module(process: HANDLE, process_id: u32, module_name: &str) -> io::Result<bool> {
        let Ok(base) = module_base(process_id, module_name) else {
            return Ok(true);
        };
        let free_library = kernel32_export(c"FreeLibrary")?;
        // SAFETY: standard remote-thread call with a scalar argument; `base` is
        // the module handle the loader reported for this process.
        let thread = unsafe {
            CreateRemoteThread(
                process,
                ptr::null(),
                0,
                Some(std::mem::transmute::<
                    usize,
                    unsafe extern "system" fn(*mut c_void) -> u32,
                >(free_library)),
                base as *mut c_void,
                0,
                ptr::null_mut(),
            )
        };
        if thread.is_null() {
            return Ok(false);
        }
        // SAFETY: `thread` is a live handle owned by this function.
        let waited = unsafe { WaitForSingleObject(thread, 5_000) };
        // SAFETY: the handle is owned by this function.
        unsafe {
            CloseHandle(thread);
        }
        if waited != WAIT_OBJECT_0 {
            return Ok(false);
        }
        Ok(module_base(process_id, module_name).is_err())
    }

    /// Injects `trainerlib` into `process_id` and starts its `Run` export with
    /// the encoded bootstrap struct.
    pub fn inject_trainer(
        process_id: u32,
        trainerlib: &Path,
        trainer_dll: &Path,
        spec: &BootstrapSpec,
    ) -> io::Result<()> {
        let access = PROCESS_CREATE_THREAD
            | PROCESS_QUERY_INFORMATION
            | PROCESS_VM_OPERATION
            | PROCESS_VM_WRITE
            | PROCESS_VM_READ;
        // SAFETY: `OpenProcess` takes plain scalar arguments.
        let process = unsafe { OpenProcess(access, 0, process_id) };
        if process.is_null() {
            return Err(io::Error::new(
                io::Error::last_os_error().kind(),
                format!(
                    "OpenProcess({process_id}) failed: {} (run the client at the same integrity level as the game)",
                    io::Error::last_os_error()
                ),
            ));
        }

        let result = (|| {
            let module_name = trainerlib
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "trainerlib has no file name")
                })?;

            // A trainer runtime can only be initialised once per game launch: its
            // `Run` export installs hooks and worker threads that survive any
            // attempt to release the module, so a second session in the same
            // process never becomes ready. Fail fast with an actionable message
            // instead of waiting for the connect timeout.
            //
            // `WANDR_FORCE_TRAINER_RELOAD=1` attempts a best-effort `FreeLibrary`
            // first; it works sometimes but is not reliable enough to default to.
            if module_base(process_id, module_name).is_ok() {
                let force_reload = std::env::var_os("WANDR_FORCE_TRAINER_RELOAD").is_some();
                let released = force_reload
                    && unload_module(process, process_id, module_name).unwrap_or(false);
                if !released {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "a trainer runtime from an earlier session is still loaded in \
                             process {process_id}. Restart the game, then load the trainer again."
                        ),
                    ));
                }
            }

            let trainerlib_wide = wide(&trainerlib.to_string_lossy());
            let path_bytes: Vec<u8> = trainerlib_wide
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            let path_allocation = RemoteAllocation::new(process, path_bytes.len())?;
            path_allocation.write(&path_bytes)?;

            let load_library = load_library_address()?;
            // SAFETY: standard remote-thread injection with a live process
            // handle and a remote argument block.
            let thread = unsafe {
                CreateRemoteThread(
                    process,
                    ptr::null(),
                    0,
                    Some(std::mem::transmute::<
                        usize,
                        unsafe extern "system" fn(*mut c_void) -> u32,
                    >(load_library)),
                    path_allocation.address,
                    0,
                    ptr::null_mut(),
                )
            };
            if thread.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("CreateRemoteThread(LoadLibraryW) failed: {}", io::Error::last_os_error()),
                ));
            }
            let module_name = trainerlib
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "trainerlib has no file name")
                })?;
            let base = wait_for_module_or_thread(process_id, module_name, thread)?;
            // SAFETY: the thread handle is owned by this function and the
            // loader thread is allowed to keep running.
            unsafe {
                CloseHandle(thread);
            }
            let image = std::fs::read(trainerlib)?;
            let run_rva = export_rva(&image, "Run").ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{module_name} does not export Run"),
                )
            })?;
            let run_address = base + run_rva as usize;

            let bootstrap = encode_bootstrap(spec)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
            let bootstrap_allocation = RemoteAllocation::new(process, bootstrap.len())?;
            bootstrap_allocation.write(&bootstrap)?;

            // SAFETY: same pattern as above, now calling the resolved Run export.
            let thread = unsafe {
                CreateRemoteThread(
                    process,
                    ptr::null(),
                    0,
                    Some(std::mem::transmute::<
                        usize,
                        unsafe extern "system" fn(*mut c_void) -> u32,
                    >(run_address)),
                    bootstrap_allocation.address,
                    0,
                    ptr::null_mut(),
                )
            };
            if thread.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("CreateRemoteThread(Run) failed: {}", io::Error::last_os_error()),
                ));
            }
            // `Run` is expected to return after installing its hooks, but a
            // trainer is allowed to keep its own threads alive, so a timeout
            // here is not fatal once the pipes are up.
            // SAFETY: `thread` is a live handle for the duration of this call.
            let waited = unsafe { WaitForSingleObject(thread, INJECT_TIMEOUT_MS) };
            if waited != WAIT_OBJECT_0 {
                let mut code = 0_u32;
                // SAFETY: `code` is a valid out pointer.
                unsafe {
                    GetExitCodeThread(thread, &mut code);
                }
                if code == 0 {
                    // Thread is still running; that is normal for a trainer
                    // that owns long-lived worker threads.
                }
            }
            // SAFETY: the thread handle is owned by this function.
            unsafe {
                CloseHandle(thread);
            }

            // Keep the bootstrap allocation alive until Run has returned, then
            // release it explicitly before the process handle is closed.
            drop(bootstrap_allocation);
            drop(path_allocation);
            let _ = trainer_dll;
            Ok(())
        })();

        // SAFETY: the process handle is owned by this function.
        unsafe {
            CloseHandle(process);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(variables: Vec<&str>) -> BootstrapSpec {
        BootstrapSpec {
            log_pipe: "\\\\.\\pipe\\WeMod\\Trainer_1_Log".to_owned(),
            message_pipe: "\\\\.\\pipe\\WeMod\\Trainer_1_In".to_owned(),
            flags: 0,
            trainer_dll: "C:\\trainers\\Trainer_50696_f0de8ff90c.dll".to_owned(),
            target_process_id: 4242,
            game_version: 2_639_440_150,
            variables: variables.into_iter().map(str::to_owned).collect(),
        }
    }

    #[test]
    fn bootstrap_layout_matches_the_recovered_offsets() {
        let encoded = encode_bootstrap(&spec(vec!["god_mode", "instant_kill"])).unwrap();
        assert_eq!(encoded.len(), 1548 + 4 + 32 * 2);
        assert_eq!(
            read_utf16_field(&encoded[0..512]),
            "\\\\.\\pipe\\WeMod\\Trainer_1_Log"
        );
        assert_eq!(
            read_utf16_field(&encoded[512..768]),
            "\\\\.\\pipe\\WeMod\\Trainer_1_In"
        );
        assert_eq!(u32::from_le_bytes(encoded[768..772].try_into().unwrap()), 0);
        assert_eq!(
            read_utf16_field(&encoded[1024..1536]),
            "C:\\trainers\\Trainer_50696_f0de8ff90c.dll"
        );
        assert_eq!(
            u32::from_le_bytes(encoded[1536..1540].try_into().unwrap()),
            4242
        );
        assert_eq!(
            u32::from_le_bytes(encoded[1540..1544].try_into().unwrap()),
            2_639_440_150
        );
        assert_eq!(u32::from_le_bytes(encoded[1544..1548].try_into().unwrap()), 2);
        assert_eq!(&encoded[1548..1556], b"god_mode");
        assert_eq!(&encoded[1580..1592], b"instant_kill");
        assert!(encoded[1556..1580].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn bootstrap_rejects_names_that_cannot_fit_a_slot() {
        let long = "x".repeat(32);
        assert!(matches!(
            encode_bootstrap(&spec(vec![&long])),
            Err(BootstrapError::InvalidVariable(_))
        ));
        assert!(matches!(
            encode_bootstrap(&spec(vec!["ok", "bad name"])),
            Err(BootstrapError::InvalidVariable(_))
        ));
        assert!(matches!(
            encode_bootstrap(&spec(vec![""])),
            Err(BootstrapError::InvalidVariable(_))
        ));
        // Wand's own variable naming convention must stay accepted.
        assert!(encode_bootstrap(&spec(vec!["wm_event_on_death", "god_mode"])).is_ok());
        assert!(matches!(
            encode_bootstrap(&spec(vec!["x"; MAX_VARIABLES + 1])),
            Err(BootstrapError::TooManyVariables(_))
        ));
    }

    #[test]
    fn export_reader_rejects_non_pe_input() {
        assert_eq!(export_rva(b"not a pe image at all", "Run"), None);
        assert_eq!(export_rva(&[], "Run"), None);
    }
}
