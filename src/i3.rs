use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use i3ipc::I3Connection;
use i3ipc::reply::{Node, NodeType, WindowProperty, Workspace};
use x11rb::protocol::xproto::{ConnectionExt, MapState, Window, WindowClass};

use crate::windows::{WindowGeometry, WindowInfo};
use crate::x11::X11Context;

#[derive(Debug, Clone)]
struct WorkspaceInfo {
    name: String,
    num: Option<i32>,
}

#[derive(Debug, Clone)]
struct I3WindowCandidate<'a> {
    node: &'a Node,
    workspace: &'a WorkspaceInfo,
    tree_order: usize,
}

pub fn discover_windows(ctx: &X11Context, debug: bool) -> Result<Vec<WindowInfo>> {
    let mut connection = I3Connection::connect().context("failed to connect to i3 IPC")?;
    let workspaces = connection
        .get_workspaces()
        .context("failed to read i3 workspaces")?;
    let tree = connection.get_tree().context("failed to read i3 tree")?;
    let workspace_map = workspace_map(&workspaces.workspaces);
    let mut candidates = Vec::new();
    let mut order = 0usize;
    collect_candidates(&tree, &workspace_map, None, &mut order, &mut candidates);

    let mut windows = Vec::new();
    for candidate in candidates {
        match inspect_candidate(ctx, candidate, debug) {
            Ok(Some(window)) => windows.push(window),
            Ok(None) => {}
            Err(error) if debug => {
                eprintln!("i3: skipped candidate: {error:#}");
            }
            Err(_) => {}
        }
    }

    windows.sort_by(|a, b| {
        workspace_sort_key(a)
            .cmp(&workspace_sort_key(b))
            .then_with(|| a.tree_order.cmp(&b.tree_order))
            .then_with(|| a.id.cmp(&b.id))
    });

    if debug {
        eprintln!("i3: {} windows detected across workspaces", windows.len());
    }

    Ok(windows)
}

pub fn current_workspace() -> Result<Option<String>> {
    let mut connection = I3Connection::connect().context("failed to connect to i3 IPC")?;
    let workspaces = connection
        .get_workspaces()
        .context("failed to read i3 workspaces")?;
    Ok(workspaces
        .workspaces
        .into_iter()
        .find(|workspace| workspace.focused)
        .map(|workspace| workspace.name))
}

pub fn switch_workspace(name: &str, debug: bool) -> Result<()> {
    let command = format!("workspace \"{}\"", escape_i3_string(name));
    if debug {
        eprintln!("i3: {command}");
    }
    run_checked_command(&command)
}

pub fn focus_con(con_id: i64, workspace: &str, debug: bool) -> Result<()> {
    let workspace_command = format!("workspace \"{}\"", escape_i3_string(workspace));
    if debug {
        eprintln!("i3: {workspace_command}");
    }
    run_checked_command(&workspace_command)?;
    thread::sleep(Duration::from_millis(60));

    let command = format!("[con_id={con_id}] focus");
    if debug {
        eprintln!("i3: {command}");
    }
    run_checked_command(&command)?;
    thread::sleep(Duration::from_millis(40));
    if debug {
        match current_workspace() {
            Ok(Some(current)) => {
                eprintln!("i3: focused workspace after focus command is {current}")
            }
            Ok(None) => eprintln!("i3: no focused workspace reported after focus command"),
            Err(error) => eprintln!("i3: failed to verify focused workspace: {error:#}"),
        }
    }
    Ok(())
}

pub fn close_con(con_id: i64, debug: bool) -> Result<()> {
    let command = format!("[con_id={con_id}] close");
    if debug {
        eprintln!("i3: {command}");
    }
    run_checked_command(&command)
}

fn run_checked_command(command: &str) -> Result<()> {
    let mut connection = I3Connection::connect().context("failed to connect to i3 IPC")?;
    let reply = connection
        .run_command(command)
        .with_context(|| format!("failed to run i3 command `{command}`"))?;
    if let Some(outcome) = reply.outcomes.iter().find(|outcome| !outcome.success) {
        bail!(
            "i3 rejected `{}`: {}",
            command,
            outcome.error.as_deref().unwrap_or("unknown error")
        );
    }
    Ok(())
}

fn workspace_map(workspaces: &[Workspace]) -> HashMap<String, WorkspaceInfo> {
    workspaces
        .iter()
        .map(|workspace| {
            (
                workspace.name.clone(),
                WorkspaceInfo {
                    name: workspace.name.clone(),
                    num: (workspace.num >= 0).then_some(workspace.num),
                },
            )
        })
        .collect()
}

fn collect_candidates<'a>(
    node: &'a Node,
    workspace_map: &'a HashMap<String, WorkspaceInfo>,
    workspace: Option<&'a WorkspaceInfo>,
    order: &mut usize,
    out: &mut Vec<I3WindowCandidate<'a>>,
) {
    let mut active_workspace = workspace;
    if node.nodetype == NodeType::Workspace {
        active_workspace = node
            .name
            .as_deref()
            .filter(|name| !name.starts_with("__i3"))
            .and_then(|name| workspace_map.get(name));
    }

    if let (Some(workspace), Some(_window)) = (active_workspace, node.window) {
        out.push(I3WindowCandidate {
            node,
            workspace,
            tree_order: *order,
        });
        *order += 1;
    }

    for child in &node.nodes {
        collect_candidates(child, workspace_map, active_workspace, order, out);
    }
    for child in &node.floating_nodes {
        collect_candidates(child, workspace_map, active_workspace, order, out);
    }
}

fn inspect_candidate(
    ctx: &X11Context,
    candidate: I3WindowCandidate<'_>,
    debug: bool,
) -> Result<Option<WindowInfo>> {
    let window_id = candidate
        .node
        .window
        .context("i3 candidate did not include an X11 window id")? as Window;
    let attrs = match ctx.conn.get_window_attributes(window_id) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply,
            Err(error) => {
                if debug {
                    eprintln!("i3: 0x{window_id:08x} attributes failed: {error}");
                }
                return Ok(None);
            }
        },
        Err(error) => {
            if debug {
                eprintln!("i3: 0x{window_id:08x} attributes request failed: {error}");
            }
            return Ok(None);
        }
    };

    if attrs.override_redirect || attrs.class != WindowClass::INPUT_OUTPUT {
        return Ok(None);
    }

    let geometry = match ctx.conn.get_geometry(window_id) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply,
            Err(error) => {
                if debug {
                    eprintln!("i3: 0x{window_id:08x} geometry failed: {error}");
                }
                return Ok(None);
            }
        },
        Err(error) => {
            if debug {
                eprintln!("i3: 0x{window_id:08x} geometry request failed: {error}");
            }
            return Ok(None);
        }
    };

    if geometry.width == 0 || geometry.height == 0 {
        return Ok(None);
    }

    let (x, y) = if attrs.map_state == MapState::VIEWABLE {
        ctx.conn
            .translate_coordinates(window_id, ctx.root, 0, 0)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .filter(|reply| reply.same_screen)
            .map(|reply| (reply.dst_x, reply.dst_y))
            .unwrap_or((geometry.x, geometry.y))
    } else {
        let (node_x, node_y, _, _) = candidate.node.rect;
        (node_x as i16, node_y as i16)
    };

    let props = candidate.node.window_properties.as_ref();
    let name = props
        .and_then(|props| props.get(&WindowProperty::Title))
        .cloned()
        .or_else(|| candidate.node.name.clone())
        .unwrap_or_else(|| format!("0x{window_id:08x}"));
    let (x11_instance, x11_class) = crate::windows::read_wm_class(ctx, window_id);
    let instance = props
        .and_then(|props| props.get(&WindowProperty::Instance))
        .cloned()
        .or(x11_instance);
    let class = props
        .and_then(|props| props.get(&WindowProperty::Class))
        .cloned()
        .or(x11_class);

    Ok(Some(WindowInfo {
        id: window_id,
        name,
        instance,
        class,
        workspace: candidate.workspace.name.clone(),
        workspace_num: candidate.workspace.num,
        i3_con_id: Some(candidate.node.id),
        tree_order: candidate.tree_order,
        urgent: candidate.node.urgent,
        focused: candidate.node.focused,
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

fn workspace_sort_key(window: &WindowInfo) -> (i32, String) {
    (
        window.workspace_num.unwrap_or(i32::MAX),
        window.workspace.clone(),
    )
}

fn escape_i3_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn refresh_geometry(ctx: &X11Context, window: &WindowInfo) -> Result<WindowGeometry> {
    let attrs = ctx
        .conn
        .get_window_attributes(window.id)
        .context("failed to request window attributes")?
        .reply()
        .context("failed to read window attributes")?;
    let geometry = ctx
        .conn
        .get_geometry(window.id)
        .context("failed to request window geometry")?
        .reply()
        .context("failed to read window geometry")?;

    Ok(WindowGeometry {
        x: geometry.x,
        y: geometry.y,
        width: geometry.width,
        height: geometry.height,
        depth: geometry.depth,
        visual: attrs.visual,
    })
}
