use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as CompositeConnectionExt, Redirect};
use x11rb::protocol::render::{
    ConnectionExt as RenderConnectionExt, CreatePictureAux, PictOp, Pictformat, Picture,
    QueryPictFormatsReply, Transform,
};
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as XprotoConnectionExt, CreateGCAux, CreateWindowAux, EventMask, Font,
    Gcontext, GrabMode, GrabStatus, Pixmap, PropMode, Rectangle, SubwindowMode, Visualid, Window,
    WindowClass,
};
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;
use x11rb::{CURRENT_TIME, NONE};

use crate::layout::{Layout, Rect};
use crate::windows::WindowInfo;
use crate::x11::X11Context;

const COLOR_BACKGROUND: u32 = 0x101418;
const COLOR_CELL: u32 = 0x202832;
const COLOR_CELL_HOVER: u32 = 0x2b3642;
const COLOR_BORDER: u32 = 0x5f6f7f;
const COLOR_SELECTED: u32 = 0x4ea1ff;
const COLOR_TEXT: u32 = 0xe8edf2;
const COLOR_ERROR: u32 = 0x66303a;
const APP_ID: &str = "tobyscope-x11";

pub struct Renderer {
    overlay: Overlay,
    thumbnails: Vec<ThumbnailSlot>,
}

struct Overlay {
    window: Window,
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
    error: Gcontext,
    font: Font,
}

struct ThumbnailSlot {
    window: Window,
    redirected: bool,
    state: ThumbnailState,
}

enum ThumbnailState {
    Ready(Thumbnail),
    Error(String),
}

struct Thumbnail {
    pixmap: Pixmap,
    picture: Picture,
    width: u16,
    height: u16,
}

impl Renderer {
    pub fn new(ctx: &X11Context, windows: &[WindowInfo], debug: bool) -> Result<Self> {
        let formats = ctx
            .conn
            .render_query_pict_formats()
            .context("failed to request XRender pict formats")?
            .reply()
            .context("failed to read XRender pict formats")?;
        let overlay = Overlay::create(ctx, &formats)?;
        let thumbnails = windows
            .iter()
            .map(|window| ThumbnailSlot::create(ctx, &formats, window, debug))
            .collect::<Vec<_>>();

        Ok(Self {
            overlay,
            thumbnails,
        })
    }

    pub fn overlay_window(&self) -> Window {
        self.overlay.window
    }

    pub fn size(&self) -> (u16, u16) {
        (self.overlay.width, self.overlay.height)
    }

    pub fn update_size(&mut self, width: u16, height: u16) {
        self.overlay.width = width;
        self.overlay.height = height;
    }

    pub fn redraw(
        &self,
        ctx: &X11Context,
        windows: &[WindowInfo],
        layout: &Layout,
        selected: usize,
    ) -> Result<()> {
        self.fill(ctx, self.overlay.rect(), self.overlay.gcs.bg)?;

        for (index, (window, item)) in windows.iter().zip(&layout.items).enumerate() {
            let cell_gc = if index == selected {
                self.overlay.gcs.cell_hover
            } else {
                self.overlay.gcs.cell
            };
            self.fill(ctx, item.cell, cell_gc)?;

            match self.thumbnails.get(index).map(|slot| &slot.state) {
                Some(ThumbnailState::Ready(thumbnail)) => {
                    self.draw_thumbnail(ctx, thumbnail, item.preview)?;
                }
                Some(ThumbnailState::Error(error)) => {
                    self.draw_preview_error(ctx, item.preview, error)?;
                }
                None => {
                    self.draw_preview_error(ctx, item.preview, "preview missing")?;
                }
            }

            self.draw_label(ctx, item.cell, &window.name)?;
            self.draw_border(ctx, item.cell, index == selected)?;
        }

        ctx.conn.flush().context("failed to flush redraw")?;
        Ok(())
    }

    pub fn cleanup(&mut self, ctx: &X11Context) -> Result<()> {
        for slot in &self.thumbnails {
            if let ThumbnailState::Ready(thumbnail) = &slot.state {
                if let Ok(cookie) = ctx.conn.render_free_picture(thumbnail.picture) {
                    cookie.ignore_error();
                }
                if let Ok(cookie) = ctx.conn.free_pixmap(thumbnail.pixmap) {
                    cookie.ignore_error();
                }
            }
            if slot.redirected {
                if let Ok(cookie) = ctx
                    .conn
                    .composite_unredirect_window(slot.window, Redirect::AUTOMATIC)
                {
                    cookie.ignore_error();
                }
            }
        }

        self.overlay.destroy(ctx)?;
        ctx.conn.flush().context("failed to flush cleanup")?;
        Ok(())
    }

    fn draw_thumbnail(&self, ctx: &X11Context, thumbnail: &Thumbnail, rect: Rect) -> Result<()> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }

        let transform = Transform {
            matrix11: fixed_ratio(thumbnail.width, rect.width),
            matrix12: 0,
            matrix13: 0,
            matrix21: 0,
            matrix22: fixed_ratio(thumbnail.height, rect.height),
            matrix23: 0,
            matrix31: 0,
            matrix32: 0,
            matrix33: 1 << 16,
        };

        ctx.conn
            .render_set_picture_transform(thumbnail.picture, transform)
            .context("failed to set thumbnail transform")?
            .check()
            .context("XRender rejected thumbnail transform")?;
        ctx.conn
            .render_composite(
                PictOp::SRC,
                thumbnail.picture,
                NONE,
                self.overlay.picture,
                0,
                0,
                0,
                0,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            )
            .context("failed to composite thumbnail")?
            .check()
            .context("XRender rejected thumbnail composite")?;
        Ok(())
    }

    fn draw_preview_error(&self, ctx: &X11Context, rect: Rect, message: &str) -> Result<()> {
        self.fill(ctx, rect, self.overlay.gcs.error)?;
        let label = format!("preview error: {message}");
        self.draw_text(ctx, rect.x + 8, rect.y + (rect.height as i16 / 2), &label)
    }

    fn draw_label(&self, ctx: &X11Context, cell: Rect, label: &str) -> Result<()> {
        let y = cell.y.saturating_add(cell.height as i16).saturating_sub(11);
        self.draw_text(ctx, cell.x + 10, y, label)
    }

    fn draw_border(&self, ctx: &X11Context, rect: Rect, selected: bool) -> Result<()> {
        let thickness = if selected { 4 } else { 2 };
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
                self.overlay.window,
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

    fn draw_text(&self, ctx: &X11Context, x: i16, y: i16, text: &str) -> Result<()> {
        let bytes = ascii_label(text, 96);
        if bytes.is_empty() {
            return Ok(());
        }
        ctx.conn
            .image_text8(self.overlay.window, self.overlay.gcs.text, x, y, &bytes)
            .context("failed to draw text")?;
        Ok(())
    }
}

impl Overlay {
    fn create(ctx: &X11Context, formats: &QueryPictFormatsReply) -> Result<Self> {
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
        let values = CreateWindowAux::new()
            .background_pixel(COLOR_BACKGROUND)
            .border_pixel(COLOR_BACKGROUND)
            .override_redirect(1u32)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::POINTER_MOTION
                    | EventMask::STRUCTURE_NOTIFY,
            );

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

        let gcs = Gcs::create(ctx, window)?;

        ctx.conn
            .map_window(window)
            .context("failed to map overlay")?
            .check()
            .context("X11 rejected overlay map")?;
        ctx.conn
            .configure_window(
                window,
                &x11rb::protocol::xproto::ConfigureWindowAux::new()
                    .stack_mode(x11rb::protocol::xproto::StackMode::ABOVE),
            )
            .context("failed to raise overlay")?;

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

        ctx.conn.flush().context("failed to flush overlay setup")?;

        Ok(Self {
            window,
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

    fn destroy(&self, ctx: &X11Context) -> Result<()> {
        if let Ok(cookie) = ctx.conn.ungrab_keyboard(CURRENT_TIME) {
            cookie.ignore_error();
        }
        if let Ok(cookie) = ctx.conn.render_free_picture(self.picture) {
            cookie.ignore_error();
        }
        self.gcs.destroy(ctx);
        if let Ok(cookie) = ctx.conn.destroy_window(self.window) {
            cookie.ignore_error();
        }
        Ok(())
    }
}

impl Gcs {
    fn create(ctx: &X11Context, drawable: Window) -> Result<Self> {
        let font = ctx
            .conn
            .generate_id()
            .context("failed to allocate font id")?;
        ctx.conn
            .open_font(font, b"fixed")
            .context("failed to open X11 fixed font")?
            .check()
            .context("X11 rejected fixed font")?;

        Ok(Self {
            bg: create_gc(ctx, drawable, COLOR_BACKGROUND, COLOR_BACKGROUND, font)?,
            cell: create_gc(ctx, drawable, COLOR_CELL, COLOR_CELL, font)?,
            cell_hover: create_gc(ctx, drawable, COLOR_CELL_HOVER, COLOR_CELL_HOVER, font)?,
            border: create_gc(ctx, drawable, COLOR_BORDER, COLOR_BORDER, font)?,
            selected: create_gc(ctx, drawable, COLOR_SELECTED, COLOR_SELECTED, font)?,
            text: create_gc(ctx, drawable, COLOR_TEXT, COLOR_BACKGROUND, font)?,
            error: create_gc(ctx, drawable, COLOR_ERROR, COLOR_ERROR, font)?,
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
            self.error,
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

impl ThumbnailSlot {
    fn create(
        ctx: &X11Context,
        formats: &QueryPictFormatsReply,
        window: &WindowInfo,
        debug: bool,
    ) -> Self {
        match create_thumbnail(ctx, formats, window) {
            Ok(thumbnail) => Self {
                window: window.id,
                redirected: true,
                state: ThumbnailState::Ready(thumbnail),
            },
            Err(error) => {
                if debug {
                    eprintln!(
                        "preview: 0x{:08x} `{}` failed: {error:#}",
                        window.id, window.name
                    );
                }
                Self {
                    window: window.id,
                    redirected: false,
                    state: ThumbnailState::Error(error.to_string()),
                }
            }
        }
    }
}

fn create_thumbnail(
    ctx: &X11Context,
    formats: &QueryPictFormatsReply,
    window: &WindowInfo,
) -> Result<Thumbnail> {
    let format = pict_format_for_visual(formats, window.geometry.visual).with_context(|| {
        format!(
            "no XRender pict format for visual 0x{:08x}",
            window.geometry.visual
        )
    })?;

    let mut redirected = false;
    let mut pixmap: Option<Pixmap> = None;
    let mut picture: Option<Picture> = None;

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
            .context("failed to allocate pixmap id")?;
        pixmap = Some(pixmap_id);
        ctx.conn
            .composite_name_window_pixmap(window.id, pixmap_id)
            .context("failed to request named window pixmap")?
            .check()
            .context("XComposite could not name the window pixmap")?;

        let picture_id = ctx
            .conn
            .generate_id()
            .context("failed to allocate picture id")?;
        picture = Some(picture_id);
        ctx.conn
            .render_create_picture(
                picture_id,
                pixmap_id,
                format,
                &CreatePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS),
            )
            .context("failed to create source picture for thumbnail")?
            .check()
            .context("XRender rejected source picture for thumbnail")?;

        if let Ok(cookie) = ctx.conn.render_set_picture_filter(picture_id, b"best", &[]) {
            cookie.ignore_error();
        }

        Ok(Thumbnail {
            pixmap: pixmap
                .take()
                .expect("pixmap was set before thumbnail success"),
            picture: picture
                .take()
                .expect("picture was set before thumbnail success"),
            width: window.geometry.width,
            height: window.geometry.height,
        })
    })();

    if result.is_err() {
        if let Some(picture_id) = picture {
            if let Ok(cookie) = ctx.conn.render_free_picture(picture_id) {
                cookie.ignore_error();
            }
        }
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
    }

    result
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
    drawable: Window,
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

fn pict_format_for_visual(formats: &QueryPictFormatsReply, visual: Visualid) -> Option<Pictformat> {
    formats
        .screens
        .iter()
        .flat_map(|screen| &screen.depths)
        .flat_map(|depth| &depth.visuals)
        .find(|candidate| candidate.visual == visual)
        .map(|candidate| candidate.format)
}

fn fixed_ratio(source: u16, destination: u16) -> i32 {
    if destination == 0 {
        return 1 << 16;
    }
    (((source as i64) << 16) / destination as i64) as i32
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
