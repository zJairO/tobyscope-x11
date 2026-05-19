use anyhow::{Context, Result, bail};
use image::RgbaImage;
use image::imageops::FilterType;
use x11rb::connection::Connection;
use x11rb::protocol::render::{
    ConnectionExt as RenderConnectionExt, CreatePictureAux, Pictformat, Picture,
    QueryPictFormatsReply,
};
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as XprotoConnectionExt, CreateGCAux, CreateWindowAux, EventMask, Font,
    Gcontext, GrabMode, GrabStatus, ImageFormat, Pixmap, PropMode, Rectangle, Window, WindowClass,
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
    config: AppConfig,
}

struct Overlay {
    window: Window,
    buffer: Pixmap,
    picture: Picture,
    gcs: Gcs,
    width: u16,
    height: u16,
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
            config: config.clone(),
        })
    }

    pub fn overlay_window(&self) -> Window {
        self.overlay.window
    }

    pub fn size(&self) -> (u16, u16) {
        (self.overlay.width, self.overlay.height)
    }

    pub fn update_size(&mut self, ctx: &X11Context, width: u16, height: u16) -> Result<()> {
        if self.overlay.width == width && self.overlay.height == height {
            return Ok(());
        }
        self.overlay.resize_buffer(ctx, width, height)?;
        self.overlay.width = width;
        self.overlay.height = height;
        Ok(())
    }

    pub fn raise(&self, ctx: &X11Context) -> Result<()> {
        self.overlay.raise(ctx)
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
            let cell_gc = if index == selected {
                self.overlay.gcs.cell_hover
            } else {
                self.overlay.gcs.cell
            };
            self.draw_card_shadow(ctx, item.cell)?;
            self.draw_cell_background(ctx, item.cell, cell_gc, index == selected)?;

            if self
                .thumbnails
                .get(index)
                .and_then(|slot| slot.state.image())
                .is_some()
            {
                self.draw_cached_image(ctx, index, item.preview, cell_gc)?;
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
            )?;
            let label = window.program_name();
            self.draw_label(ctx, item.cell, label.as_ref())?;
            if !self.config.ui.rounded_corners {
                self.draw_border(ctx, item.cell, index == selected)?;
            }
        }

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
                .context("failed to copy back buffer to overlay")?
                .check()
                .context("X11 rejected overlay back-buffer copy")?;
        } else {
            for item in &layout.items {
                self.copy_card_to_overlay(ctx, self.card_paint_bounds(item.cell))?;
            }
        }
        ctx.conn.flush().context("failed to flush redraw")?;
        Ok(())
    }

    pub fn cleanup(&mut self, ctx: &X11Context) -> Result<()> {
        for slot in &mut self.thumbnails {
            slot.release_prepared(ctx);
        }
        self.overlay.destroy(ctx)?;
        ctx.conn.flush().context("failed to flush cleanup")?;
        Ok(())
    }

    fn draw_cached_image(
        &mut self,
        ctx: &X11Context,
        index: usize,
        rect: Rect,
        corner_gc: Gcontext,
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
        if self.config.ui.rounded_corners {
            self.cover_rounded_corners(ctx, target, self.config.ui.corner_radius, corner_gc)?;
        }
        Ok(())
    }

    fn copy_card_to_overlay(&self, ctx: &X11Context, rect: Rect) -> Result<()> {
        let spans = if self.config.ui.rounded_corners {
            rounded_rect_spans(rect, rounded_radius(rect, self.config.ui.corner_radius))
        } else {
            vec![Rectangle {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            }]
        };

        for span in spans {
            ctx.conn
                .copy_area(
                    self.overlay.buffer,
                    self.overlay.window,
                    self.overlay.gcs.image,
                    span.x,
                    span.y,
                    span.x,
                    span.y,
                    span.width,
                    span.height,
                )
                .context("failed to copy card buffer to overlay")?
                .check()
                .context("X11 rejected card buffer copy")?;
        }
        Ok(())
    }

    fn card_paint_bounds(&self, rect: Rect) -> Rect {
        if !self.config.ui.shadows {
            return rect;
        }

        union_rect(
            rect,
            offset_rect(
                rect,
                self.config.ui.shadow_offset_x,
                self.config.ui.shadow_offset_y,
            ),
            self.overlay.width,
            self.overlay.height,
        )
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

    fn draw_card_shadow(&self, ctx: &X11Context, rect: Rect) -> Result<()> {
        if !self.config.ui.shadows {
            return Ok(());
        }

        let shadow = offset_rect(
            rect,
            self.config.ui.shadow_offset_x,
            self.config.ui.shadow_offset_y,
        );
        let radius = if self.config.ui.rounded_corners {
            self.config
                .ui
                .shadow_radius
                .max(self.config.ui.corner_radius)
        } else {
            self.config.ui.shadow_radius
        };
        self.fill_rounded(ctx, shadow, radius, self.overlay.gcs.shadow)
    }

    fn draw_cell_background(
        &self,
        ctx: &X11Context,
        rect: Rect,
        cell_gc: Gcontext,
        selected: bool,
    ) -> Result<()> {
        if !self.config.ui.rounded_corners {
            return self.fill(ctx, rect, cell_gc);
        }

        let thickness = border_thickness(selected);
        let border_gc = if selected {
            self.overlay.gcs.selected
        } else {
            self.overlay.gcs.border
        };
        let radius = rounded_radius(rect, self.config.ui.corner_radius);
        self.fill_rounded(ctx, rect, radius, border_gc)?;

        let inner = inset_rect(rect, thickness);
        if inner.width > 0 && inner.height > 0 {
            self.fill_rounded(ctx, inner, radius.saturating_sub(thickness), cell_gc)?;
        }
        Ok(())
    }

    fn fill_preview(&self, ctx: &X11Context, rect: Rect, gc: Gcontext) -> Result<()> {
        if self.config.ui.rounded_corners {
            self.fill_rounded(ctx, rect, self.config.ui.corner_radius, gc)
        } else {
            self.fill(ctx, rect, gc)
        }
    }

    fn draw_status(&self, ctx: &X11Context, rect: Rect, message: &str, error: bool) -> Result<()> {
        let gc = if error {
            self.overlay.gcs.text
        } else {
            self.overlay.gcs.muted
        };
        self.draw_text_with_gc(ctx, gc, rect.x + 8, rect.y + 19, message, 48)
    }

    fn draw_workspace_badge(
        &self,
        ctx: &X11Context,
        cell: Rect,
        workspace: &str,
        focused: bool,
        urgent: bool,
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
        self.draw_text_without_background(ctx, rect.x + 6, rect.y + 15, &label, 20)
    }

    fn draw_label(&self, ctx: &X11Context, cell: Rect, label: &str) -> Result<()> {
        if !self.config.ui.show_program_name {
            return Ok(());
        }
        let y = cell.y.saturating_add(cell.height as i16).saturating_sub(12);
        let max_chars = (usize::from(cell.width) / 7)
            .saturating_sub(2)
            .clamp(12, 120);
        self.draw_text_without_background(ctx, cell.x + 10, y, label, max_chars)
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

    fn cover_rounded_corners(
        &self,
        ctx: &X11Context,
        rect: Rect,
        radius: u16,
        gc: Gcontext,
    ) -> Result<()> {
        let radius = rounded_radius(rect, radius);
        if radius == 0 {
            return Ok(());
        }

        let rectangles = rounded_corner_cutouts(rect, radius);
        if rectangles.is_empty() {
            return Ok(());
        }
        ctx.conn
            .poly_fill_rectangle(self.overlay.buffer, gc, &rectangles)
            .context("failed to cover rounded thumbnail corners")?;
        Ok(())
    }

    fn draw_text(&self, ctx: &X11Context, x: i16, y: i16, text: &str, max: usize) -> Result<()> {
        self.draw_text_with_gc(ctx, self.overlay.gcs.text, x, y, text, max)
    }

    fn draw_text_with_gc(
        &self,
        ctx: &X11Context,
        gc: Gcontext,
        x: i16,
        y: i16,
        text: &str,
        max: usize,
    ) -> Result<()> {
        let bytes = ascii_label(text, max);
        if bytes.is_empty() {
            return Ok(());
        }
        ctx.conn
            .image_text8(self.overlay.buffer, gc, x, y, &bytes)
            .context("failed to draw text")?;
        Ok(())
    }

    fn draw_text_without_background(
        &self,
        ctx: &X11Context,
        x: i16,
        y: i16,
        text: &str,
        max: usize,
    ) -> Result<()> {
        let bytes = ascii_label(text, max);
        if bytes.is_empty() {
            return Ok(());
        }

        let mut items = Vec::with_capacity(bytes.len() + 2);
        for chunk in bytes.chunks(254) {
            items.push(chunk.len() as u8);
            items.push(0);
            items.extend_from_slice(chunk);
        }

        ctx.conn
            .poly_text8(self.overlay.buffer, self.overlay.gcs.text, x, y, &items)
            .context("failed to draw text without background")?;
        Ok(())
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
    ) -> Result<Option<Pixmap>> {
        if self
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.width == rect.width && prepared.height == rect.height)
        {
            return Ok(self.prepared.as_ref().map(|prepared| prepared.pixmap));
        }

        let Some((pixmap, width, height)) = (|| -> Result<Option<(Pixmap, u16, u16)>> {
            let Some(image) = self.state.image() else {
                return Ok(None);
            };
            let scaled = image::imageops::resize(
                image,
                u32::from(rect.width),
                u32::from(rect.height),
                FilterType::Triangle,
            );
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

        ctx.conn
            .map_window(window)
            .context("failed to map overlay")?
            .check()
            .context("X11 rejected overlay map")?;
        raise_window(ctx, window)?;

        let grab = ctx
            .conn
            .grab_keyboard(
                false,
                window,
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
                window,
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
            bail!(
                "could not grab pointer for overview overlay: {:?}",
                pointer_grab.status
            );
        }

        ctx.conn.flush().context("failed to flush overlay setup")?;

        Ok(Self {
            window,
            buffer,
            picture,
            gcs,
            width: ctx.width,
            height: ctx.height,
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

    fn resize_buffer(&mut self, ctx: &X11Context, width: u16, height: u16) -> Result<()> {
        let next = create_buffer(ctx, self.window, width, height)?;
        if let Ok(cookie) = ctx.conn.free_pixmap(self.buffer) {
            cookie.ignore_error();
        }
        self.buffer = next;
        Ok(())
    }

    fn raise(&self, ctx: &X11Context) -> Result<()> {
        raise_window(ctx, self.window)
    }

    fn destroy(&self, ctx: &X11Context) -> Result<()> {
        if let Ok(cookie) = ctx.conn.ungrab_keyboard(CURRENT_TIME) {
            cookie.ignore_error();
        }
        if let Ok(cookie) = ctx.conn.ungrab_pointer(CURRENT_TIME) {
            cookie.ignore_error();
        }
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
        font_name: &str,
    ) -> Result<Self> {
        let font = ctx
            .conn
            .generate_id()
            .context("failed to allocate font id")?;
        ctx.conn
            .open_font(font, font_name.as_bytes())
            .with_context(|| format!("failed to open X11 font `{font_name}`"))?
            .check()
            .with_context(|| format!("X11 rejected font `{font_name}`"))?;

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

fn ascii_label(text: &str, max: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(max.min(text.len()));
    for ch in text.chars() {
        if out.len() >= max {
            break;
        }
        if ch.is_ascii_graphic() || ch == ' ' {
            out.push(ch as u8);
        } else if !out.last().is_some_and(|last| *last == b'?') {
            out.push(b'?');
        }
    }
    out
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

fn inset_rect(rect: Rect, amount: u16) -> Rect {
    let inset_x = amount.min(rect.width / 2);
    let inset_y = amount.min(rect.height / 2);
    Rect {
        x: rect.x + inset_x as i16,
        y: rect.y + inset_y as i16,
        width: rect.width.saturating_sub(inset_x.saturating_mul(2)),
        height: rect.height.saturating_sub(inset_y.saturating_mul(2)),
    }
}

fn offset_rect(rect: Rect, offset_x: i16, offset_y: i16) -> Rect {
    Rect {
        x: rect.x.saturating_add(offset_x),
        y: rect.y.saturating_add(offset_y),
        width: rect.width,
        height: rect.height,
    }
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

fn rounded_corner_cutouts(rect: Rect, radius: u16) -> Vec<Rectangle> {
    let mut cutouts = Vec::new();
    for row in 0..radius {
        let offset = corner_offset(radius, row);
        if offset == 0 {
            continue;
        }

        let top_y = rect.y + row as i16;
        push_span(
            &mut cutouts,
            Rectangle {
                x: rect.x,
                y: top_y,
                width: offset,
                height: 1,
            },
        );
        push_span(
            &mut cutouts,
            Rectangle {
                x: rect.x + rect.width as i16 - offset as i16,
                y: top_y,
                width: offset,
                height: 1,
            },
        );

        let bottom_y = rect.y + rect.height as i16 - row as i16 - 1;
        if bottom_y != top_y {
            push_span(
                &mut cutouts,
                Rectangle {
                    x: rect.x,
                    y: bottom_y,
                    width: offset,
                    height: 1,
                },
            );
            push_span(
                &mut cutouts,
                Rectangle {
                    x: rect.x + rect.width as i16 - offset as i16,
                    y: bottom_y,
                    width: offset,
                    height: 1,
                },
            );
        }
    }
    cutouts
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
