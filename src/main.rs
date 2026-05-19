mod app;
mod atoms;
mod cache;
mod capture;
mod cli;
mod i3;
mod input;
mod layout;
mod pixels;
mod render;
mod windows;
mod x11;

use anyhow::Result;

fn main() -> Result<()> {
    let args = cli::Args::parse()?;
    let ctx = x11::X11Context::connect(args.debug)?;
    ctx.require_extensions(args.debug)?;
    let atoms = atoms::Atoms::intern(&ctx.conn)?;
    let windows = windows::discover_windows(&ctx, &atoms, args.debug)?;

    if args.list_windows {
        windows::print_window_list(&windows);
        return Ok(());
    }

    if windows.is_empty() {
        eprintln!("tobyscope-x11: no i3/X11 client windows found");
        return Ok(());
    }

    app::OverviewApp::new(ctx, atoms, windows, args.debug)?.run()
}
