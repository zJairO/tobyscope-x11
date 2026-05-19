use anyhow::{Context, Result};
use std::collections::HashSet;
use std::thread;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, CLIENT_MESSAGE_EVENT, ClientMessageData, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt, EventMask, GetPropertyReply, InputFocus, MapState, StackMode, Visualid, Window,
    WindowClass,
};
use x11rb::{CURRENT_TIME, NONE};

use crate::atoms::Atoms;
use crate::x11::X11Context;

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: Window,
    pub name: String,
    pub instance: Option<String>,
    pub class: Option<String>,
    pub workspace: String,
    pub workspace_num: Option<i32>,
    pub i3_con_id: Option<i64>,
    pub tree_order: usize,
    pub urgent: bool,
    pub focused: bool,
    pub geometry: WindowGeometry,
}

#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub depth: u8,
    pub visual: Visualid,
}

pub fn discover_windows(ctx: &X11Context, atoms: &Atoms, debug: bool) -> Result<Vec<WindowInfo>> {
    match crate::i3::discover_windows(ctx, debug) {
        Ok(windows) => return Ok(windows),
        Err(error) if debug => {
            eprintln!(
                "windows: i3 IPC discovery failed, falling back to visible X11 clients: {error:#}"
            );
        }
        Err(_) => {}
    }

    let mut ids = read_window_list(ctx, atoms.net_client_list_stacking)?;
    if ids.is_empty() {
        ids = read_window_list(ctx, atoms.net_client_list)?;
    }
    if ids.is_empty() {
        ids = query_root_children(ctx)?;
    }

    let mut seen = HashSet::new();
    let mut windows = Vec::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }

        match inspect_window(ctx, atoms, id, debug)? {
            Some(mut info) => {
                info.tree_order = windows.len();
                windows.push(info);
            }
            None => continue,
        }
    }

    if debug {
        eprintln!(
            "windows: {} visible client windows detected via X11 fallback",
            windows.len()
        );
    }

    Ok(windows)
}

pub fn print_window_list(windows: &[WindowInfo]) {
    for (index, window) in windows.iter().enumerate() {
        let class = window
            .class
            .as_deref()
            .or(window.instance.as_deref())
            .unwrap_or("-");
        println!(
            "{:>2}. ws={:<10} con={:<14} flags={}{} 0x{:08x} {:>4}x{:<4} {:+5}{:+5} depth={} visual=0x{:08x} class={} name={}",
            index + 1,
            window.workspace,
            window
                .i3_con_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            if window.focused { "F" } else { "-" },
            if window.urgent { "U" } else { "-" },
            window.id,
            window.geometry.width,
            window.geometry.height,
            window.geometry.x,
            window.geometry.y,
            window.geometry.depth,
            window.geometry.visual,
            class,
            window.name
        );
    }
}

pub fn focus_window(
    ctx: &X11Context,
    atoms: &Atoms,
    window: &WindowInfo,
    debug: bool,
) -> Result<()> {
    let mut i3_focus_succeeded = false;
    if let Some(con_id) = window.i3_con_id {
        match crate::i3::focus_con(con_id, &window.workspace, debug) {
            Ok(()) => {
                i3_focus_succeeded = true;
            }
            Err(error) if debug => {
                eprintln!(
                    "focus: i3 focus for workspace={} con_id={con_id} failed, falling back to EWMH: {error:#}",
                    window.workspace
                );
            }
            Err(_) => {}
        }
    }

    let id = window.id;
    if debug {
        eprintln!("focus: requesting _NET_ACTIVE_WINDOW for 0x{id:08x}");
    }
    if i3_focus_succeeded {
        thread::sleep(Duration::from_millis(40));
    }

    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window: id,
        type_: atoms.net_active_window,
        data: ClientMessageData::from([2, CURRENT_TIME, 0, 0, 0]),
    };

    ctx.conn
        .send_event(
            false,
            ctx.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .context("failed to send _NET_ACTIVE_WINDOW client message")?
        .check()
        .context("_NET_ACTIVE_WINDOW client message was rejected by X11")?;

    if let Ok(cookie) = ctx
        .conn
        .set_input_focus(InputFocus::POINTER_ROOT, id, CURRENT_TIME)
    {
        cookie.ignore_error();
    }
    if let Ok(cookie) = ctx
        .conn
        .configure_window(id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))
    {
        cookie.ignore_error();
    }
    ctx.conn.flush().context("failed to flush focus requests")?;
    Ok(())
}

fn read_window_list(ctx: &X11Context, property: u32) -> Result<Vec<Window>> {
    let reply = ctx
        .conn
        .get_property(false, ctx.root, property, AtomEnum::WINDOW, 0, 1024 * 1024)
        .context("failed to request EWMH window list")?
        .reply()
        .context("failed to read EWMH window list")?;

    Ok(reply
        .value32()
        .map(|values| values.collect())
        .unwrap_or_default())
}

fn query_root_children(ctx: &X11Context) -> Result<Vec<Window>> {
    Ok(ctx
        .conn
        .query_tree(ctx.root)
        .context("failed to query root window tree")?
        .reply()
        .context("failed to receive root window tree")?
        .children)
}

fn inspect_window(
    ctx: &X11Context,
    atoms: &Atoms,
    id: Window,
    debug: bool,
) -> Result<Option<WindowInfo>> {
    let attrs = match ctx.conn.get_window_attributes(id) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply,
            Err(err) => {
                if debug {
                    eprintln!("windows: skipping 0x{id:08x}; attributes failed: {err}");
                }
                return Ok(None);
            }
        },
        Err(err) => {
            if debug {
                eprintln!("windows: skipping 0x{id:08x}; attributes request failed: {err}");
            }
            return Ok(None);
        }
    };

    if attrs.override_redirect
        || attrs.map_state != MapState::VIEWABLE
        || attrs.class != WindowClass::INPUT_OUTPUT
    {
        return Ok(None);
    }

    let geometry = match ctx.conn.get_geometry(id) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply,
            Err(err) => {
                if debug {
                    eprintln!("windows: skipping 0x{id:08x}; geometry failed: {err}");
                }
                return Ok(None);
            }
        },
        Err(err) => {
            if debug {
                eprintln!("windows: skipping 0x{id:08x}; geometry request failed: {err}");
            }
            return Ok(None);
        }
    };

    if geometry.width == 0 || geometry.height == 0 {
        return Ok(None);
    }

    let (x, y) = ctx
        .conn
        .translate_coordinates(id, ctx.root, 0, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .filter(|reply| reply.same_screen)
        .map(|reply| (reply.dst_x, reply.dst_y))
        .unwrap_or((geometry.x, geometry.y));

    let name = read_window_name(ctx, atoms, id).unwrap_or_else(|| format!("0x{id:08x}"));
    let (instance, class) = read_wm_class(ctx, id);

    Ok(Some(WindowInfo {
        id,
        name,
        instance,
        class,
        workspace: "current".to_string(),
        workspace_num: None,
        i3_con_id: None,
        tree_order: 0,
        urgent: false,
        focused: false,
        geometry: WindowGeometry {
            x,
            y,
            width: geometry.width,
            height: geometry.height,
            depth: geometry.depth,
            visual: attrs.visual,
        },
    }))
}

fn read_window_name(ctx: &X11Context, atoms: &Atoms, window: Window) -> Option<String> {
    read_string_property(ctx, window, atoms.net_wm_name, atoms.utf8_string).or_else(|| {
        read_string_property(
            ctx,
            window,
            AtomEnum::WM_NAME.into(),
            AtomEnum::STRING.into(),
        )
    })
}

pub(crate) fn read_wm_class(ctx: &X11Context, window: Window) -> (Option<String>, Option<String>) {
    let Some(reply) = get_property(
        ctx,
        window,
        AtomEnum::WM_CLASS.into(),
        AtomEnum::STRING.into(),
    ) else {
        return (None, None);
    };
    if reply.format != 8 || reply.value.is_empty() {
        return (None, None);
    }

    let parts = reply
        .value
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    (parts.first().cloned(), parts.get(1).cloned())
}

fn read_string_property(
    ctx: &X11Context,
    window: Window,
    property: u32,
    type_: u32,
) -> Option<String> {
    let reply = get_property(ctx, window, property, type_)?;
    if reply.format != 8 || reply.value.is_empty() {
        return None;
    }

    let bytes = reply
        .value
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or(&reply.value);
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn get_property(
    ctx: &X11Context,
    window: Window,
    property: u32,
    type_: u32,
) -> Option<GetPropertyReply> {
    ctx.conn
        .get_property(false, window, property, type_, 0, 1024)
        .ok()?
        .reply()
        .ok()
        .filter(|reply| reply.type_ != NONE)
}
