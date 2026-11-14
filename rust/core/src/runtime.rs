use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const REQUIRED_TRAINER_FILES: &[&str] = &[
    "TrainerHost_x64.exe",
    "TrainerHost_x86.exe",
    "TrainerLib_x64.dll",
    "TrainerLib_x86.dll",
    "TrainerLibPlugin_x64.dll",
    "TrainerLibPlugin_x86.dll",
    "InputCapturePlugin_x64.dll",
    "InputCapturePlugin_x86.dll",
    "CELib_x64.dll",
    "CELib_x86.dll",
    "tophat_service_x64.dll",
    "tophat_service_x86.dll",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WandRuntime {
    pub version: Vec<u32>,
    pub app_directory: PathBuf,
    pub trainer_directory: PathBuf,
    pub archive_path: PathBuf,
}

pub fn find_newest(local_app_data: &Path) -> io::Result<Option<WandRuntime>> {
    let wand_directory = local_app_data.join("Wand");
    let entries = match fs::read_dir(wand_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut newest: Option<WandRuntime> = None;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let Some(version) = parse_version(&name) else {
            continue;
        };
        let app_directory = entry.path();
        let archive_path = app_directory.join("resources").join("app.asar");
        let trainer_directory = app_directory
            .join("resources")
            .join("app.asar.unpacked")
            .join("static")
            .join("unpacked")
            .join("trainerlib");
        if !app_directory.join("Wand.exe").is_file()
            || !archive_path.is_file()
            || REQUIRED_TRAINER_FILES
                .iter()
                .any(|file| !trainer_directory.join(file).is_file())
        {
            continue;
        }
        let candidate = WandRuntime {
            version,
            app_directory,
            trainer_directory,
            archive_path,
        };
        if newest
            .as_ref()
            .is_none_or(|current| candidate.version > current.version)
        {
            newest = Some(candidate);
        }
    }
    Ok(newest)
}

fn parse_version(folder: &str) -> Option<Vec<u32>> {
    let rest = folder.strip_prefix("app-")?;
    let parts = rest
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (2..=4).contains(&parts.len()).then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_numeric_app_versions() {
        assert_eq!(parse_version("app-12.30.1"), Some(vec![12, 30, 1]));
        assert_eq!(parse_version("app-12.beta"), None);
        assert_eq!(parse_version("app-12"), None);
        assert_eq!(parse_version("app-12.30.1.2.3"), None);
    }

    #[test]
    fn chooses_complete_runtime() {
        let root = std::env::temp_dir().join(format!("wand-rust-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (version, complete) in [("12.30.1", true), ("12.31.0", false)] {
            let app = root.join("Wand").join(format!("app-{version}"));
            let trainer = app.join("resources/app.asar.unpacked/static/unpacked/trainerlib");
            fs::create_dir_all(&trainer).unwrap();
            fs::write(app.join("Wand.exe"), []).unwrap();
            fs::write(app.join("resources/app.asar"), []).unwrap();
            if complete {
                for file in REQUIRED_TRAINER_FILES {
                    fs::write(trainer.join(file), []).unwrap();
                }
            }
        }
        let selected = find_newest(&root).unwrap().unwrap();
        assert_eq!(selected.version, vec![12, 30, 1]);
        fs::remove_dir_all(root).unwrap();
    }
}
