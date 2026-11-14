use serde::Deserialize;
use std::fs;
use std::io;
use std::path::Path;
#[cfg(windows)]
use std::{mem, net::Ipv4Addr};

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    pid: u32,
    port: u32,
    #[serde(default)]
    service: String,
    #[serde(default)]
    target: String,
}

#[derive(Debug)]
pub enum RegistryError {
    Io(io::Error),
    InvalidPort(u32),
}

impl From<io::Error> for RegistryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads Wand's local Tophat port registry without opening a connection.
/// Callers must verify that the PID is live and belongs to the intended target.
pub fn find_port(
    registry_path: &Path,
    process_name: &str,
    process_id: Option<u32>,
) -> Result<Option<u16>, RegistryError> {
    let json = match fs::read(registry_path) {
        Ok(json) => json,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    // Wand writes this file while services start or stop. An incomplete read is retried later.
    let Ok(entries) = serde_json::from_slice::<Vec<RegistryEntry>>(&json) else {
        return Ok(None);
    };
    let found = if let Some(pid) = process_id {
        entries.iter().find(|entry| entry.pid == pid)
    } else {
        entries
            .iter()
            .find(|entry| entry.target.eq_ignore_ascii_case(process_name))
            .or_else(|| {
                entries
                    .iter()
                    .find(|entry| entry.service.eq_ignore_ascii_case(process_name))
            })
    };
    match found {
        Some(entry) if (1..=u16::MAX as u32).contains(&entry.port) => Ok(Some(entry.port as u16)),
        Some(entry) => Err(RegistryError::InvalidPort(entry.port)),
        None => Ok(None),
    }
}

/// Returns a registered port only when the same PID owns a live loopback-capable TCP listener.
/// The caller still has to establish that `process_id` belongs to the selected game or host.
#[cfg(windows)]
pub fn find_verified_port(
    registry_path: &Path,
    process_id: u32,
) -> Result<Option<u16>, RegistryError> {
    let Some(port) = find_port(registry_path, "", Some(process_id))? else {
        return Ok(None);
    };
    let rows = tcp_listeners()?;
    Ok(rows
        .iter()
        .any(|row| {
            row.pid == process_id
                && row.port == port
                && (row.address.is_loopback() || row.address.is_unspecified())
        })
        .then_some(port))
}

#[cfg(windows)]
struct TcpListenerRow {
    pid: u32,
    port: u16,
    address: Ipv4Addr,
}

#[cfg(windows)]
fn tcp_listeners() -> Result<Vec<TcpListenerRow>, RegistryError> {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_LISTEN, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    let mut size = 0_u32;
    // SAFETY: a null buffer requests its required size; `size` is a valid output pointer.
    let first = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if first != ERROR_INSUFFICIENT_BUFFER && first != 0 {
        return Err(RegistryError::Io(io::Error::from_raw_os_error(
            first as i32,
        )));
    }
    for _ in 0..3 {
        if size < mem::size_of::<MIB_TCPTABLE_OWNER_PID>() as u32 || size > 8 * 1024 * 1024 {
            return Err(RegistryError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid TCP table size",
            )));
        }
        let mut storage = vec![0_u32; (size as usize).div_ceil(4)];
        let mut returned_size = size;
        // SAFETY: `storage` is writable and at least `size` bytes long. Windows writes `returned_size`.
        let result = unsafe {
            GetExtendedTcpTable(
                storage.as_mut_ptr().cast(),
                &mut returned_size,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            size = returned_size;
            continue;
        }
        if result != 0 {
            return Err(RegistryError::Io(io::Error::from_raw_os_error(
                result as i32,
            )));
        }
        let byte_len = returned_size as usize;
        let count = storage[0] as usize;
        let offset = mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table);
        let row_size = mem::size_of::<MIB_TCPROW_OWNER_PID>();
        if count
            .checked_mul(row_size)
            .and_then(|bytes| offset.checked_add(bytes))
            .is_none_or(|end| end > byte_len)
        {
            return Err(RegistryError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid TCP table rows",
            )));
        }
        let mut rows = Vec::new();
        for index in 0..count {
            // SAFETY: the count and row extent were bounds checked above. The returned table is DWORD aligned.
            let row = unsafe {
                std::ptr::read_unaligned(
                    (storage.as_ptr() as *const u8)
                        .add(offset + index * row_size)
                        .cast::<MIB_TCPROW_OWNER_PID>(),
                )
            };
            if row.dwState as i32 == MIB_TCP_STATE_LISTEN {
                rows.push(TcpListenerRow {
                    pid: row.dwOwningPid,
                    port: u16::from_be(row.dwLocalPort as u16),
                    address: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()),
                });
            }
        }
        return Ok(rows);
    }
    Err(RegistryError::Io(io::Error::new(
        io::ErrorKind::WouldBlock,
        "TCP listener table changed repeatedly",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_target_and_pid_without_falling_back_to_another_process() {
        let path =
            std::env::temp_dir().join(format!("wand-rust-registry-{}.json", std::process::id()));
        fs::write(&path, r#"[{"pid":12,"port":50123,"service":"tophat_service","target":"Stick It To The Stickman"},{"pid":13,"port":50124,"service":"tophat_service","target":"Other Game"}]"#).unwrap();
        assert_eq!(
            find_port(&path, "stick it to the stickman", None).unwrap(),
            Some(50123)
        );
        assert_eq!(find_port(&path, "ignored", Some(13)).unwrap(), Some(50124));
        assert_eq!(find_port(&path, "ignored", Some(99)).unwrap(), None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_invalid_port() {
        let path = std::env::temp_dir().join(format!(
            "wand-rust-registry-invalid-{}.json",
            std::process::id()
        ));
        fs::write(&path, r#"[{"pid":12,"port":70000,"target":"Game"}]"#).unwrap();
        assert!(matches!(
            find_port(&path, "Game", None),
            Err(RegistryError::InvalidPort(70000))
        ));
        fs::remove_file(path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verified_lookup_rejects_stale_ports_and_accepts_matching_listener() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let path = std::env::temp_dir().join(format!(
            "wand-rust-live-registry-{}.json",
            std::process::id()
        ));
        fs::write(
            &path,
            format!(
                r#"[{{"pid":{},"port":{},"target":"test"}}]"#,
                std::process::id(),
                port
            ),
        )
        .unwrap();
        assert_eq!(
            find_verified_port(&path, std::process::id()).unwrap(),
            Some(port)
        );
        drop(listener);
        assert_eq!(find_verified_port(&path, std::process::id()).unwrap(), None);
        fs::remove_file(path).unwrap();
    }
}
