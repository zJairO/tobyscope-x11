use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, Keycode, Keysym};

use crate::x11::X11Context;

const XK_ESCAPE: Keysym = 0xff1b;
const XK_RETURN: Keysym = 0xff0d;
const XK_LEFT: Keysym = 0xff51;
const XK_UP: Keysym = 0xff52;
const XK_RIGHT: Keysym = 0xff53;
const XK_DOWN: Keysym = 0xff54;
const XK_H: Keysym = b'h' as Keysym;
const XK_J: Keysym = b'j' as Keysym;
const XK_K: Keysym = b'k' as Keysym;
const XK_L: Keysym = b'l' as Keysym;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Close,
    Confirm,
    Left,
    Right,
    Up,
    Down,
}

pub struct KeyMap {
    min_keycode: Keycode,
    keysyms_per_keycode: usize,
    keysyms: Vec<Keysym>,
}

impl KeyMap {
    pub fn load(ctx: &X11Context) -> Result<Self> {
        let setup = ctx.conn.setup();
        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;
        let count = max_keycode.saturating_sub(min_keycode).saturating_add(1);
        let reply = ctx
            .conn
            .get_keyboard_mapping(min_keycode, count)
            .context("failed to request keyboard mapping")?
            .reply()
            .context("failed to read keyboard mapping")?;

        Ok(Self {
            min_keycode,
            keysyms_per_keycode: reply.keysyms_per_keycode as usize,
            keysyms: reply.keysyms,
        })
    }

    pub fn action_for_keycode(&self, keycode: Keycode) -> Option<KeyAction> {
        let keysyms = self.keysyms_for_keycode(keycode)?;
        if keysyms.contains(&XK_ESCAPE) {
            return Some(KeyAction::Close);
        }
        if keysyms.contains(&XK_RETURN) {
            return Some(KeyAction::Confirm);
        }
        if keysyms.contains(&XK_LEFT) || keysyms.contains(&XK_H) {
            return Some(KeyAction::Left);
        }
        if keysyms.contains(&XK_RIGHT) || keysyms.contains(&XK_L) {
            return Some(KeyAction::Right);
        }
        if keysyms.contains(&XK_UP) || keysyms.contains(&XK_K) {
            return Some(KeyAction::Up);
        }
        if keysyms.contains(&XK_DOWN) || keysyms.contains(&XK_J) {
            return Some(KeyAction::Down);
        }
        None
    }

    fn keysyms_for_keycode(&self, keycode: Keycode) -> Option<&[Keysym]> {
        if keycode < self.min_keycode {
            return None;
        }
        let offset = (keycode - self.min_keycode) as usize;
        let start = offset.checked_mul(self.keysyms_per_keycode)?;
        let end = start.checked_add(self.keysyms_per_keycode)?;
        self.keysyms.get(start..end)
    }
}
