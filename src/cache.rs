use std::collections::{HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use image::{ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::windows::WindowInfo;

const FRESH_THUMBNAIL_FOR: Duration = Duration::from_secs(10 * 60);

#[derive(Debug)]
pub struct ThumbnailCache {
    dir: PathBuf,
    valid_keys: HashSet<String>,
    debug: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ThumbnailMetadata {
    key: String,
    display: String,
    window_id: u32,
    i3_con_id: Option<i64>,
    workspace: String,
    title: String,
    class: Option<String>,
    width: u16,
    height: u16,
}

impl ThumbnailCache {
    pub fn new(windows: &[WindowInfo], debug: bool) -> Result<Self> {
        let display = display_key();
        let dir = dirs_next::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tobyscope-x11")
            .join(sanitize_path_component(&display));
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create thumbnail cache {}", dir.display()))?;

        let valid_keys = windows.iter().map(cache_key).collect();
        if debug {
            eprintln!("cache: using {}", dir.display());
        }

        Ok(Self {
            dir,
            valid_keys,
            debug,
        })
    }

    pub fn load(&self, window: &WindowInfo) -> Option<RgbaImage> {
        let path = self.png_path(window);
        match image::open(&path) {
            Ok(image) => Some(image.to_rgba8()),
            Err(error) => {
                if self.debug && path.exists() {
                    eprintln!(
                        "cache: failed to load {} for 0x{:08x}: {error}",
                        path.display(),
                        window.id
                    );
                }
                None
            }
        }
    }

    pub fn needs_refresh(&self, window: &WindowInfo) -> bool {
        let path = self.png_path(window);
        let Ok(metadata) = fs::metadata(&path) else {
            return true;
        };
        let Ok(modified) = metadata.modified() else {
            return true;
        };
        match modified.elapsed() {
            Ok(age) => age > FRESH_THUMBNAIL_FOR,
            Err(_) => false,
        }
    }

    pub fn store(&self, window: &WindowInfo, image: &RgbaImage) -> Result<()> {
        let key = cache_key(window);
        let path = self.dir.join(format!("{key}.png"));
        let tmp_path = self.dir.join(format!("{key}.{}.tmp", std::process::id()));
        image
            .save_with_format(&tmp_path, ImageFormat::Png)
            .with_context(|| format!("failed to encode thumbnail {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to publish thumbnail {}", path.display()))?;

        let metadata = ThumbnailMetadata {
            key: key.clone(),
            display: display_key(),
            window_id: window.id,
            i3_con_id: window.i3_con_id,
            workspace: window.workspace.clone(),
            title: window.name.clone(),
            class: window.class.clone(),
            width: window.geometry.width,
            height: window.geometry.height,
        };
        let json = serde_json::to_vec_pretty(&metadata).context("failed to encode metadata")?;
        let meta_path = self.dir.join(format!("{key}.json"));
        let meta_tmp_path = self
            .dir
            .join(format!("{key}.{}.json.tmp", std::process::id()));
        fs::write(&meta_tmp_path, json)
            .with_context(|| format!("failed to write metadata {}", meta_tmp_path.display()))?;
        fs::rename(&meta_tmp_path, &meta_path)
            .with_context(|| format!("failed to publish metadata {}", meta_path.display()))?;

        if self.debug {
            eprintln!(
                "cache: stored thumbnail for 0x{:08x} at {}",
                window.id,
                path.display()
            );
        }
        Ok(())
    }

    pub fn prune(&self) -> Result<usize> {
        let mut removed = 0usize;
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("failed to read cache dir {}", self.dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !is_cache_file(&path) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if !self.valid_keys.contains(stem) {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove stale cache {}", path.display()))?;
                removed += 1;
            }
        }
        if self.debug && removed > 0 {
            eprintln!("cache: pruned {removed} stale files");
        }
        Ok(removed)
    }

    fn png_path(&self, window: &WindowInfo) -> PathBuf {
        self.dir.join(format!("{}.png", cache_key(window)))
    }
}

fn cache_key(window: &WindowInfo) -> String {
    let mut hasher = DefaultHasher::new();
    display_key().hash(&mut hasher);
    window.id.hash(&mut hasher);
    window.i3_con_id.hash(&mut hasher);
    window.workspace.hash(&mut hasher);
    window.workspace_num.hash(&mut hasher);
    window.name.hash(&mut hasher);
    window.class.hash(&mut hasher);
    window.instance.hash(&mut hasher);
    window.geometry.width.hash(&mut hasher);
    window.geometry.height.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn display_key() -> String {
    std::env::var("DISPLAY").unwrap_or_else(|_| "unknown-display".to_string())
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn is_cache_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("png" | "json")
    )
}
