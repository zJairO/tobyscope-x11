use anyhow::{Context, Result};
use x11rb::protocol::xproto::{Atom, ConnectionExt};
use x11rb::rust_connection::RustConnection;

#[derive(Debug, Clone)]
pub struct Atoms {
    pub net_client_list: Atom,
    pub net_client_list_stacking: Atom,
    pub net_wm_name: Atom,
    pub net_active_window: Atom,
    pub utf8_string: Atom,
}

impl Atoms {
    pub fn intern(conn: &RustConnection) -> Result<Self> {
        Ok(Self {
            net_client_list: intern(conn, b"_NET_CLIENT_LIST")?,
            net_client_list_stacking: intern(conn, b"_NET_CLIENT_LIST_STACKING")?,
            net_wm_name: intern(conn, b"_NET_WM_NAME")?,
            net_active_window: intern(conn, b"_NET_ACTIVE_WINDOW")?,
            utf8_string: intern(conn, b"UTF8_STRING")?,
        })
    }
}

fn intern(conn: &RustConnection, name: &[u8]) -> Result<Atom> {
    Ok(conn
        .intern_atom(false, name)
        .with_context(|| format!("failed to intern atom {}", String::from_utf8_lossy(name)))?
        .reply()
        .with_context(|| format!("failed to receive atom {}", String::from_utf8_lossy(name)))?
        .atom)
}
