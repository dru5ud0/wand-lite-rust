use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTrainer {
    pub trainer_id: u64,
    pub path: PathBuf,
    pub sha256: String,
}

/// Finds a locally cached Wand trainer DLL whose filename digest matches its bytes.
/// This is a read-only check; it does not load or execute the DLL.
pub fn find(trainer_directory: &Path, trainer_id: u64) -> io::Result<Option<CachedTrainer>> {
    let directory = match trainer_directory.canonicalize() {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(digest_prefix) = parse_file_name(&file_name, trainer_id) else {
            continue;
        };
        let path = entry.path().canonicalize()?;
        if !path.starts_with(&directory) {
            continue;
        }
        let sha256 = hash_file(&path)?;
        if sha256.starts_with(&digest_prefix) {
            found.push(CachedTrainer {
                trainer_id,
                path,
                sha256,
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found.pop())
}

fn parse_file_name(file_name: &str, trainer_id: u64) -> Option<String> {
    let stem = file_name.strip_suffix(".dll")?;
    let digest_prefix = stem.strip_prefix(&format!("Trainer_{trainer_id}_"))?;
    (digest_prefix.len() == 10 && digest_prefix.bytes().all(|ch| ch.is_ascii_hexdigit()))
        .then(|| digest_prefix.to_ascii_lowercase())
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_matching_cached_binary_and_rejects_tampering() {
        let directory =
            std::env::temp_dir().join(format!("wand-trainer-cache-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("Trainer_50696_2cf24dba5f.dll");
        fs::write(&file, b"hello").unwrap();
        let trainer = find(&directory, 50696).unwrap().unwrap();
        assert_eq!(trainer.trainer_id, 50696);
        assert_eq!(
            trainer.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(find(&directory, 12345).unwrap().is_none());
        fs::write(&file, b"tampered").unwrap();
        assert!(find(&directory, 50696).unwrap().is_none());
        fs::remove_dir_all(directory).unwrap();
    }
}
