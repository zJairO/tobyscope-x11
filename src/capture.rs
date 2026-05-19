use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use image::imageops::FilterType;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as CompositeConnectionExt, Redirect};
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, MapState, Pixmap};

use crate::cache::ThumbnailCache;
use crate::i3;
use crate::pixels;
use crate::render::Renderer;
use crate::windows::WindowInfo;
use crate::x11::X11Context;

const VIEWABLE_TIMEOUT: Duration = Duration::from_millis(1200);
const VIEWABLE_POLL: Duration = Duration::from_millis(35);
const WORKSPACE_SETTLE: Duration = Duration::from_millis(90);
const MAX_CACHE_EDGE: u32 = 960;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepStatus {
    Updated,
    Finished,
}

#[derive(Debug)]
struct CaptureTask {
    index: usize,
    workspace: String,
}

pub struct CaptureSweep {
    tasks: VecDeque<CaptureTask>,
    original_workspace: Option<String>,
    active_workspace: Option<String>,
    restored: bool,
    debug: bool,
}

impl CaptureSweep {
    pub fn new(windows: &[WindowInfo], cache: &ThumbnailCache, debug: bool) -> Self {
        let original_workspace = match i3::current_workspace() {
            Ok(workspace) => workspace,
            Err(error) => {
                if debug {
                    eprintln!("capture: could not read focused i3 workspace: {error:#}");
                }
                None
            }
        };
        let tasks = windows
            .iter()
            .enumerate()
            .filter(|(_, window)| cache.needs_refresh(window))
            .map(|(index, window)| CaptureTask {
                index,
                workspace: window.workspace.clone(),
            })
            .collect::<VecDeque<_>>();
        if debug {
            eprintln!("capture: queued {} missing/stale thumbnails", tasks.len());
        }

        Self {
            tasks,
            original_workspace,
            active_workspace: None,
            restored: false,
            debug,
        }
    }

    pub fn step(
        &mut self,
        ctx: &X11Context,
        cache: &ThumbnailCache,
        windows: &[WindowInfo],
        renderer: &mut Renderer,
    ) -> Result<SweepStatus> {
        let Some(task) = self.tasks.pop_front() else {
            self.restore_original_workspace();
            return Ok(SweepStatus::Finished);
        };

        if self.original_workspace.is_some()
            && self.active_workspace.as_deref() != Some(task.workspace.as_str())
        {
            i3::switch_workspace(&task.workspace, self.debug)
                .with_context(|| format!("failed to switch to workspace {}", task.workspace))?;
            self.active_workspace = Some(task.workspace.clone());
            thread::sleep(WORKSPACE_SETTLE);
            renderer.raise(ctx)?;
        }

        let Some(window) = windows.get(task.index) else {
            return Ok(SweepStatus::Updated);
        };
        renderer.set_refreshing(task.index);
        match capture_window(ctx, window) {
            Ok(image) => {
                cache.store(window, &image)?;
                renderer.set_image(ctx, task.index, image);
            }
            Err(error) => {
                if self.debug {
                    eprintln!(
                        "capture: 0x{:08x} `{}` on workspace `{}` failed: {error:#}",
                        window.id, window.name, window.workspace
                    );
                }
                renderer.set_error(task.index, error.to_string());
            }
        }

        Ok(SweepStatus::Updated)
    }

    pub fn cancel(&mut self) {
        self.restore_original_workspace();
    }

    fn restore_original_workspace(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let Some(workspace) = self.original_workspace.as_deref() else {
            return;
        };
        if self.active_workspace.as_deref() == Some(workspace) {
            return;
        }
        if let Err(error) = i3::switch_workspace(workspace, self.debug) {
            if self.debug {
                eprintln!("capture: failed to restore workspace `{workspace}`: {error:#}");
            }
        }
    }
}

pub fn capture_window(ctx: &X11Context, window: &WindowInfo) -> Result<RgbaImage> {
    wait_for_viewable(ctx, window)?;
    let geometry = i3::refresh_geometry(ctx, window)?;
    if geometry.width == 0 || geometry.height == 0 {
        bail!("window has empty geometry");
    }

    let mut redirected = false;
    let mut pixmap: Option<Pixmap> = None;
    let result = (|| {
        ctx.conn
            .composite_redirect_window(window.id, Redirect::AUTOMATIC)
            .context("failed to request XComposite redirect")?
            .check()
            .context("XComposite rejected redirect for window")?;
        redirected = true;

        let pixmap_id = ctx
            .conn
            .generate_id()
            .context("failed to allocate capture pixmap id")?;
        pixmap = Some(pixmap_id);
        ctx.conn
            .composite_name_window_pixmap(window.id, pixmap_id)
            .context("failed to request named window pixmap")?
            .check()
            .context("XComposite could not name window pixmap")?;

        let image = ctx
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                pixmap_id,
                0,
                0,
                geometry.width,
                geometry.height,
                u32::MAX,
            )
            .context("failed to request XImage")?
            .reply()
            .context("failed to receive XImage")?;
        let visual = if image.visual == 0 {
            geometry.visual
        } else {
            image.visual
        };
        let rgba = pixels::ximage_to_rgba(
            ctx,
            visual,
            image.depth,
            geometry.width,
            geometry.height,
            &image.data,
        )?;
        Ok(scale_for_cache(&rgba))
    })();

    if let Some(pixmap_id) = pixmap {
        if let Ok(cookie) = ctx.conn.free_pixmap(pixmap_id) {
            cookie.ignore_error();
        }
    }
    if redirected {
        if let Ok(cookie) = ctx
            .conn
            .composite_unredirect_window(window.id, Redirect::AUTOMATIC)
        {
            cookie.ignore_error();
        }
    }

    result
}

fn wait_for_viewable(ctx: &X11Context, window: &WindowInfo) -> Result<()> {
    let started = Instant::now();
    loop {
        let attrs = ctx
            .conn
            .get_window_attributes(window.id)
            .context("failed to request window attributes")?
            .reply()
            .context("failed to read window attributes")?;
        if attrs.map_state == MapState::VIEWABLE {
            return Ok(());
        }
        if started.elapsed() >= VIEWABLE_TIMEOUT {
            bail!("window did not become viewable after workspace switch");
        }
        thread::sleep(VIEWABLE_POLL);
    }
}

fn scale_for_cache(image: &RgbaImage) -> RgbaImage {
    let max_edge = image.width().max(image.height());
    if max_edge <= MAX_CACHE_EDGE {
        return image.clone();
    }
    let scale = MAX_CACHE_EDGE as f32 / max_edge as f32;
    let width = ((image.width() as f32 * scale).round() as u32).max(1);
    let height = ((image.height() as f32 * scale).round() as u32).max(1);
    image::imageops::resize(image, width, height, FilterType::Triangle)
}
