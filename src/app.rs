use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::ButtonIndex;

use crate::atoms::Atoms;
use crate::cache::ThumbnailCache;
use crate::capture::{CaptureSweep, SweepStatus};
use crate::input::{KeyAction, KeyMap};
use crate::layout::{self, Layout};
use crate::render::Renderer;
use crate::windows::{self, WindowInfo};
use crate::x11::X11Context;

pub struct OverviewApp {
    ctx: X11Context,
    atoms: Atoms,
    windows: Vec<WindowInfo>,
    cache: ThumbnailCache,
    renderer: Renderer,
    keymap: KeyMap,
    selected: usize,
    debug: bool,
}

impl OverviewApp {
    pub fn new(
        ctx: X11Context,
        atoms: Atoms,
        windows: Vec<WindowInfo>,
        debug: bool,
    ) -> Result<Self> {
        let cache = ThumbnailCache::new(&windows, debug)?;
        cache.prune()?;
        let renderer = Renderer::new(&ctx, &windows, &cache, debug)?;
        let keymap = KeyMap::load(&ctx)?;
        Ok(Self {
            ctx,
            atoms,
            windows,
            cache,
            renderer,
            keymap,
            selected: 0,
            debug,
        })
    }

    pub fn run(mut self) -> Result<()> {
        let result = self.event_loop();
        let cleanup = self.renderer.cleanup(&self.ctx);

        let target = match result {
            Ok(target) => target,
            Err(error) => {
                cleanup?;
                return Err(error);
            }
        };
        cleanup?;

        if let Some(window) = target {
            windows::focus_window(&self.ctx, &self.atoms, &window, self.debug)?;
        }

        Ok(())
    }

    fn event_loop(&mut self) -> Result<Option<WindowInfo>> {
        let mut sweep = Some(CaptureSweep::new(&self.windows, &self.cache, self.debug));
        let mut layout = self.redraw()?;

        loop {
            while let Some(event) = self
                .ctx
                .conn
                .poll_for_event()
                .context("failed while polling for X11 event")?
            {
                if let Some(result) = self.handle_event(event, &mut layout)? {
                    if let Some(sweep) = sweep.as_mut() {
                        sweep.cancel();
                    }
                    return Ok(result);
                }
            }

            if let Some(active_sweep) = sweep.as_mut() {
                match active_sweep.step(
                    &self.ctx,
                    &self.cache,
                    &self.windows,
                    &mut self.renderer,
                )? {
                    SweepStatus::Updated => {
                        layout = self.redraw()?;
                        continue;
                    }
                    SweepStatus::Finished => {
                        sweep = None;
                        layout = self.redraw()?;
                        continue;
                    }
                }
            }

            let event = self
                .ctx
                .conn
                .wait_for_event()
                .context("failed while waiting for X11 event")?;
            if let Some(result) = self.handle_event(event, &mut layout)? {
                if let Some(sweep) = sweep.as_mut() {
                    sweep.cancel();
                }
                return Ok(result);
            }
        }
    }

    fn handle_event(
        &mut self,
        event: Event,
        layout: &mut Layout,
    ) -> Result<Option<Option<WindowInfo>>> {
        match event {
            Event::Expose(_) => {
                *layout = self.redraw()?;
            }
            Event::ConfigureNotify(event) if event.window == self.renderer.overlay_window() => {
                self.renderer
                    .update_size(&self.ctx, event.width, event.height)?;
                *layout = self.redraw()?;
            }
            Event::KeyPress(event) => {
                if let Some(action) = self.keymap.action_for_keycode(event.detail) {
                    match action {
                        KeyAction::Close => return Ok(Some(None)),
                        KeyAction::Confirm => {
                            return Ok(Some(Some(self.windows[self.selected].clone())));
                        }
                        KeyAction::Left => self.move_left(layout),
                        KeyAction::Right => self.move_right(layout),
                        KeyAction::Up => self.move_up(layout),
                        KeyAction::Down => self.move_down(layout),
                    }
                    *layout = self.redraw()?;
                }
            }
            Event::MotionNotify(event) => {
                if let Some(index) = layout::hit_test(layout, event.event_x, event.event_y) {
                    if index != self.selected {
                        self.selected = index;
                        *layout = self.redraw()?;
                    }
                }
            }
            Event::ButtonPress(event) => {
                if u8::from(event.detail) == u8::from(ButtonIndex::M1) {
                    if let Some(index) = layout::hit_test(layout, event.event_x, event.event_y) {
                        self.selected = index;
                        return Ok(Some(Some(self.windows[self.selected].clone())));
                    }
                }
            }
            Event::Error(error) => {
                if self.debug {
                    eprintln!("x11 event error: {error:?}");
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn redraw(&mut self) -> Result<layout::Layout> {
        let (width, height) = self.renderer.size();
        let layout = layout::compute(width, height, &self.windows);
        self.renderer
            .redraw(&self.ctx, &self.windows, &layout, self.selected)?;
        Ok(layout)
    }

    fn move_left(&mut self, layout: &layout::Layout) {
        if self.selected % layout.columns > 0 {
            self.selected -= 1;
        }
    }

    fn move_right(&mut self, layout: &layout::Layout) {
        if self.selected + 1 < self.windows.len()
            && self.selected % layout.columns + 1 < layout.columns
        {
            self.selected += 1;
        }
    }

    fn move_up(&mut self, layout: &layout::Layout) {
        if self.selected >= layout.columns {
            self.selected -= layout.columns;
        }
    }

    fn move_down(&mut self, layout: &layout::Layout) {
        let target = self.selected + layout.columns;
        if target < self.windows.len() {
            self.selected = target;
        } else if self.selected / layout.columns < (self.windows.len() - 1) / layout.columns {
            self.selected = self.windows.len() - 1;
        }
    }
}
