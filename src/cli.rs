use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub struct Args {
    pub list_windows: bool,
    pub debug: bool,
    pub config_path: Option<PathBuf>,
}

impl Args {
    pub fn parse() -> Result<Self> {
        let mut args = Self {
            list_windows: false,
            debug: false,
            config_path: None,
        };

        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--list-windows" => args.list_windows = true,
                "--debug" => args.debug = true,
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
    "Usage: tobyscope-x11 [--list-windows] [--debug] [--config PATH]\n\n\
     Options:\n\
       --list-windows  Print detected visible X11 client windows and exit.\n\
       --debug         Print extra diagnostics to stderr.\n\
       --config PATH   Load settings from a TOML config file.\n\
       -h, --help      Show this help.\n"
}
