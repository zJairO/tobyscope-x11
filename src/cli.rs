use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub struct Args {
    pub list_windows: bool,
    pub debug: bool,
    pub config_path: Option<PathBuf>,
    pub mode: RunMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Auto,
    Standalone,
    Daemon,
    DaemonStatus,
    DaemonQuit,
}

impl Args {
    pub fn parse() -> Result<Self> {
        let mut args = Self {
            list_windows: false,
            debug: false,
            config_path: None,
            mode: RunMode::Auto,
        };

        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--list-windows" => args.list_windows = true,
                "--debug" => args.debug = true,
                "--standalone" => set_mode(&mut args.mode, RunMode::Standalone)?,
                "--daemon" => set_mode(&mut args.mode, RunMode::Daemon)?,
                "--daemon-status" => set_mode(&mut args.mode, RunMode::DaemonStatus)?,
                "--daemon-quit" => set_mode(&mut args.mode, RunMode::DaemonQuit)?,
                "--config" => {
                    let Some(path) = iter.next() else {
                        bail!("--config requires a path\n\n{}", usage());
                    };
                    args.config_path = Some(PathBuf::from(path));
                }
                "-h" | "--help" => {
                    print!("{}", usage());
                    std::process::exit(0);
                }
                _ if arg.starts_with("--config=") => {
                    let path = arg.trim_start_matches("--config=");
                    if path.is_empty() {
                        bail!("--config requires a path\n\n{}", usage());
                    }
                    args.config_path = Some(PathBuf::from(path));
                }
                _ => bail!("unknown argument `{arg}`\n\n{}", usage()),
            }
        }

        Ok(args)
    }
}

fn usage() -> &'static str {
    "Usage: tobyscope-x11 [--standalone] [--daemon] [--daemon-status] [--daemon-quit] [--list-windows] [--debug] [--config PATH]\n\n\
     Options:\n\
       --standalone    Open the overview directly instead of using the daemon.\n\
       --daemon        Run the resident daemon that keeps cache and rendering state warm.\n\
       --daemon-status Query the resident daemon status and exit.\n\
       --daemon-quit   Ask the resident daemon to quit and exit.\n\
       --list-windows  Print detected X11/i3 client windows and exit.\n\
       --debug         Print extra diagnostics to stderr.\n\
       --config PATH   Load settings from a TOML config file.\n\
       -h, --help      Show this help.\n"
}

fn set_mode(target: &mut RunMode, next: RunMode) -> Result<()> {
    if *target != RunMode::Auto && *target != next {
        bail!(
            "daemon/standalone mode flags are mutually exclusive\n\n{}",
            usage()
        );
    }
    *target = next;
    Ok(())
}
