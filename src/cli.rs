use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy)]
pub struct Args {
    pub list_windows: bool,
    pub debug: bool,
}

impl Args {
    pub fn parse() -> Result<Self> {
        let mut args = Self {
            list_windows: false,
            debug: false,
        };

        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--list-windows" => args.list_windows = true,
                "--debug" => args.debug = true,
                "-h" | "--help" => {
                    print!("{}", usage());
                    std::process::exit(0);
                }
                _ => bail!("unknown argument `{arg}`\n\n{}", usage()),
            }
        }

        Ok(args)
    }
}

fn usage() -> &'static str {
    "Usage: tobyscope-x11 [--list-windows] [--debug]\n\n\
     Options:\n\
       --list-windows  Print detected visible X11 client windows and exit.\n\
       --debug         Print extra diagnostics to stderr.\n\
       -h, --help      Show this help.\n"
}
