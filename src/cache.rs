use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use image::{ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::windows::WindowInfo;

const CACHE_FORMAT_VERSION: u32 = 2;

#[derive(Debug)]
pub struct ThumbnailCache {
    dir: PathBuf,
    valid_keys: HashSet<String>,
    compatible_keys: HashMap<WindowIdentity, Vec<String>>,
    refresh_after: Duration,
    debug: bool,
}

type WindowIdentity = (u32, Option<i64>);

#[derive(Debug, Serialize, Deserialize)]
struct ThumbnailMetadata {
    #[serde(default)]
    format_version: u32,
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
    pub fn new(windows: &[WindowInfo], refresh_after: Duration, debug: bool) -> Result<Self> {
        let display = display_key();
        let dir = dirs_next::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tobyscope-x11")
            .join(sanitize_path_component(&display));
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create thumbnail cache {}", dir.display()))?;

        let current_identities: HashSet<_> = windows.iter().map(window_identity).collect();
        let mut valid_keys: HashSet<_> = windows.iter().map(cache_key).collect();
        let mut compatible_keys: HashMap<WindowIdentity, Vec<String>> = HashMap::new();
        index_existing_metadata(
            &dir,
            &current_identities,
            &mut valid_keys,
            &mut compatible_keys,
        );
        if debug {
            eprintln!("cache: using {}", dir.display());
        }

        Ok(Self {
            dir,
            valid_keys,
            compatible_keys,
            refresh_after,
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
                self.load_compatible(window)
            }
        }
    }

    pub fn needs_refresh(&self, window: &WindowInfo) -> bool {
        self.cached_entry_needs_refresh(window, &cache_key(window))
            .or_else(|| self.compatible_entry_needs_refresh(window))
            .unwrap_or(true)
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
            format_version: CACHE_FORMAT_VERSION,
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

    fn png_path_for_key(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.png"))
    }

    fn metadata_path_for_key(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn load_compatible(&self, window: &WindowInfo) -> Option<RgbaImage> {
        let keys = self.compatible_keys.get(&window_identity(window))?;
        for key in keys {
            let path = self.png_path_for_key(key);
            match image::open(&path) {
                Ok(image) => {
                    if self.debug {
                        eprintln!(
                            "cache: compatible hit for 0x{:08x} `{}` at {}",
                            window.id,
                            window.name,
                            path.display()
                        );
                    }
                    return Some(image.to_rgba8());
                }
                Err(error) if self.debug && path.exists() => {
                    eprintln!(
                        "cache: failed to load compatible {} for 0x{:08x}: {error}",
                        path.display(),
                        window.id
                    );
                }
                Err(_) => {}
            }
        }
        None
    }

    fn compatible_entry_needs_refresh(&self, window: &WindowInfo) -> Option<bool> {
        let keys = self.compatible_keys.get(&window_identity(window))?;
        for key in keys {
            if let Some(needs_refresh) = self.cached_entry_needs_refresh(window, key) {
                return Some(needs_refresh);
            }
        }
        None
    }

    fn cached_entry_needs_refresh(&self, window: &WindowInfo, key: &str) -> Option<bool> {
        let png_metadata = fs::metadata(self.png_path_for_key(key)).ok()?;
        let modified = png_metadata.modified().ok()?;
        match modified.elapsed() {
            Ok(age) if age > self.refresh_after => return Some(true),
            Ok(_) | Err(_) => {}
        }

        let metadata = match self.read_metadata(key) {
            Ok(metadata) => metadata,
            Err(error) => {
                if self.debug {
                    eprintln!(
                        "cache: metadata for 0x{:08x} `{}` needs refresh: {error:#}",
                        window.id, window.name
                    );
                }
                return Some(true);
            }
        };

        if metadata.format_version != CACHE_FORMAT_VERSION {
            if self.debug {
                eprintln!(
                    "cache: format changed for 0x{:08x} `{}` cached={} current={}",
                    window.id, window.name, metadata.format_version, CACHE_FORMAT_VERSION
                );
            }
            return Some(true);
        }

        let same_geometry =
            metadata.width == window.geometry.width && metadata.height == window.geometry.height;
        if !same_geometry && self.debug {
            eprintln!(
                "cache: geometry changed for 0x{:08x} `{}` cached={}x{} current={}x{}",
                window.id,
                window.name,
                metadata.width,
                metadata.height,
                window.geometry.width,
                window.geometry.height
            );
        }
        Some(!same_geometry)
    }

    fn read_metadata(&self, key: &str) -> Result<ThumbnailMetadata> {
        let path = self.metadata_path_for_key(key);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read metadata {}", path.display()))?;
        serde_json::from_str(&source)
            .with_context(|| format!("failed to parse metadata {}", path.display()))
    }
}

fn cache_key(window: &WindowInfo) -> String {
    let mut hasher = DefaultHasher::new();
    display_key().hash(&mut hasher);
    CACHE_FORMAT_VERSION.hash(&mut hasher);
    window.id.hash(&mut hasher);
    window.i3_con_id.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn window_identity(window: &WindowInfo) -> WindowIdentity {
    (window.id, window.i3_con_id)
}

fn index_existing_metadata(
    dir: &Path,
    current_identities: &HashSet<WindowIdentity>,
    valid_keys: &mut HashSet<String>,
    compatible_keys: &mut HashMap<WindowIdentity, Vec<String>>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_str::<ThumbnailMetadata>(&source) else {
            continue;
        };
        let identity = (metadata.window_id, metadata.i3_con_id);
        if !current_identities.contains(&identity) {
            continue;
        }
        valid_keys.insert(metadata.key.clone());
        compatible_keys
            .entry(identity)
            .or_default()
            .push(metadata.key);
    }
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
