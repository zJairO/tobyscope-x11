use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use i3ipc::{I3EventListener, Subscription};

use crate::app::{AppTick, OverviewApp};
use crate::atoms::Atoms;
use crate::cli::Args;
use crate::config::{self, AppConfig};
use crate::{windows, x11};

const SOCKET_DIR_NAME: &str = "tobyscope-x11";
const SOCKET_FILE_NAME: &str = "socket";
const CLIENT_TIMEOUT: Duration = Duration::from_millis(700);
const SHOW_CLIENT_TIMEOUT: Duration = Duration::from_millis(120);
const MAIN_TICK: Duration = Duration::from_millis(16);

#[derive(Debug)]
enum DaemonMessage {
    Show(u128),
    I3Changed,
    Quit,
}

#[derive(Debug, Clone)]
struct SharedStatus {
    windows: usize,
    visible: bool,
}

impl SharedStatus {
    fn text(&self) -> String {
        format!(
            "windows={} visible={} cache=warm",
            self.windows, self.visible
        )
    }
}

pub fn run(args: &Args) -> Result<()> {
    let socket = socket_path();
    prepare_socket(&socket)?;

    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(SharedStatus {
        windows: 0,
        visible: false,
    }));

    let mut state = DaemonState::new(args.config_path.clone(), args.debug, status.clone())?;
    if args.debug {
        eprintln!(
            "daemon: starting with {} windows, socket={}",
            state.window_count(),
            socket.display()
        );
    }

    let _server = spawn_server(socket.clone(), tx.clone(), status, args.debug)?;
    spawn_i3_watcher(tx.clone(), args.debug);

    let result = state.run(rx);
    let _ = fs::remove_file(&socket);
    state.cleanup();
    result
}

pub fn send_show_command(debug: bool) -> Result<()> {
    let socket = socket_path();
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("daemon is not reachable at {}", socket.display()))?;
    stream
        .set_read_timeout(Some(SHOW_CLIENT_TIMEOUT))
        .context("failed to set daemon show read timeout")?;
    stream
        .set_write_timeout(Some(SHOW_CLIENT_TIMEOUT))
        .context("failed to set daemon show write timeout")?;

    let command = format!("show {}\n", unix_millis());
    if let Err(error) = stream.write_all(command.as_bytes()) {
        if debug {
            eprintln!("daemon: connected but could not send show command: {error}");
        }
        return Ok(());
    }

    let mut buffer = [0u8; 512];
    let count = match stream.read(&mut buffer) {
        Ok(count) => count,
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            if debug {
                eprintln!("daemon: connected but busy; assuming overview is already open");
            }
            return Ok(());
        }
        Err(error) => return Err(error).context("failed to read show daemon response"),
    };

    let response = String::from_utf8_lossy(&buffer[..count]).trim().to_string();
    if !response.starts_with("ok") {
        bail!("daemon rejected `show`: {response}");
    }
    if debug {
        eprintln!("daemon: {response}");
    }
    Ok(())
}

pub fn send_command(command: &str, debug: bool, print_response: bool) -> Result<()> {
    let socket = socket_path();
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("daemon is not reachable at {}", socket.display()))?;
    stream
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .context("failed to set daemon client read timeout")?;
    stream
        .set_write_timeout(Some(CLIENT_TIMEOUT))
        .context("failed to set daemon client write timeout")?;
    stream
        .write_all(format!("{command}\n").as_bytes())
        .with_context(|| format!("failed to send `{command}` to daemon"))?;

    let mut buffer = [0u8; 512];
    let count = stream
        .read(&mut buffer)
        .with_context(|| format!("failed to read `{command}` daemon response"))?;
    let response = String::from_utf8_lossy(&buffer[..count]).trim().to_string();
    if print_response {
        println!("{response}");
    }
    if !response.starts_with("ok") {
        bail!("daemon rejected `{command}`: {response}");
    }
    if debug && !print_response {
        eprintln!("daemon: {response}");
    }
    Ok(())
}

fn spawn_server(
    socket: PathBuf,
    tx: Sender<DaemonMessage>,
    status: Arc<Mutex<SharedStatus>>,
    debug: bool,
) -> Result<thread::JoinHandle<()>> {
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("failed to bind daemon socket {}", socket.display()))?;
    let handle = thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut stream) = client else {
                continue;
            };
            let command = match read_command(&mut stream) {
                Ok(command) => command,
                Err(error) => {
                    let _ = write_response(&mut stream, &format!("error {error:#}\n"));
                    continue;
                }
            };
            let mut parts = command.split_whitespace();
            match parts.next().unwrap_or("") {
                "show" => {
                    let requested_at = parts.next().and_then(|value| value.parse().ok());
                    let _ = tx.send(DaemonMessage::Show(
                        requested_at.unwrap_or_else(unix_millis),
                    ));
                    let _ = write_response(&mut stream, "ok\n");
                }
                "status" => {
                    let text = status
                        .lock()
                        .map(|status| status.text())
                        .unwrap_or_else(|_| "status=poisoned".to_string());
                    let _ = write_response(&mut stream, &format!("ok {text}\n"));
                }
                "quit" => {
                    let _ = tx.send(DaemonMessage::Quit);
                    let _ = write_response(&mut stream, "ok quitting\n");
                    break;
                }
                "" => {
                    let _ = write_response(&mut stream, "error empty command\n");
                }
                other => {
                    let _ =
                        write_response(&mut stream, &format!("error unknown command `{other}`\n"));
                }
            }
        }
        if debug {
            eprintln!("daemon: socket server stopped");
        }
    });
    Ok(handle)
}

fn spawn_i3_watcher(tx: Sender<DaemonMessage>, debug: bool) {
    thread::spawn(move || {
        let mut listener = match I3EventListener::connect() {
            Ok(listener) => listener,
            Err(error) => {
                if debug {
                    eprintln!("daemon: i3 event listener unavailable: {error}");
                }
                return;
            }
        };
        if let Err(error) = listener.subscribe(&[Subscription::Workspace, Subscription::Window]) {
            if debug {
                eprintln!("daemon: failed to subscribe to i3 events: {error}");
            }
            return;
        }

        for event in listener.listen() {
            match event {
                Ok(_) => {
                    if tx.send(DaemonMessage::I3Changed).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if debug {
                        eprintln!("daemon: i3 event listener stopped: {error}");
                    }
                    break;
                }
            }
        }
    });
}

fn prepare_socket(socket: &Path) -> Result<()> {
    let dir = socket
        .parent()
        .context("daemon socket path did not have a parent directory")?;
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create daemon socket dir {}", dir.display()))?;

    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            bail!(
                "tobyscope-x11 daemon is already running at {}",
                socket.display()
            );
        }
        fs::remove_file(socket).with_context(|| {
            format!("failed to remove stale daemon socket {}", socket.display())
        })?;
    }
    Ok(())
}

fn socket_path() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime)
            .join(SOCKET_DIR_NAME)
            .join(SOCKET_FILE_NAME);
    }

    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    std::env::temp_dir()
        .join(format!("{SOCKET_DIR_NAME}-{user}"))
        .join(SOCKET_FILE_NAME)
}

fn read_command(stream: &mut UnixStream) -> Result<String> {
    let mut buffer = [0u8; 128];
    let count = stream
        .read(&mut buffer)
        .context("failed to read daemon command")?;
    Ok(String::from_utf8_lossy(&buffer[..count]).trim().to_string())
}

fn write_response(stream: &mut UnixStream, response: &str) -> Result<()> {
    stream
        .write_all(response.as_bytes())
        .context("failed to write daemon response")
}

struct DaemonState {
    config_path: Option<PathBuf>,
    debug: bool,
    app: Option<OverviewApp>,
    model_key: Vec<String>,
    config_key: String,
    config: AppConfig,
    dirty: bool,
    visible: bool,
    last_refresh: Instant,
    last_model_check: Instant,
    last_background_refresh: Instant,
    last_closed_ms: u128,
    status: Arc<Mutex<SharedStatus>>,
}

impl DaemonState {
    fn new(
        config_path: Option<PathBuf>,
        debug: bool,
        status: Arc<Mutex<SharedStatus>>,
    ) -> Result<Self> {
        let snapshot = build_snapshot(config_path.as_deref(), debug)?;
        let state = Self {
            config_path,
            debug,
            app: snapshot.app,
            model_key: snapshot.model_key,
            config_key: snapshot.config_key,
            config: snapshot.config,
            dirty: false,
            visible: false,
            last_refresh: Instant::now(),
            last_model_check: Instant::now(),
            last_background_refresh: Instant::now(),
            last_closed_ms: 0,
            status,
        };
        state.update_status();
        Ok(state)
    }

    fn run(&mut self, rx: Receiver<DaemonMessage>) -> Result<()> {
        loop {
            match rx.recv_timeout(MAIN_TICK) {
                Ok(message) => {
                    if self.handle_message(message)? {
                        return Ok(());
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }

            while let Ok(message) = rx.try_recv() {
                if self.handle_message(message)? {
                    return Ok(());
                }
            }

            self.tick()?;
        }
    }

    fn handle_message(&mut self, message: DaemonMessage) -> Result<bool> {
        match message {
            DaemonMessage::Show(requested_at) => {
                self.show(requested_at)?;
                Ok(false)
            }
            DaemonMessage::I3Changed => {
                self.dirty = true;
                Ok(false)
            }
            DaemonMessage::Quit => Ok(true),
        }
    }

    fn show(&mut self, requested_at: u128) -> Result<()> {
        if self.should_skip_show(requested_at) {
            if self.debug {
                eprintln!("daemon: skipped stale show request");
            }
            return Ok(());
        }
        if self.visible {
            if self.debug {
                eprintln!("daemon: show ignored because overlay is already visible");
            }
            return Ok(());
        }

        if self.app.is_none() {
            self.rebuild()?;
        }
        let Some(app) = self.app.as_mut() else {
            if self.debug {
                eprintln!("daemon: no windows to show");
            }
            return Ok(());
        };
        app.show_prepared()?;
        self.visible = true;
        self.update_status();
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        if self.visible {
            if let Some(app) = self.app.as_mut() {
                match app.tick_nonblocking()? {
                    AppTick::Idle => {}
                    AppTick::Closed | AppTick::Focused => {
                        self.visible = false;
                        self.last_closed_ms = unix_millis();
                        self.update_status();
                    }
                }
            }
            return Ok(());
        }

        if self.refresh_hidden_state()? {
            return Ok(());
        }

        let interval = Duration::from_millis(self.config.daemon.idle_refresh_ms);
        if let Some(app) = self.app.as_mut() {
            if self.config.daemon.background_current_workspace_refresh
                && self.last_background_refresh.elapsed() >= interval
            {
                app.start_background_capture();
                self.last_background_refresh = Instant::now();
            }
            app.tick_background()?;
        }

        Ok(())
    }

    fn should_skip_show(&self, requested_at: u128) -> bool {
        requested_at <= self.last_closed_ms
            || unix_millis().saturating_sub(requested_at)
                > u128::from(self.config.daemon.stale_show_ms)
    }

    fn refresh_hidden_state(&mut self) -> Result<bool> {
        let interval = Duration::from_millis(self.config.daemon.idle_refresh_ms);
        let should_check_config = self.last_refresh.elapsed() >= interval;
        let should_check_model = self.dirty && self.last_model_check.elapsed() >= interval;
        if !should_check_config && !should_check_model {
            return Ok(false);
        }

        if should_check_config {
            self.last_refresh = Instant::now();
        }
        if should_check_model {
            self.last_model_check = Instant::now();
        }

        let config_key = if should_check_config {
            Some(load_config_key(self.config_path.as_deref())?)
        } else {
            None
        };
        let model_key = if should_check_model {
            Some(discover_model_key(self.debug)?)
        } else {
            None
        };

        let config_changed = config_key
            .as_ref()
            .is_some_and(|config_key| config_key != &self.config_key);
        let model_changed = model_key
            .as_ref()
            .is_some_and(|model_key| model_key != &self.model_key);

        if config_changed || model_changed {
            if self.debug {
                eprintln!(
                    "daemon: refreshing prepared state config_changed={} model_changed={}",
                    config_changed, model_changed
                );
            }
            self.rebuild()?;
            return Ok(true);
        }

        if should_check_model {
            self.dirty = false;
        }
        if let Some(config_key) = config_key {
            self.config_key = config_key;
        }
        Ok(false)
    }

    fn rebuild(&mut self) -> Result<()> {
        let snapshot = build_snapshot(self.config_path.as_deref(), self.debug)?;
        let old = self.app.take();
        self.app = snapshot.app;
        self.model_key = snapshot.model_key;
        self.config_key = snapshot.config_key;
        self.config = snapshot.config;
        self.dirty = false;
        self.visible = false;
        self.last_refresh = Instant::now();
        self.last_model_check = Instant::now();
        self.last_background_refresh = Instant::now();
        if let Some(mut app) = old {
            app.cleanup()?;
        }
        if self.debug {
            eprintln!("daemon: warmed {} windows", self.window_count());
        }
        self.update_status();
        Ok(())
    }

    fn window_count(&self) -> usize {
        self.app
            .as_ref()
            .map(OverviewApp::window_count)
            .unwrap_or(0)
    }

    fn update_status(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.windows = self.window_count();
            status.visible = self.visible;
        }
    }

    fn cleanup(&mut self) {
        if let Some(app) = self.app.as_mut()
            && let Err(error) = app.cleanup()
            && self.debug
        {
            eprintln!("daemon: cleanup failed: {error:#}");
        }
    }
}

struct BuildSnapshot {
    app: Option<OverviewApp>,
    model_key: Vec<String>,
    config_key: String,
    config: AppConfig,
}

fn build_snapshot(config_path: Option<&Path>, debug: bool) -> Result<BuildSnapshot> {
    let loaded = config::load(config_path, debug)?;
    let config_key = format!("{:?}", loaded.config);
    let ctx = x11::X11Context::connect(debug)?;
    ctx.require_extensions(debug)?;
    let atoms = Atoms::intern(&ctx.conn)?;
    let detected = windows::discover_windows(&ctx, &atoms, debug)?;
    let model_key = windows::model_key(&detected);
    if detected.is_empty() {
        if debug {
            eprintln!("daemon: no i3/X11 client windows found while warming cache");
        }
        return Ok(BuildSnapshot {
            app: None,
            model_key,
            config_key,
            config: loaded.config,
        });
    }

    let mut app = OverviewApp::new(ctx, atoms, detected, loaded.config.clone(), debug)?;
    app.prewarm()?;
    Ok(BuildSnapshot {
        app: Some(app),
        model_key,
        config_key,
        config: loaded.config,
    })
}

fn load_config_key(config_path: Option<&Path>) -> Result<String> {
    let loaded = config::load(config_path, false)?;
    Ok(format!("{:?}", loaded.config))
}

fn discover_model_key(debug: bool) -> Result<Vec<String>> {
    let ctx = x11::X11Context::connect(false)?;
    ctx.require_extensions(false)?;
    let atoms = Atoms::intern(&ctx.conn)?;
    let detected = windows::discover_windows(&ctx, &atoms, false)?;
    if debug {
        eprintln!("daemon: checked live model with {} windows", detected.len());
    }
    Ok(windows::model_key(&detected))
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
