use image::ImageFormat;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_ART_BYTES: u64 = 1_000_000;

pub fn cached_path(directory: &Path, app_id: u64) -> PathBuf {
    directory.join(format!("{app_id}.jpg"))
}

pub fn fetch_to_cache(directory: &Path, app_id: u64, thumbnail: &str) -> Result<bool, String> {
    let destination = cached_path(directory, app_id);
    if destination.is_file() {
        return Ok(false);
    }
    let path = thumbnail.replace("{size}", "240");
    if !valid_thumbnail_path(&path) {
        return Err("Catalog thumbnail path is invalid".to_owned());
    }
    let url = format!("https://api-cdn.wemod.com{path}");
    let response = minreq::get(&url)
        .with_timeout(10)
        .send_lazy()
        .map_err(|error| format!("Artwork request failed: {error}"))?;
    if response.status_code != 200 {
        return Err(format!("Artwork returned HTTP {}", response.status_code));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_ART_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Artwork read failed: {error}"))?;
    if bytes.len() as u64 > MAX_ART_BYTES {
        return Err("Artwork exceeds 1 MB".to_owned());
    }
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::WebP)
        .map_err(|error| format!("Artwork decode failed: {error}"))?;
    if image.width() > 1024 || image.height() > 1024 {
        return Err("Artwork dimensions exceed 1024 pixels".to_owned());
    }
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("Could not create artwork cache: {error}"))?;
    let temporary = directory.join(format!("{app_id}.{}.tmp", std::process::id()));
    image
        .save_with_format(&temporary, ImageFormat::Jpeg)
        .map_err(|error| format!("Could not save artwork: {error}"))?;
    if destination.exists() {
        let _ = std::fs::remove_file(&temporary);
        return Ok(false);
    }
    std::fs::rename(&temporary, &destination)
        .map_err(|error| format!("Could not install artwork: {error}"))?;
    Ok(true)
}

fn valid_thumbnail_path(path: &str) -> bool {
    path.starts_with("/title_thumbnails/")
        && !path.contains("..")
        && !path.contains('%')
        && path
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._-".contains(character))
        && path.ends_with(".webp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_paths_cannot_change_host_or_escape_path() {
        assert!(valid_thumbnail_path(
            "/title_thumbnails/97315/1474383/240/1/thumbnail.webp"
        ));
        assert!(!valid_thumbnail_path("//evil.example/thumbnail.webp"));
        assert!(!valid_thumbnail_path("/title_thumbnails/../private.webp"));
        assert!(!valid_thumbnail_path(
            "/title_thumbnails/%2e%2e/private.webp"
        ));
    }
}
