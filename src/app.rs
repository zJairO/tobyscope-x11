use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::ButtonIndex;

use crate::atoms::Atoms;
use crate::cache::ThumbnailCache;
use crate::capture::{CaptureScope, CaptureSweep, SweepStatus};
use crate::config::AppConfig;
use crate::input::{KeyAction, KeyMap};
use crate::layout::{self, Layout};
use crate::render::Renderer;
use crate::windows::{self, WindowInfo};
use crate::x11::X11Context;

pub struct OverviewApp {
    ctx: X11Context,
    atoms: Atoms,
    windows: Vec<WindowInfo>,
    config: AppConfig,
    cache: ThumbnailCache,
    renderer: Renderer,
    keymap: KeyMap,
    selected: usize,
    layout: Option<Layout>,
    sweep: Option<CaptureSweep>,
    visible: bool,
    debug: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTick {
    Idle,
    Closed,
    Focused,
}

impl OverviewApp {
    pub fn new(
        ctx: X11Context,
        atoms: Atoms,
        windows: Vec<WindowInfo>,
        config: AppConfig,
        debug: bool,
    ) -> Result<Self> {
        let cache = ThumbnailCache::new(
            &windows,
            std::time::Duration::from_secs(config.thumbnails.refresh_after_seconds),
            debug,
        )?;
        cache.prune()?;
        let renderer = Renderer::new(&ctx, &windows, &cache, &config, debug)?;
        let keymap = KeyMap::load(&ctx)?;
        Ok(Self {
            ctx,
            atoms,
            windows,
            config,
            cache,
            renderer,
            keymap,
            selected: 0,
            layout: None,
            sweep: None,
            visible: false,
            debug,
        })
    }

    pub fn run(mut self) -> Result<()> {
        let result = self.show_once();
        let cleanup = self.cleanup();
        result?;
        cleanup
    }

    pub fn show_once(&mut self) -> Result<()> {
        let result = self.event_loop();
        let hide = self.renderer.hide(&self.ctx);

        let target = match result {
            Ok(target) => target,
            Err(error) => {
                hide?;
                return Err(error);
            }
        };
        hide?;

        if let Some(window) = target {
            windows::focus_window(&self.ctx, &self.atoms, &window, self.debug)?;
        }

        Ok(())
    }

    pub fn prewarm(&mut self) -> Result<()> {
        self.prepare_frame()
    }

    pub fn cleanup(&mut self) -> Result<()> {
        self.renderer.cleanup(&self.ctx)
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    pub fn show_prepared(&mut self) -> Result<()> {
        let resized = self.renderer.sync_root_size(&self.ctx)?;
        if resized || self.layout.is_none() {
            self.prepare_frame()?;
        }
        self.renderer.show(&self.ctx)?;
        self.visible = true;
        if let Some(layout) = self.layout.as_ref() {
            self.renderer.present(&self.ctx, layout)?;
        }
        self.start_capture(CaptureScope::AllWorkspaces);
        Ok(())
    }

    pub fn start_background_capture(&mut self) {
        if !self.visible && self.sweep.is_none() {
            self.start_capture(CaptureScope::CurrentWorkspaceOnly);
        }
    }

    pub fn tick_nonblocking(&mut self) -> Result<AppTick> {
        while let Some(event) = self
            .ctx
            .conn
            .poll_for_event()
            .context("failed while polling for X11 event")?
        {
            let mut layout = self.current_layout()?;
            if let Some(result) = self.handle_event(event, &mut layout)? {
                self.layout = Some(layout);
                return self.finish_interaction(result);
            }
            self.layout = Some(layout);
        }

        if let Some(active_sweep) = self.sweep.as_mut() {
            match active_sweep.step(&self.ctx, &self.cache, &self.windows, &mut self.renderer)? {
                SweepStatus::Updated => {
                    self.prepare_frame()?;
                }
                SweepStatus::Finished => {
                    self.sweep = None;
                    if self.visible {
                        self.prepare_frame()?;
                    }
                }
            }
        }

        Ok(AppTick::Idle)
    }

    pub fn tick_background(&mut self) -> Result<()> {
        if let Some(active_sweep) = self.sweep.as_mut() {
            match active_sweep.step(&self.ctx, &self.cache, &self.windows, &mut self.renderer)? {
                SweepStatus::Updated => {
                    self.prepare_frame()?;
                }
                SweepStatus::Finished => {
                    self.sweep = None;
                    self.prepare_frame()?;
                }
            }
        }

        Ok(())
    }

    pub fn hide_without_focus(&mut self) -> Result<()> {
        if let Some(sweep) = self.sweep.as_mut() {
            sweep.cancel();
        }
        self.sweep = None;
        self.visible = false;
        self.renderer.hide(&self.ctx)
    }

    fn event_loop(&mut self) -> Result<Option<WindowInfo>> {
        let mut sweep = Some(CaptureSweep::new(
            &self.windows,
            &self.cache,
            self.config.thumbnails.max_cache_edge,
            self.debug,
            CaptureScope::AllWorkspaces,
        ));
        self.redraw()?;
        self.renderer.show(&self.ctx)?;
        let mut layout = self.redraw()?;

        loop {
            while let Some(event) = self
                .ctx
                .conn
                .poll_for_event()
                .context("failed while polling for X11 event")?
            {
                if let Some(result) = self.handle_event(event, &mut layout)? {
                    if result.is_none()
                        && let Some(sweep) = sweep.as_mut()
                    {
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
                if result.is_none()
                    && let Some(sweep) = sweep.as_mut()
                {
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
                            let window = self.windows[self.selected].clone();
                            if self.debug {
                                eprintln!(
                                    "confirm: keyboard selected index={} ws={} con={:?} window=0x{:08x} name={}",
                                    self.selected,
                                    window.workspace,
                                    window.i3_con_id,
                                    window.id,
                                    window.name
                                );
                            }
                            return Ok(Some(Some(window)));
                        }
                        KeyAction::Left => self.move_left(layout),
                        KeyAction::Right => self.move_right(layout),
                        KeyAction::Up => self.move_up(layout),
                        KeyAction::Down => self.move_down(layout),
                    }
                    *layout = self.redraw()?;
                }
            }
            Event::KeyRelease(event) => {
                if self.keymap.is_super_keycode(event.detail) {
                    return Ok(Some(None));
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
                        let window = self.windows[self.selected].clone();
                        if self.debug {
                            eprintln!(
                                "confirm: click selected index={} at {:+}{:+} ws={} con={:?} window=0x{:08x} name={}",
                                self.selected,
                                event.event_x,
                                event.event_y,
                                window.workspace,
                                window.i3_con_id,
                                window.id,
                                window.name
                            );
                        }
                        return Ok(Some(Some(window)));
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
        let layout = layout::compute(width, height, &self.windows, &self.config);
        self.renderer
            .redraw(&self.ctx, &self.windows, &layout, self.selected)?;
        Ok(layout)
    }

    fn prepare_frame(&mut self) -> Result<()> {
        let layout = self.redraw()?;
        self.layout = Some(layout);
        Ok(())
    }

    fn current_layout(&mut self) -> Result<Layout> {
        if let Some(layout) = self.layout.clone() {
            Ok(layout)
        } else {
            self.redraw()
        }
    }

    fn start_capture(&mut self, scope: CaptureScope) {
        let sweep = CaptureSweep::new(
            &self.windows,
            &self.cache,
            self.config.thumbnails.max_cache_edge,
            self.debug,
            scope,
        );
        self.sweep = (!sweep.is_empty()).then_some(sweep);
    }

    fn finish_interaction(&mut self, result: Option<WindowInfo>) -> Result<AppTick> {
        if result.is_none() {
            if self.debug {
                eprintln!("overview: closing without focus");
            }
            self.hide_without_focus()?;
            return Ok(AppTick::Closed);
        }

        self.sweep = None;
        self.visible = false;
        self.renderer.hide(&self.ctx)?;
        if let Some(window) = result {
            windows::focus_window(&self.ctx, &self.atoms, &window, self.debug)?;
        }
        Ok(AppTick::Focused)
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
