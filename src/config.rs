use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ui: UiConfig,
    pub colors: ColorConfig,
    pub layout: LayoutConfig,
    pub thumbnails: ThumbnailConfig,
    pub daemon: DaemonConfig,
}

#[derive(Debug, Clone)]
pub struct UiConfig {
    pub font: String,
    pub show_overlay_background: bool,
    pub show_workspace_number: bool,
    pub show_program_name: bool,
    pub rounded_corners: bool,
    pub corner_radius: u16,
    pub shadows: bool,
    pub shadow_offset_x: i16,
    pub shadow_offset_y: i16,
    pub shadow_radius: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct ColorConfig {
    pub background: u32,
    pub cell: u32,
    pub cell_hover: u32,
    pub border: u32,
    pub selected: u32,
    pub text: u32,
    pub muted: u32,
    pub error: u32,
    pub empty: u32,
    pub shadow: u32,
    pub close_button: u32,
    pub close_button_border: u32,
    pub close_button_text: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    pub margin: u16,
    pub gap_small: u16,
    pub gap: u16,
    pub padding: u16,
    pub top_meta_height: u16,
    pub label_height: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct ThumbnailConfig {
    pub refresh_after_seconds: u64,
    pub max_cache_edge: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DaemonConfig {
    pub background_current_workspace_refresh: bool,
    pub idle_refresh_ms: u64,
    pub stale_show_ms: u64,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: AppConfig,
}

#[derive(Debug, Clone)]
enum ConfigSource {
    Explicit(PathBuf),
    Xdg(PathBuf),
    Defaults(Option<PathBuf>),
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    ui: Option<RawUiConfig>,
    colors: Option<RawColorConfig>,
    layout: Option<RawLayoutConfig>,
    thumbnails: Option<RawThumbnailConfig>,
    daemon: Option<RawDaemonConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawUiConfig {
    font: Option<String>,
    show_overlay_background: Option<bool>,
    show_workspace_number: Option<bool>,
    show_program_name: Option<bool>,
    rounded_corners: Option<bool>,
    corner_radius: Option<u16>,
    shadows: Option<bool>,
    shadow_offset_x: Option<i16>,
    shadow_offset_y: Option<i16>,
    shadow_radius: Option<u16>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawColorConfig {
    background: Option<String>,
    cell: Option<String>,
    cell_hover: Option<String>,
    border: Option<String>,
    selected: Option<String>,
    text: Option<String>,
    muted: Option<String>,
    error: Option<String>,
    empty: Option<String>,
    shadow: Option<String>,
    close_button: Option<String>,
    close_button_border: Option<String>,
    close_button_text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLayoutConfig {
    margin: Option<u16>,
    gap_small: Option<u16>,
    gap: Option<u16>,
    padding: Option<u16>,
    top_meta_height: Option<u16>,
    label_height: Option<u16>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawThumbnailConfig {
    refresh_after_seconds: Option<u64>,
    max_cache_edge: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDaemonConfig {
    background_current_workspace_refresh: Option<bool>,
    idle_refresh_ms: Option<u64>,
    stale_show_ms: Option<u64>,
}

impl AppConfig {
    fn merge(raw: RawConfig) -> Result<Self> {
        let mut config = Self::default();

        if let Some(ui) = raw.ui {
            if let Some(font) = ui.font {
                let font = font.trim();
                if font.is_empty() {
                    bail!("ui.font must not be empty");
                }
                config.ui.font = font.to_string();
            }
            if let Some(value) = ui.show_overlay_background {
                config.ui.show_overlay_background = value;
            }
            if let Some(value) = ui.show_workspace_number {
                config.ui.show_workspace_number = value;
            }
            if let Some(value) = ui.show_program_name {
                config.ui.show_program_name = value;
            }
            if let Some(value) = ui.rounded_corners {
                config.ui.rounded_corners = value;
            }
            if let Some(value) = ui.corner_radius {
                if value == 0 {
                    bail!("ui.corner_radius must be at least 1");
                }
                config.ui.corner_radius = value;
            }
            if let Some(value) = ui.shadows {
                config.ui.shadows = value;
            }
            if let Some(value) = ui.shadow_offset_x {
                config.ui.shadow_offset_x = value;
            }
            if let Some(value) = ui.shadow_offset_y {
                config.ui.shadow_offset_y = value;
            }
            if let Some(value) = ui.shadow_radius {
                if value == 0 {
                    bail!("ui.shadow_radius must be at least 1");
                }
                config.ui.shadow_radius = value;
            }
        }

        if let Some(colors) = raw.colors {
            if let Some(value) = colors.background {
                config.colors.background = parse_color("colors.background", &value)?;
            }
            if let Some(value) = colors.cell {
                config.colors.cell = parse_color("colors.cell", &value)?;
            }
            if let Some(value) = colors.cell_hover {
                config.colors.cell_hover = parse_color("colors.cell_hover", &value)?;
            }
            if let Some(value) = colors.border {
                config.colors.border = parse_color("colors.border", &value)?;
            }
            if let Some(value) = colors.selected {
                config.colors.selected = parse_color("colors.selected", &value)?;
            }
            if let Some(value) = colors.text {
                config.colors.text = parse_color("colors.text", &value)?;
            }
            if let Some(value) = colors.muted {
                config.colors.muted = parse_color("colors.muted", &value)?;
            }
            if let Some(value) = colors.error {
                config.colors.error = parse_color("colors.error", &value)?;
            }
            if let Some(value) = colors.empty {
                config.colors.empty = parse_color("colors.empty", &value)?;
            }
            if let Some(value) = colors.shadow {
                config.colors.shadow = parse_color("colors.shadow", &value)?;
            }
            if let Some(value) = colors.close_button {
                config.colors.close_button = parse_color("colors.close_button", &value)?;
            }
            if let Some(value) = colors.close_button_border {
                config.colors.close_button_border =
                    parse_color("colors.close_button_border", &value)?;
            }
            if let Some(value) = colors.close_button_text {
                config.colors.close_button_text = parse_color("colors.close_button_text", &value)?;
            }
        }

        if let Some(layout) = raw.layout {
            if let Some(value) = layout.margin {
                config.layout.margin = value;
            }
            if let Some(value) = layout.gap_small {
                config.layout.gap_small = value;
            }
            if let Some(value) = layout.gap {
                config.layout.gap = value;
            }
            if let Some(value) = layout.padding {
                config.layout.padding = value;
            }
            if let Some(value) = layout.top_meta_height {
                config.layout.top_meta_height = value;
            }
            if let Some(value) = layout.label_height {
                config.layout.label_height = value;
            }
        }

        if let Some(thumbnails) = raw.thumbnails {
            if let Some(value) = thumbnails.refresh_after_seconds {
                config.thumbnails.refresh_after_seconds = value;
            }
            if let Some(value) = thumbnails.max_cache_edge {
                if value == 0 {
                    bail!("thumbnails.max_cache_edge must be at least 1");
                }
                config.thumbnails.max_cache_edge = value;
            }
        }

        if let Some(daemon) = raw.daemon {
            if let Some(value) = daemon.background_current_workspace_refresh {
                config.daemon.background_current_workspace_refresh = value;
            }
            if let Some(value) = daemon.idle_refresh_ms {
                if value == 0 {
                    bail!("daemon.idle_refresh_ms must be at least 1");
                }
                config.daemon.idle_refresh_ms = value;
            }
            if let Some(value) = daemon.stale_show_ms {
                if value == 0 {
                    bail!("daemon.stale_show_ms must be at least 1");
                }
                config.daemon.stale_show_ms = value;
            }
        }

        Ok(config)
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            ui: UiConfig {
                font: "fixed".to_string(),
                show_overlay_background: true,
                show_workspace_number: true,
                show_program_name: true,
                rounded_corners: false,
                corner_radius: 14,
                shadows: false,
                shadow_offset_x: 0,
                shadow_offset_y: 8,
                shadow_radius: 18,
            },
            colors: ColorConfig {
                background: 0x101418,
                cell: 0x202832,
                cell_hover: 0x2b3642,
                border: 0x5f6f7f,
                selected: 0x4ea1ff,
                text: 0xe8edf2,
                muted: 0x95a3b2,
                error: 0x66303a,
                empty: 0x151b21,
                shadow: 0x05080c,
                close_button: 0xffffff,
                close_button_border: 0xffffff,
                close_button_text: 0x000000,
            },
            layout: LayoutConfig {
                margin: 24,
                gap_small: 12,
                gap: 18,
                padding: 12,
                top_meta_height: 40,
                label_height: 34,
            },
            thumbnails: ThumbnailConfig {
                refresh_after_seconds: 60,
                max_cache_edge: 960,
            },
            daemon: DaemonConfig {
                background_current_workspace_refresh: true,
                idle_refresh_ms: 750,
                stale_show_ms: 250,
            },
        }
    }
}

pub fn load(explicit_path: Option<&Path>, debug: bool) -> Result<LoadedConfig> {
    let (config, source) = if let Some(path) = explicit_path {
        (
            load_from_path(path)?,
            ConfigSource::Explicit(path.to_path_buf()),
        )
    } else if let Some(path) = default_config_path() {
        if path.exists() {
            (load_from_path(&path)?, ConfigSource::Xdg(path))
        } else {
            (AppConfig::default(), ConfigSource::Defaults(Some(path)))
        }
    } else {
        (AppConfig::default(), ConfigSource::Defaults(None))
    };

    if debug {
        match &source {
            ConfigSource::Explicit(path) => eprintln!("config: loaded {}", path.display()),
            ConfigSource::Xdg(path) => eprintln!("config: loaded {}", path.display()),
            ConfigSource::Defaults(Some(path)) => {
                eprintln!(
                    "config: using built-in defaults; no config at {}",
                    path.display()
                )
            }
            ConfigSource::Defaults(None) => {
                eprintln!("config: using built-in defaults; XDG config dir is unavailable")
            }
        }
    }

    Ok(LoadedConfig { config })
}

fn load_from_path(path: &Path) -> Result<AppConfig> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&source)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    AppConfig::merge(raw).with_context(|| format!("invalid config {}", path.display()))
}

fn default_config_path() -> Option<PathBuf> {
    dirs_next::config_dir().map(|dir| dir.join("tobyscope-x11").join("config.toml"))
}

fn parse_color(field: &str, value: &str) -> Result<u32> {
    let value = value.trim();
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        bail!("{field} must be a #RRGGBB hex color");
    }
    u32::from_str_radix(value, 16).with_context(|| format!("{field} must be a #RRGGBB hex color"))
}
