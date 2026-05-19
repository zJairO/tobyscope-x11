use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use cairo::{Context as CairoContext, Format, ImageSurface};
use image::imageops::FilterType;
use image::{Rgba, RgbaImage};
use pango::FontDescription;
use x11rb::connection::Connection;
use x11rb::protocol::render::{
    ConnectionExt as RenderConnectionExt, CreatePictureAux, Pictformat, Picture,
    QueryPictFormatsReply,
};
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as XprotoConnectionExt, CreateGCAux,
    CreateWindowAux, EventMask, Font, Gcontext, GrabMode, GrabStatus, ImageFormat, Pixmap,
    PropMode, Rectangle, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;
use x11rb::{CURRENT_TIME, NONE};

use crate::cache::ThumbnailCache;
use crate::config::{AppConfig, ColorConfig};
use crate::layout::{Layout, Rect};
use crate::pixels;
use crate::windows::WindowInfo;
use crate::x11::X11Context;

const APP_ID: &str = "tobyscope-x11";

pub struct Renderer {
    overlay: Overlay,
    thumbnails: Vec<ThumbnailSlot>,
    panel_cache: HashMap<PanelKey, Pixmap>,
    shadow_cache: HashMap<ShadowKey, Pixmap>,
    text: TextRenderer,
    config: AppConfig,
}

struct Overlay {
    window: Window,
    buffer: Pixmap,
    picture: Picture,
    gcs: Gcs,
    width: u16,
    height: u16,
    visible: bool,
}

struct Gcs {
    bg: Gcontext,
    cell: Gcontext,
    cell_hover: Gcontext,
    border: Gcontext,
    selected: Gcontext,
    text: Gcontext,
    muted: Gcontext,
    error: Gcontext,
    empty: Gcontext,
    shadow: Gcontext,
    image: Gcontext,
    font: Font,
}

struct ThumbnailSlot {
    state: ThumbnailState,
    prepared: Option<PreparedImage>,
}

struct PreparedImage {
    pixmap: Pixmap,
    width: u16,
    height: u16,
    corner_background: Option<u32>,
    corner_radius: u16,
}

struct TextRenderer {
    font: String,
    cache: RefCell<HashMap<TextKey, TextPixmap>>,
}

#[derive(Debug, Clone, Copy)]
struct TextPixmap {
    pixmap: Pixmap,
    width: u16,
    height: u16,
    baseline: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PanelKey {
    width: u16,
    height: u16,
    radius: u16,
    thickness: u16,
    background: u32,
    border: u32,
    fill: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ShadowKey {
    width: u16,
    height: u16,
    radius: u16,
    blur: u16,
    background: u32,
    shadow: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TextKey {
    text: String,
    foreground: u32,
    background: u32,
}

enum ThumbnailState {
    Image(RgbaImage),
    Refreshing(Option<RgbaImage>),
    Missing,
    Error {
        message: String,
        image: Option<RgbaImage>,
    },
}

impl Renderer {
    pub fn new(
        ctx: &X11Context,
        windows: &[WindowInfo],
        cache: &ThumbnailCache,
        config: &AppConfig,
        debug: bool,
    ) -> Result<Self> {
        let formats = ctx
            .conn
            .render_query_pict_formats()
            .context("failed to request XRender pict formats")?
            .reply()
            .context("failed to read XRender pict formats")?;
        let overlay = Overlay::create(ctx, &formats, config)?;
        let thumbnails = windows
            .iter()
            .map(|window| {
                let state = match cache.load(window) {
                    Some(image) => {
                        if debug {
                            eprintln!("cache: hit for 0x{:08x} `{}`", window.id, window.name);
                        }
                        ThumbnailState::Image(image)
                    }
                    None => ThumbnailState::Missing,
                };
                ThumbnailSlot {
                    state,
                    prepared: None,
                }
            })
            .collect();

        Ok(Self {
            overlay,
            thumbnails,
            panel_cache: HashMap::new(),
            shadow_cache: HashMap::new(),
            text: TextRenderer::new(&config.ui.font),
            config: config.clone(),
        })
    }

    pub fn overlay_window(&self) -> Window {
        self.overlay.window
    }

    pub fn size(&self) -> (u16, u16) {
        (self.overlay.width, self.overlay.height)
    }

    pub fn sync_root_size(&mut self, ctx: &X11Context) -> Result<bool> {
        let (width, height) = ctx.root_size()?;
        if self.overlay.width == width && self.overlay.height == height {
            return Ok(false);
        }
        self.update_size(ctx, width, height)?;
        Ok(true)
    }

    pub fn update_size(&mut self, ctx: &X11Context, width: u16, height: u16) -> Result<()> {
        if self.overlay.width == width && self.overlay.height == height {
            return Ok(());
        }
        self.overlay.resize(ctx, width, height)?;
        self.overlay.width = width;
        self.overlay.height = height;
        Ok(())
    }

    pub fn raise(&self, ctx: &X11Context) -> Result<()> {
        self.overlay.raise(ctx)
    }

    pub fn show(&mut self, ctx: &X11Context) -> Result<()> {
        self.overlay.show(ctx)
    }

    pub fn hide(&mut self, ctx: &X11Context) -> Result<()> {
        self.overlay.hide(ctx)
    }

    pub fn present(&self, ctx: &X11Context, layout: &Layout) -> Result<()> {
        if self.config.ui.show_overlay_background {
            ctx.conn
                .copy_area(
                    self.overlay.buffer,
                    self.overlay.window,
                    self.overlay.gcs.image,
                    0,
                    0,
                    0,
                    0,
                    self.overlay.width,
                    self.overlay.height,
                )
                .context("failed to copy prepared back buffer to overlay")?
                .check()
                .context("X11 rejected prepared overlay copy")?;
        } else {
            for item in &layout.items {
                self.copy_card_to_overlay(ctx, self.card_paint_bounds(item.cell))?;
            }
        }
        ctx.conn.flush().context("failed to flush prepared frame")?;
        Ok(())
    }

    pub fn set_refreshing(&mut self, index: usize) {
        if let Some(slot) = self.thumbnails.get_mut(index) {
            let image = slot.state.image().cloned();
            slot.state = ThumbnailState::Refreshing(image);
        }
    }

    pub fn set_image(&mut self, ctx: &X11Context, index: usize, image: RgbaImage) {
        if let Some(slot) = self.thumbnails.get_mut(index) {
            slot.release_prepared(ctx);
            slot.state = ThumbnailState::Image(image);
        }
    }

    pub fn set_error(&mut self, index: usize, message: String) {
        if let Some(slot) = self.thumbnails.get_mut(index) {
            let image = slot.state.image().cloned();
            slot.state = ThumbnailState::Error { message, image };
        }
    }

    pub fn redraw(
        &mut self,
        ctx: &X11Context,
        windows: &[WindowInfo],
        layout: &Layout,
        selected: usize,
    ) -> Result<()> {
        if self.config.ui.show_overlay_background {
            self.fill(ctx, self.overlay.rect(), self.overlay.gcs.bg)?;
        }

        for (index, (window, item)) in windows.iter().zip(&layout.items).enumerate() {
            let (cell_gc, cell_color) = if index == selected {
                (self.overlay.gcs.cell_hover, self.config.colors.cell_hover)
            } else {
                (self.overlay.gcs.cell, self.config.colors.cell)
            };
            self.draw_card_shadow(ctx, item.cell)?;
            self.draw_cell_background(ctx, item.cell, cell_gc, cell_color, index == selected)?;

            if self
                .thumbnails
                .get(index)
                .and_then(|slot| slot.state.image())
                .is_some()
            {
                self.draw_cached_image(ctx, index, item.preview, cell_color)?;
            }

            match self.thumbnails.get(index).map(|slot| &slot.state) {
                Some(ThumbnailState::Image(_)) => {}
                Some(ThumbnailState::Refreshing(Some(_))) => {
                    self.draw_status(ctx, item.preview, "refreshing", false)?;
                }
                Some(ThumbnailState::Refreshing(None)) => {
                    self.draw_status_box(ctx, item.preview, "capturing")?;
                }
                Some(ThumbnailState::Missing) => {
                    self.draw_status_box(ctx, item.preview, "waiting for thumbnail")?;
                }
                Some(ThumbnailState::Error {
                    message,
                    image: Some(_),
                }) => {
                    self.draw_status(ctx, item.preview, "refresh failed", true)?;
                    if item.preview.height > 70 {
                        self.draw_text(ctx, item.preview.x + 8, item.preview.y + 36, message, 72)?;
                    }
                }
                Some(ThumbnailState::Error {
                    message,
                    image: None,
                }) => {
                    self.draw_error_box(ctx, item.preview, message)?;
                }
                None => {
                    self.draw_error_box(ctx, item.preview, "preview missing")?;
                }
            }

            self.draw_workspace_badge(
                ctx,
                item.cell,
                &window.workspace,
                window.focused,
                window.urgent,
                cell_color,
            )?;
            let label = window.program_name();
            self.draw_label(ctx, item.cell, label.as_ref(), cell_color)?;
            if !self.config.ui.rounded_corners {
                self.draw_border(ctx, item.cell, index == selected)?;
            }
        }

        self.present(ctx, layout)
    }

    pub fn cleanup(&mut self, ctx: &X11Context) -> Result<()> {
        for slot in &mut self.thumbnails {
            slot.release_prepared(ctx);
        }
        for (_, pixmap) in self.panel_cache.drain() {
            if let Ok(cookie) = ctx.conn.free_pixmap(pixmap) {
                cookie.ignore_error();
            }
        }
        for (_, pixmap) in self.shadow_cache.drain() {
            if let Ok(cookie) = ctx.conn.free_pixmap(pixmap) {
                cookie.ignore_error();
            }
        }
        self.text.destroy(ctx);
        self.overlay.destroy(ctx)?;
        ctx.conn.flush().context("failed to flush cleanup")?;
        Ok(())
    }

    fn draw_cached_image(
        &mut self,
        ctx: &X11Context,
        index: usize,
        rect: Rect,
        corner_color: u32,
    ) -> Result<()> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        let Some((source_width, source_height)) = self
            .thumbnails
            .get(index)
            .and_then(|slot| slot.state.image())
            .map(|image| (image.width(), image.height()))
        else {
            return Ok(());
        };
        let target = fit_image(rect, source_width, source_height);
        let Some(slot) = self.thumbnails.get_mut(index) else {
            return Ok(());
        };
        let Some(pixmap) = slot.ensure_prepared(
            ctx,
            self.overlay.window,
            self.overlay.gcs.image,
            ctx.root_depth,
            target,
            self.config.ui.rounded_corners.then_some(corner_color),
            self.config.ui.corner_radius,
        )?
        else {
            return Ok(());
        };
        ctx.conn
            .copy_area(
                pixmap,
                self.overlay.buffer,
                self.overlay.gcs.image,
                0,
                0,
                target.x,
                target.y,
                target.width,
                target.height,
            )
            .context("failed to copy prepared thumbnail")?
            .check()
            .context("X11 rejected prepared thumbnail copy")?;
        Ok(())
    }

    fn copy_card_to_overlay(&self, ctx: &X11Context, rect: Rect) -> Result<()> {
        self.copy_buffer_region_to_overlay(ctx, rect)
    }

    fn copy_buffer_region_to_overlay(&self, ctx: &X11Context, rect: Rect) -> Result<()> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        ctx.conn
            .copy_area(
                self.overlay.buffer,
                self.overlay.window,
                self.overlay.gcs.image,
                rect.x,
                rect.y,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            )
            .context("failed to copy buffer region to overlay")?
            .check()
            .context("X11 rejected buffer region copy")?;
        Ok(())
    }

    fn card_paint_bounds(&self, rect: Rect) -> Rect {
        if !self.config.ui.shadows {
            return rect;
        }

        let blur = self.config.ui.shadow_radius;
        let shadow = Rect {
            x: rect
                .x
                .saturating_add(self.config.ui.shadow_offset_x)
                .saturating_sub(blur as i16),
            y: rect
                .y
                .saturating_add(self.config.ui.shadow_offset_y)
                .saturating_sub(blur as i16),
            width: rect.width.saturating_add(blur.saturating_mul(2)),
            height: rect.height.saturating_add(blur.saturating_mul(2)),
        };

        union_rect(rect, shadow, self.overlay.width, self.overlay.height)
    }

    fn draw_status_box(&self, ctx: &X11Context, rect: Rect, message: &str) -> Result<()> {
        self.fill_preview(ctx, rect, self.overlay.gcs.empty)?;
        self.draw_status(ctx, rect, message, false)
    }

    fn draw_error_box(&self, ctx: &X11Context, rect: Rect, message: &str) -> Result<()> {
        self.fill_preview(ctx, rect, self.overlay.gcs.error)?;
        self.draw_status(ctx, rect, "preview error", true)?;
        if rect.height > 48 {
            self.draw_text(ctx, rect.x + 8, rect.y + 38, message, 72)?;
        }
        Ok(())
    }

    fn draw_card_shadow(&mut self, ctx: &X11Context, rect: Rect) -> Result<()> {
        if !self.config.ui.shadows {
            return Ok(());
        }

        let blur = self.config.ui.shadow_radius.max(1);
        let radius = if self.config.ui.rounded_corners {
            rounded_radius(rect, self.config.ui.corner_radius)
        } else {
            0
        };
        let key = ShadowKey {
            width: rect.width,
            height: rect.height,
            radius,
            blur,
            background: self.config.colors.background,
            shadow: self.config.colors.shadow,
        };
        let pixmap = self.ensure_shadow_pixmap(ctx, key)?;
        let dst_x = rect
            .x
            .saturating_add(self.config.ui.shadow_offset_x)
            .saturating_sub(blur as i16);
        let dst_y = rect
            .y
            .saturating_add(self.config.ui.shadow_offset_y)
            .saturating_sub(blur as i16);
        self.copy_pixmap_clipped(
            ctx,
            pixmap,
            dst_x,
            dst_y,
            rect.width.saturating_add(blur.saturating_mul(2)),
            rect.height.saturating_add(blur.saturating_mul(2)),
        )
    }

    fn ensure_shadow_pixmap(&mut self, ctx: &X11Context, key: ShadowKey) -> Result<Pixmap> {
        if let Some(pixmap) = self.shadow_cache.get(&key) {
            return Ok(*pixmap);
        }

        let image = render_shadow_image(key);
        let data = pixels::rgba_to_zpixmap(ctx, &image)?;
        let pixmap = create_buffer(
            ctx,
            self.overlay.window,
            key.width.saturating_add(key.blur.saturating_mul(2)),
            key.height.saturating_add(key.blur.saturating_mul(2)),
        )?;
        ctx.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                self.overlay.gcs.image,
                key.width.saturating_add(key.blur.saturating_mul(2)),
                key.height.saturating_add(key.blur.saturating_mul(2)),
                0,
                0,
                0,
                ctx.root_depth,
                &data,
            )
            .context("failed to upload antialiased shadow")?
            .check()
            .context("X11 rejected antialiased shadow upload")?;
        self.shadow_cache.insert(key, pixmap);
        Ok(pixmap)
    }

    fn copy_pixmap_clipped(
        &self,
        ctx: &X11Context,
        pixmap: Pixmap,
        dst_x: i16,
        dst_y: i16,
        width: u16,
        height: u16,
    ) -> Result<()> {
        let src_left = 0i16.saturating_sub(dst_x).max(0);
        let src_top = 0i16.saturating_sub(dst_y).max(0);
        let clipped_dst_x = dst_x.max(0);
        let clipped_dst_y = dst_y.max(0);
        let right = (i32::from(dst_x) + i32::from(width)).min(i32::from(self.overlay.width));
        let bottom = (i32::from(dst_y) + i32::from(height)).min(i32::from(self.overlay.height));
        let clipped_width = (right - i32::from(clipped_dst_x)).max(0) as u16;
        let clipped_height = (bottom - i32::from(clipped_dst_y)).max(0) as u16;
        if clipped_width == 0 || clipped_height == 0 {
            return Ok(());
        }

        ctx.conn
            .copy_area(
                pixmap,
                self.overlay.buffer,
                self.overlay.gcs.image,
                src_left,
                src_top,
                clipped_dst_x,
                clipped_dst_y,
                clipped_width,
                clipped_height,
            )
            .context("failed to copy clipped pixmap")?
            .check()
            .context("X11 rejected clipped pixmap copy")?;
        Ok(())
    }

    fn draw_cell_background(
        &mut self,
        ctx: &X11Context,
        rect: Rect,
        cell_gc: Gcontext,
        cell_color: u32,
        selected: bool,
    ) -> Result<()> {
        if !self.config.ui.rounded_corners {
            return self.fill(ctx, rect, cell_gc);
        }

        let thickness = border_thickness(selected);
        let border_color = if selected {
            self.config.colors.selected
        } else {
            self.config.colors.border
        };
        let radius = rounded_radius(rect, self.config.ui.corner_radius);
        let key = PanelKey {
            width: rect.width,
            height: rect.height,
            radius,
            thickness,
            background: self.config.colors.background,
            border: border_color,
            fill: cell_color,
        };
        let pixmap = self.ensure_panel_pixmap(ctx, key)?;

        ctx.conn
            .copy_area(
                pixmap,
                self.overlay.buffer,
                self.overlay.gcs.image,
                0,
                0,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            )
            .context("failed to copy antialiased card")?
            .check()
            .context("X11 rejected antialiased card copy")?;
        Ok(())
    }

    fn ensure_panel_pixmap(&mut self, ctx: &X11Context, key: PanelKey) -> Result<Pixmap> {
        if let Some(pixmap) = self.panel_cache.get(&key) {
            return Ok(*pixmap);
        }

        let image = render_panel_image(key);
        let data = pixels::rgba_to_zpixmap(ctx, &image)?;
        let pixmap = create_buffer(ctx, self.overlay.window, key.width, key.height)?;
        ctx.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                self.overlay.gcs.image,
                key.width,
                key.height,
                0,
                0,
                0,
                ctx.root_depth,
                &data,
            )
            .context("failed to upload antialiased card")?
            .check()
            .context("X11 rejected antialiased card upload")?;
        self.panel_cache.insert(key, pixmap);
        Ok(pixmap)
    }

    fn fill_preview(&self, ctx: &X11Context, rect: Rect, gc: Gcontext) -> Result<()> {
        if self.config.ui.rounded_corners {
            self.fill_rounded(ctx, rect, self.config.ui.corner_radius, gc)
        } else {
            self.fill(ctx, rect, gc)
        }
    }

    fn draw_status(&self, ctx: &X11Context, rect: Rect, message: &str, error: bool) -> Result<()> {
        let (foreground, background) = if error {
            (self.config.colors.text, self.config.colors.error)
        } else {
            (self.config.colors.muted, self.config.colors.empty)
        };
        self.draw_text_with_colors(
            ctx,
            rect.x + 8,
            rect.y + 19,
            message,
            48,
            foreground,
            background,
        )
    }

    fn draw_workspace_badge(
        &self,
        ctx: &X11Context,
        cell: Rect,
        workspace: &str,
        focused: bool,
        urgent: bool,
        background: u32,
    ) -> Result<()> {
        if !self.config.ui.show_workspace_number {
            return Ok(());
        }
        let width = cell.width.saturating_sub(18).min(136);
        let rect = Rect {
            x: cell.x + 9,
            y: cell.y + 8,
            width,
            height: 22,
        };
        let marker = if urgent {
            "! "
        } else if focused {
            "* "
        } else {
            ""
        };
        let label = format!("{marker}{workspace}");
        self.draw_text_with_colors(
            ctx,
            rect.x + 6,
            rect.y + 15,
            &label,
            20,
            self.config.colors.text,
            background,
        )
    }

    fn draw_label(&self, ctx: &X11Context, cell: Rect, label: &str, background: u32) -> Result<()> {
        if !self.config.ui.show_program_name {
            return Ok(());
        }
        let y = cell.y.saturating_add(cell.height as i16).saturating_sub(12);
        let max_chars = (usize::from(cell.width) / 7)
            .saturating_sub(2)
            .clamp(12, 120);
        self.draw_text_with_colors(
            ctx,
            cell.x + 10,
            y,
            label,
            max_chars,
            self.config.colors.text,
            background,
        )
    }

    fn draw_border(&self, ctx: &X11Context, rect: Rect, selected: bool) -> Result<()> {
        let thickness = border_thickness(selected);
        let gc = if selected {
            self.overlay.gcs.selected
        } else {
            self.overlay.gcs.border
        };
        let x2 = rect.x + rect.width as i16 - thickness as i16;
        let y2 = rect.y + rect.height as i16 - thickness as i16;
        let pieces = [
            Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: thickness,
            },
            Rect {
                x: rect.x,
                y: y2,
                width: rect.width,
                height: thickness,
            },
            Rect {
                x: rect.x,
                y: rect.y,
                width: thickness,
                height: rect.height,
            },
            Rect {
                x: x2,
                y: rect.y,
                width: thickness,
                height: rect.height,
            },
        ];
        for piece in pieces {
            self.fill(ctx, piece, gc)?;
        }
        Ok(())
    }

    fn fill(&self, ctx: &X11Context, rect: Rect, gc: Gcontext) -> Result<()> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        ctx.conn
            .poly_fill_rectangle(
                self.overlay.buffer,
                gc,
                &[Rectangle {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                }],
            )
            .context("failed to fill rectangle")?;
        Ok(())
    }

    fn fill_rounded(&self, ctx: &X11Context, rect: Rect, radius: u16, gc: Gcontext) -> Result<()> {
        let radius = rounded_radius(rect, radius);
        if radius == 0 {
            return self.fill(ctx, rect, gc);
        }

        let rectangles = rounded_rect_spans(rect, radius);
        ctx.conn
            .poly_fill_rectangle(self.overlay.buffer, gc, &rectangles)
            .context("failed to fill rounded rectangle")?;
        Ok(())
    }

    fn draw_text(&self, ctx: &X11Context, x: i16, y: i16, text: &str, max: usize) -> Result<()> {
        self.draw_text_with_colors(
            ctx,
            x,
            y,
            text,
            max,
            self.config.colors.text,
            self.config.colors.error,
        )
    }

    fn draw_text_with_colors(
        &self,
        ctx: &X11Context,
        x: i16,
        y: i16,
        text: &str,
        max: usize,
        foreground: u32,
        background: u32,
    ) -> Result<()> {
        self.text.draw(
            ctx,
            self.overlay.window,
            self.overlay.buffer,
            self.overlay.gcs.image,
            ctx.root_depth,
            x,
            y,
            text,
            max,
            foreground,
            background,
        )
    }
}

impl ThumbnailState {
    fn image(&self) -> Option<&RgbaImage> {
        match self {
            Self::Image(image) | Self::Refreshing(Some(image)) => Some(image),
            Self::Error {
                image: Some(image), ..
            } => Some(image),
            Self::Refreshing(None) | Self::Missing | Self::Error { image: None, .. } => None,
        }
    }
}

impl ThumbnailSlot {
    fn ensure_prepared(
        &mut self,
        ctx: &X11Context,
        drawable: Window,
        gc: Gcontext,
        depth: u8,
        rect: Rect,
        corner_background: Option<u32>,
        corner_radius: u16,
    ) -> Result<Option<Pixmap>> {
        if self.prepared.as_ref().is_some_and(|prepared| {
            prepared.width == rect.width
                && prepared.height == rect.height
                && prepared.corner_background == corner_background
                && prepared.corner_radius == corner_radius
        }) {
            return Ok(self.prepared.as_ref().map(|prepared| prepared.pixmap));
        }

        let Some((pixmap, width, height)) = (|| -> Result<Option<(Pixmap, u16, u16)>> {
            let Some(image) = self.state.image() else {
                return Ok(None);
            };
            let mut scaled = image::imageops::resize(
                image,
                u32::from(rect.width),
                u32::from(rect.height),
                FilterType::Triangle,
            );
            if let Some(background) = corner_background {
                apply_rounded_image_mask(&mut scaled, corner_radius, background);
            }
            let data = pixels::rgba_to_zpixmap(ctx, &scaled)?;
            let pixmap = create_buffer(ctx, drawable, rect.width, rect.height)?;
            ctx.conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    pixmap,
                    gc,
                    rect.width,
                    rect.height,
                    0,
                    0,
                    0,
                    depth,
                    &data,
                )
                .context("failed to upload prepared thumbnail")?
                .check()
                .context("X11 rejected prepared thumbnail upload")?;
            Ok(Some((pixmap, rect.width, rect.height)))
        })()?
        else {
            self.release_prepared(ctx);
            return Ok(None);
        };

        self.release_prepared(ctx);
        self.prepared = Some(PreparedImage {
            pixmap,
            width,
            height,
            corner_background,
            corner_radius,
        });
        Ok(Some(pixmap))
    }

    fn release_prepared(&mut self, ctx: &X11Context) {
        if let Some(prepared) = self.prepared.take() {
            if let Ok(cookie) = ctx.conn.free_pixmap(prepared.pixmap) {
                cookie.ignore_error();
            }
        }
    }
}

impl TextRenderer {
    fn new(font: &str) -> Self {
        Self {
            font: normalize_pango_font(font),
            cache: RefCell::new(HashMap::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        ctx: &X11Context,
        drawable: Window,
        target: Pixmap,
        gc: Gcontext,
        depth: u8,
        x: i16,
        baseline_y: i16,
        text: &str,
        max: usize,
        foreground: u32,
        background: u32,
    ) -> Result<()> {
        let text = clipped_label(text, max);
        if text.is_empty() {
            return Ok(());
        }

        let key = TextKey {
            text,
            foreground,
            background,
        };
        if let Some(cached) = self.cache.borrow().get(&key) {
            return copy_text_pixmap(ctx, *cached, target, gc, x, baseline_y);
        }

        let rendered = render_text_image(&self.font, &key.text, key.foreground, key.background)?;
        let data = pixels::rgba_to_zpixmap(ctx, &rendered.image)?;
        let pixmap = create_buffer(ctx, drawable, rendered.width, rendered.height)?;
        ctx.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                gc,
                rendered.width,
                rendered.height,
                0,
                0,
                0,
                depth,
                &data,
            )
            .context("failed to upload text")?
            .check()
            .context("X11 rejected text upload")?;

        let cached = TextPixmap {
            pixmap,
            width: rendered.width,
            height: rendered.height,
            baseline: rendered.baseline,
        };
        self.cache.borrow_mut().insert(key, cached);
        copy_text_pixmap(ctx, cached, target, gc, x, baseline_y)
    }

    fn destroy(&self, ctx: &X11Context) {
        for (_, cached) in self.cache.borrow_mut().drain() {
            if let Ok(cookie) = ctx.conn.free_pixmap(cached.pixmap) {
                cookie.ignore_error();
            }
        }
    }
}

#[derive(Debug)]
struct RenderedText {
    image: RgbaImage,
    width: u16,
    height: u16,
    baseline: i16,
}

fn copy_text_pixmap(
    ctx: &X11Context,
    cached: TextPixmap,
    target: Pixmap,
    gc: Gcontext,
    x: i16,
    baseline_y: i16,
) -> Result<()> {
    let y = baseline_y.saturating_sub(cached.baseline);
    ctx.conn
        .copy_area(
            cached.pixmap,
            target,
            gc,
            0,
            0,
            x,
            y,
            cached.width,
            cached.height,
        )
        .context("failed to copy rendered text")?
        .check()
        .context("X11 rejected rendered text copy")?;
    Ok(())
}

impl Overlay {
    fn create(
        ctx: &X11Context,
        formats: &QueryPictFormatsReply,
        config: &AppConfig,
    ) -> Result<Self> {
        let pict_format = pict_format_for_visual(formats, ctx.root_visual).with_context(|| {
            format!(
                "no XRender pict format for root visual 0x{:08x}",
                ctx.root_visual
            )
        })?;
        let window = ctx
            .conn
            .generate_id()
            .context("failed to allocate overlay window id")?;
        let mut values = CreateWindowAux::new()
            .border_pixel(config.colors.background)
            .override_redirect(1u32)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::POINTER_MOTION
                    | EventMask::STRUCTURE_NOTIFY,
            );
        values = if config.ui.show_overlay_background {
            values.background_pixel(config.colors.background)
        } else {
            values.background_pixmap(NONE)
        };

        ctx.conn
            .create_window(
                ctx.root_depth,
                window,
                ctx.root,
                0,
                0,
                ctx.width,
                ctx.height,
                0,
                WindowClass::INPUT_OUTPUT,
                ctx.root_visual,
                &values,
            )
            .context("failed to create overlay window")?
            .check()
            .context("X11 rejected overlay window creation")?;
        set_overlay_identity(ctx, window)?;

        let picture = ctx
            .conn
            .generate_id()
            .context("failed to allocate overlay picture")?;
        ctx.conn
            .render_create_picture(picture, window, pict_format, &CreatePictureAux::new())
            .context("failed to create overlay render picture")?
            .check()
            .context("XRender rejected overlay picture")?;

        let buffer = create_buffer(ctx, window, ctx.width, ctx.height)?;
        let gcs = Gcs::create(ctx, buffer, &config.colors, &config.ui.font)?;

        ctx.conn.flush().context("failed to flush overlay setup")?;

        Ok(Self {
            window,
            buffer,
            picture,
            gcs,
            width: ctx.width,
            height: ctx.height,
            visible: false,
        })
    }

    fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    fn resize(&mut self, ctx: &X11Context, width: u16, height: u16) -> Result<()> {
        let next = create_buffer(ctx, self.window, width, height)?;
        ctx.conn
            .configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .x(0)
                    .y(0)
                    .width(u32::from(width.max(1)))
                    .height(u32::from(height.max(1))),
            )
            .context("failed to resize overlay window")?
            .check()
            .context("X11 rejected overlay window resize")?;
        if let Ok(cookie) = ctx.conn.free_pixmap(self.buffer) {
            cookie.ignore_error();
        }
        self.buffer = next;
        Ok(())
    }

    fn raise(&self, ctx: &X11Context) -> Result<()> {
        raise_window(ctx, self.window)
    }

    fn show(&mut self, ctx: &X11Context) -> Result<()> {
        if !self.visible {
            ctx.conn
                .map_window(self.window)
                .context("failed to map overlay")?
                .check()
                .context("X11 rejected overlay map")?;
            self.visible = true;
        }
        raise_window(ctx, self.window)?;

        let grab = ctx
            .conn
            .grab_keyboard(
                false,
                self.window,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .context("failed to request keyboard grab")?
            .reply()
            .context("failed to receive keyboard grab reply")?;
        if grab.status != GrabStatus::SUCCESS {
            bail!(
                "could not grab keyboard for overview overlay: {:?}",
                grab.status
            );
        }

        let pointer_grab = ctx
            .conn
            .grab_pointer(
                false,
                self.window,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                NONE,
                NONE,
                CURRENT_TIME,
            )
            .context("failed to request pointer grab")?
            .reply()
            .context("failed to receive pointer grab reply")?;
        if pointer_grab.status != GrabStatus::SUCCESS {
            if let Ok(cookie) = ctx.conn.ungrab_keyboard(CURRENT_TIME) {
                cookie.ignore_error();
            }
            bail!(
                "could not grab pointer for overview overlay: {:?}",
                pointer_grab.status
            );
        }

        ctx.conn.flush().context("failed to flush overlay show")?;
        Ok(())
    }

    fn hide(&mut self, ctx: &X11Context) -> Result<()> {
        if let Ok(cookie) = ctx.conn.ungrab_keyboard(CURRENT_TIME) {
            cookie.ignore_error();
        }
        if let Ok(cookie) = ctx.conn.ungrab_pointer(CURRENT_TIME) {
            cookie.ignore_error();
        }
        if self.visible {
            if let Ok(cookie) = ctx.conn.unmap_window(self.window) {
                cookie.ignore_error();
            }
            self.visible = false;
        }
        ctx.conn.flush().context("failed to flush overlay hide")?;
        Ok(())
    }

    fn destroy(&mut self, ctx: &X11Context) -> Result<()> {
        self.hide(ctx)?;
        if let Ok(cookie) = ctx.conn.render_free_picture(self.picture) {
            cookie.ignore_error();
        }
        self.gcs.destroy(ctx);
        if let Ok(cookie) = ctx.conn.free_pixmap(self.buffer) {
            cookie.ignore_error();
        }
        if let Ok(cookie) = ctx.conn.destroy_window(self.window) {
            cookie.ignore_error();
        }
        Ok(())
    }
}

impl Gcs {
    fn create(
        ctx: &X11Context,
        drawable: Pixmap,
        colors: &ColorConfig,
        _font_name: &str,
    ) -> Result<Self> {
        let font = ctx
            .conn
            .generate_id()
            .context("failed to allocate font id")?;
        ctx.conn
            .open_font(font, b"fixed")
            .context("failed to open fallback X11 font `fixed`")?
            .check()
            .context("X11 rejected fallback font `fixed`")?;

        Ok(Self {
            bg: create_gc(ctx, drawable, colors.background, colors.background, font)?,
            cell: create_gc(ctx, drawable, colors.cell, colors.cell, font)?,
            cell_hover: create_gc(ctx, drawable, colors.cell_hover, colors.cell_hover, font)?,
            border: create_gc(ctx, drawable, colors.border, colors.border, font)?,
            selected: create_gc(ctx, drawable, colors.selected, colors.selected, font)?,
            text: create_gc(ctx, drawable, colors.text, colors.background, font)?,
            muted: create_gc(ctx, drawable, colors.muted, colors.background, font)?,
            error: create_gc(ctx, drawable, colors.error, colors.error, font)?,
            empty: create_gc(ctx, drawable, colors.empty, colors.empty, font)?,
            shadow: create_gc(ctx, drawable, colors.shadow, colors.background, font)?,
            image: create_gc(ctx, drawable, colors.text, colors.background, font)?,
            font,
        })
    }

    fn destroy(&self, ctx: &X11Context) {
        for gc in [
            self.bg,
            self.cell,
            self.cell_hover,
            self.border,
            self.selected,
            self.text,
            self.muted,
            self.error,
            self.empty,
            self.shadow,
            self.image,
        ] {
            if let Ok(cookie) = ctx.conn.free_gc(gc) {
                cookie.ignore_error();
            }
        }
        if let Ok(cookie) = ctx.conn.close_font(self.font) {
            cookie.ignore_error();
        }
    }
}

fn set_overlay_identity(ctx: &X11Context, window: Window) -> Result<()> {
    let wm_class = b"tobyscope-x11\0tobyscope-x11\0";
    ctx.conn
        .change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            wm_class,
        )
        .context("failed to set overlay WM_CLASS")?
        .check()
        .context("X11 rejected overlay WM_CLASS")?;

    ctx.conn
        .change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            APP_ID.as_bytes(),
        )
        .context("failed to set overlay WM_NAME")?
        .check()
        .context("X11 rejected overlay WM_NAME")?;

    Ok(())
}

fn create_gc(
    ctx: &X11Context,
    drawable: Pixmap,
    foreground: u32,
    background: u32,
    font: Font,
) -> Result<Gcontext> {
    let gc = ctx.conn.generate_id().context("failed to allocate GC id")?;
    let values = CreateGCAux::new()
        .foreground(foreground)
        .background(background)
        .font(font)
        .graphics_exposures(0u32);
    ctx.conn
        .create_gc(gc, drawable, &values)
        .context("failed to create graphics context")?
        .check()
        .context("X11 rejected graphics context")?;
    Ok(gc)
}

fn create_buffer(ctx: &X11Context, drawable: Window, width: u16, height: u16) -> Result<Pixmap> {
    let buffer = ctx
        .conn
        .generate_id()
        .context("failed to allocate overlay back-buffer id")?;
    ctx.conn
        .create_pixmap(
            ctx.root_depth,
            buffer,
            drawable,
            width.max(1),
            height.max(1),
        )
        .context("failed to create overlay back buffer")?
        .check()
        .context("X11 rejected overlay back-buffer creation")?;
    Ok(buffer)
}

fn raise_window(ctx: &X11Context, window: Window) -> Result<()> {
    ctx.conn
        .configure_window(
            window,
            &x11rb::protocol::xproto::ConfigureWindowAux::new()
                .stack_mode(x11rb::protocol::xproto::StackMode::ABOVE),
        )
        .context("failed to raise overlay")?
        .check()
        .context("X11 rejected overlay raise")?;
    Ok(())
}

fn pict_format_for_visual(
    formats: &QueryPictFormatsReply,
    visual: x11rb::protocol::xproto::Visualid,
) -> Option<Pictformat> {
    formats
        .screens
        .iter()
        .flat_map(|screen| &screen.depths)
        .flat_map(|depth| &depth.visuals)
        .find(|candidate| candidate.visual == visual)
        .map(|candidate| candidate.format)
}

fn clipped_label(text: &str, max: usize) -> String {
    let clean: Vec<char> = text.chars().filter(|ch| !ch.is_control()).collect();
    if clean.len() <= max {
        return clean.into_iter().collect();
    }

    if max <= 3 {
        clean.into_iter().take(max).collect()
    } else {
        let mut out: String = clean.into_iter().take(max - 3).collect();
        out.push_str("...");
        out
    }
}

fn normalize_pango_font(font: &str) -> String {
    let font = font.trim();
    if font.contains(":size=") {
        let mut parts = font.split(';');
        if let Some(base) = parts.next()
            && let Some((family, size)) = base.split_once(":size=")
        {
            return format!("{} {}", family.trim(), size.trim());
        }
    }
    font.to_string()
}

fn render_text_image(
    font: &str,
    text: &str,
    foreground: u32,
    background: u32,
) -> Result<RenderedText> {
    let measure_surface = ImageSurface::create(Format::ARgb32, 1, 1)
        .context("failed to create text measure surface")?;
    let measure_context =
        CairoContext::new(&measure_surface).context("failed to create text measure context")?;
    let layout = pangocairo::functions::create_layout(&measure_context);
    layout.set_text(text);
    layout.set_font_description(Some(&FontDescription::from_string(font)));
    let (_, logical) = layout.pixel_extents();
    let width = logical.width().max(1).min(i32::from(u16::MAX)) as u16;
    let height = logical.height().max(1).min(i32::from(u16::MAX)) as u16;
    let baseline = ((layout.baseline() / pango::SCALE) - logical.y())
        .max(1)
        .min(i32::from(i16::MAX)) as i16;

    let mut surface = ImageSurface::create(Format::ARgb32, i32::from(width), i32::from(height))
        .context("failed to create text surface")?;
    let context = CairoContext::new(&surface).context("failed to create text cairo context")?;
    let [br, bg, bb] = unpack_rgb(background);
    context.set_source_rgb(
        f64::from(br) / 255.0,
        f64::from(bg) / 255.0,
        f64::from(bb) / 255.0,
    );
    context.paint().context("failed to paint text background")?;

    let [fr, fg, fb] = unpack_rgb(foreground);
    context.set_source_rgb(
        f64::from(fr) / 255.0,
        f64::from(fg) / 255.0,
        f64::from(fb) / 255.0,
    );
    context.move_to(f64::from(-logical.x()), f64::from(-logical.y()));
    let layout = pangocairo::functions::create_layout(&context);
    layout.set_text(text);
    layout.set_font_description(Some(&FontDescription::from_string(font)));
    pangocairo::functions::show_layout(&context, &layout);
    drop(layout);
    drop(context);
    surface.flush();

    let stride = surface.stride() as usize;
    let data = surface
        .data()
        .map_err(|err| anyhow!("failed to read text surface data: {err}"))?;
    let mut image = RgbaImage::new(u32::from(width), u32::from(height));
    for y in 0..height as usize {
        for x in 0..width as usize {
            let offset = y * stride + x * 4;
            let blue = data[offset];
            let green = data[offset + 1];
            let red = data[offset + 2];
            image.put_pixel(x as u32, y as u32, Rgba([red, green, blue, 255]));
        }
    }

    Ok(RenderedText {
        image,
        width,
        height,
        baseline,
    })
}

fn render_shadow_image(key: ShadowKey) -> RgbaImage {
    let width = key.width.saturating_add(key.blur.saturating_mul(2));
    let height = key.height.saturating_add(key.blur.saturating_mul(2));
    let mut image = RgbaImage::new(u32::from(width), u32::from(height));
    let background = unpack_rgb(key.background);
    let shadow = unpack_rgb(key.shadow);
    let blur = f32::from(key.blur.max(1));
    let max_alpha = 0.58;

    for y in 0..height {
        for x in 0..width {
            let distance = rounded_rect_signed_distance(
                f32::from(x) + 0.5,
                f32::from(y) + 0.5,
                blur,
                blur,
                blur + f32::from(key.width),
                blur + f32::from(key.height),
                f32::from(key.radius),
            );
            let alpha = if distance <= 0.0 {
                max_alpha
            } else if distance < blur {
                let t = 1.0 - distance / blur;
                max_alpha * t * t
            } else {
                0.0
            };
            let rgb = blend_rgb(background, shadow, alpha);
            image.put_pixel(x.into(), y.into(), Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }

    image
}

fn render_panel_image(key: PanelKey) -> RgbaImage {
    let mut image = RgbaImage::new(u32::from(key.width), u32::from(key.height));
    let background = unpack_rgb(key.background);
    let border = unpack_rgb(key.border);
    let fill = unpack_rgb(key.fill);
    let inner_left = f32::from(key.thickness);
    let inner_top = f32::from(key.thickness);
    let inner_right = f32::from(key.width.saturating_sub(key.thickness));
    let inner_bottom = f32::from(key.height.saturating_sub(key.thickness));
    let inner_radius = key.radius.saturating_sub(key.thickness);

    for y in 0..key.height {
        for x in 0..key.width {
            let outer_coverage = rounded_rect_coverage(
                f32::from(x),
                f32::from(y),
                0.0,
                0.0,
                f32::from(key.width),
                f32::from(key.height),
                f32::from(key.radius),
            );
            let inner_coverage = if inner_right > inner_left && inner_bottom > inner_top {
                rounded_rect_coverage(
                    f32::from(x),
                    f32::from(y),
                    inner_left,
                    inner_top,
                    inner_right,
                    inner_bottom,
                    f32::from(inner_radius),
                )
            } else {
                0.0
            };
            let with_border = blend_rgb(background, border, outer_coverage);
            let rgb = blend_rgb(with_border, fill, inner_coverage);
            image.put_pixel(x.into(), y.into(), Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }

    image
}

fn apply_rounded_image_mask(image: &mut RgbaImage, radius: u16, background: u32) {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return;
    }

    let radius = f32::from(radius)
        .min(width as f32 / 2.0)
        .min(height as f32 / 2.0);
    if radius <= 0.0 {
        return;
    }

    let background = unpack_rgb(background);
    for y in 0..height {
        for x in 0..width {
            let coverage = rounded_rect_coverage(
                x as f32,
                y as f32,
                0.0,
                0.0,
                width as f32,
                height as f32,
                radius,
            );
            if coverage >= 1.0 {
                continue;
            }
            let source = image.get_pixel(x, y).0;
            let rgb = blend_rgb(background, [source[0], source[1], source[2]], coverage);
            image.put_pixel(x, y, Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }
}

fn rounded_rect_coverage(
    pixel_x: f32,
    pixel_y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> f32 {
    const SAMPLES: u16 = 4;
    let mut covered = 0u16;
    for sample_y in 0..SAMPLES {
        for sample_x in 0..SAMPLES {
            let x = pixel_x + (f32::from(sample_x) + 0.5) / f32::from(SAMPLES);
            let y = pixel_y + (f32::from(sample_y) + 0.5) / f32::from(SAMPLES);
            if inside_rounded_rect(x, y, left, top, right, bottom, radius) {
                covered += 1;
            }
        }
    }
    f32::from(covered) / f32::from(SAMPLES * SAMPLES)
}

fn inside_rounded_rect(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> bool {
    if x < left || x >= right || y < top || y >= bottom {
        return false;
    }
    if radius <= 0.0 {
        return true;
    }

    let radius = radius.min((right - left) / 2.0).min((bottom - top) / 2.0);
    let nearest_x = x.clamp(left + radius, right - radius);
    let nearest_y = y.clamp(top + radius, bottom - radius);
    let dx = x - nearest_x;
    let dy = y - nearest_y;
    dx * dx + dy * dy <= radius * radius
}

fn rounded_rect_signed_distance(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> f32 {
    let radius = radius
        .max(0.0)
        .min((right - left) / 2.0)
        .min((bottom - top) / 2.0);
    let center_x = (left + right) / 2.0;
    let center_y = (top + bottom) / 2.0;
    let half_x = ((right - left) / 2.0 - radius).max(0.0);
    let half_y = ((bottom - top) / 2.0 - radius).max(0.0);
    let qx = (x - center_x).abs() - half_x;
    let qy = (y - center_y).abs() - half_y;
    let outside_x = qx.max(0.0);
    let outside_y = qy.max(0.0);
    let outside = (outside_x * outside_x + outside_y * outside_y).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - radius
}

fn unpack_rgb(color: u32) -> [u8; 3] {
    [
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    ]
}

fn blend_rgb(base: [u8; 3], overlay: [u8; 3], alpha: f32) -> [u8; 3] {
    if alpha <= 0.0 {
        return base;
    }
    if alpha >= 1.0 {
        return overlay;
    }

    [
        blend_channel(base[0], overlay[0], alpha),
        blend_channel(base[1], overlay[1], alpha),
        blend_channel(base[2], overlay[2], alpha),
    ]
}

fn blend_channel(base: u8, overlay: u8, alpha: f32) -> u8 {
    (f32::from(base) + (f32::from(overlay) - f32::from(base)) * alpha).round() as u8
}

fn fit_image(area: Rect, source_width: u32, source_height: u32) -> Rect {
    if area.width == 0 || area.height == 0 || source_width == 0 || source_height == 0 {
        return area;
    }

    let area_width = u32::from(area.width);
    let area_height = u32::from(area.height);
    let source_width = u64::from(source_width);
    let source_height = u64::from(source_height);
    let (width, height) =
        if source_width * u64::from(area_height) > source_height * u64::from(area_width) {
            let width = area_width;
            let height = rounded_div(u64::from(width) * source_height, source_width)
                .max(1)
                .min(area_height);
            (width, height)
        } else {
            let height = area_height;
            let width = rounded_div(u64::from(height) * source_width, source_height)
                .max(1)
                .min(area_width);
            (width, height)
        };

    Rect {
        x: area.x + ((area_width - width) / 2) as i16,
        y: area.y + ((area_height - height) / 2) as i16,
        width: width as u16,
        height: height as u16,
    }
}

fn rounded_div(value: u64, divisor: u64) -> u32 {
    ((value + divisor / 2) / divisor) as u32
}

fn border_thickness(selected: bool) -> u16 {
    if selected { 4 } else { 2 }
}

fn union_rect(a: Rect, b: Rect, max_width: u16, max_height: u16) -> Rect {
    let left = a.x.min(b.x).max(0);
    let top = a.y.min(b.y).max(0);
    let right = rect_right(a).max(rect_right(b)).min(i32::from(max_width));
    let bottom = rect_bottom(a)
        .max(rect_bottom(b))
        .min(i32::from(max_height));
    Rect {
        x: left,
        y: top,
        width: (right - i32::from(left)).max(0) as u16,
        height: (bottom - i32::from(top)).max(0) as u16,
    }
}

fn rect_right(rect: Rect) -> i32 {
    i32::from(rect.x) + i32::from(rect.width)
}

fn rect_bottom(rect: Rect) -> i32 {
    i32::from(rect.y) + i32::from(rect.height)
}

fn rounded_radius(rect: Rect, radius: u16) -> u16 {
    radius.min(rect.width / 2).min(rect.height / 2)
}

fn rounded_rect_spans(rect: Rect, radius: u16) -> Vec<Rectangle> {
    if radius == 0 {
        return vec![Rectangle {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }];
    }

    let mut spans = Vec::with_capacity(rect.height as usize);
    for row in 0..rect.height {
        let offset = rounded_row_offset(radius, row, rect.height);
        let width = rect.width.saturating_sub(offset.saturating_mul(2));
        if width == 0 {
            continue;
        }
        push_span(
            &mut spans,
            Rectangle {
                x: rect.x + offset as i16,
                y: rect.y + row as i16,
                width,
                height: 1,
            },
        );
    }
    spans
}

fn rounded_row_offset(radius: u16, row: u16, height: u16) -> u16 {
    if row < radius {
        corner_offset(radius, row)
    } else if row >= height.saturating_sub(radius) {
        corner_offset(radius, height.saturating_sub(row).saturating_sub(1))
    } else {
        0
    }
}

fn corner_offset(radius: u16, row_from_edge: u16) -> u16 {
    let radius = i32::from(radius);
    let y = radius - 1 - i32::from(row_from_edge);
    let inside = integer_sqrt((radius * radius - y * y) as u32) as i32;
    (radius - inside).max(0) as u16
}

fn integer_sqrt(value: u32) -> u32 {
    (value as f64).sqrt().floor() as u32
}

fn push_span(spans: &mut Vec<Rectangle>, next: Rectangle) {
    if let Some(previous) = spans.last_mut()
        && previous.x == next.x
        && previous.width == next.width
        && previous.y.saturating_add(previous.height as i16) == next.y
    {
        previous.height = previous.height.saturating_add(next.height);
        return;
    }
    spans.push(next);
}
