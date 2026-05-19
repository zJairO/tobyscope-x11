use anyhow::{Context, Result, bail};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::{composite, render, xproto::*};
use x11rb::rust_connection::RustConnection;

pub struct X11Context {
    pub conn: RustConnection,
    pub root: Window,
    pub root_visual: Visualid,
    pub root_depth: u8,
    pub width: u16,
    pub height: u16,
}

impl X11Context {
    pub fn connect(debug: bool) -> Result<Self> {
        if std::env::var_os("DISPLAY").is_none() {
            bail!(
                "DISPLAY is not set. tobyscope-x11 only works inside a Linux X11 session, not pure Wayland/headless mode."
            );
        }

        let (conn, screen_num) =
            RustConnection::connect(None).context("failed to connect to the X11 server")?;
        let screen = &conn.setup().roots[screen_num];

        if debug {
            eprintln!(
                "x11: connected to screen {screen_num}, root=0x{:08x}, size={}x{}",
                screen.root, screen.width_in_pixels, screen.height_in_pixels
            );
        }

        Ok(Self {
            root: screen.root,
            root_visual: screen.root_visual,
            root_depth: screen.root_depth,
            width: screen.width_in_pixels,
            height: screen.height_in_pixels,
            conn,
        })
    }

    pub fn require_extensions(&self, debug: bool) -> Result<()> {
        require_extension(&self.conn, composite::X11_EXTENSION_NAME)?;
        require_extension(&self.conn, render::X11_EXTENSION_NAME)?;

        let composite_version = composite::query_version(&self.conn, 0, 4)
            .context("failed to query XComposite version")?
            .reply()
            .context("XComposite version query failed")?;
        let render_version = render::query_version(&self.conn, 0, 11)
            .context("failed to query XRender version")?
            .reply()
            .context("XRender version query failed")?;

        if debug {
            eprintln!(
                "x11: using Composite {}.{} and Render {}.{}",
                composite_version.major_version,
                composite_version.minor_version,
                render_version.major_version,
                render_version.minor_version
            );
        }

        Ok(())
    }

    pub fn root_size(&self) -> Result<(u16, u16)> {
        let geometry = self
            .conn
            .get_geometry(self.root)
            .context("failed to request root window geometry")?
            .reply()
            .context("failed to read root window geometry")?;
        Ok((geometry.width, geometry.height))
    }
}

fn require_extension(conn: &RustConnection, name: &'static str) -> Result<()> {
    if conn
        .extension_information(name)
        .with_context(|| format!("failed to inspect X11 extension `{name}`"))?
        .is_none()
    {
        bail!("required X11 extension `{name}` is not available on this display");
    }
    Ok(())
}
