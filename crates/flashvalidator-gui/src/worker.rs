//! Runs a validation in the background and forwards its events to the UI.
//!
//! If this process may already open the drive, the run happens on a thread.
//! Otherwise an elevated helper process is started (see [`crate::helper`]),
//! which makes the OS show its password / consent prompt.

use crate::helper::{HelperArgs, HelperMsg};
use eframe::egui;
use flashvalidator_core::blockdev::Access;
use flashvalidator_core::device::DeviceInfo;
use flashvalidator_core::engine::{CancelToken, Config, Event, Validator};
use flashvalidator_core::privilege::{self, ElevatedProcess, Prompt};
use flashvalidator_core::report::Report;
use flashvalidator_core::rng::entropy_seed;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long to wait for the user to answer the permission prompt.
const AUTH_TIMEOUT: Duration = Duration::from_secs(300);

/// Message from the worker thread.
#[derive(Debug)]
pub enum Message {
    /// A human-readable status (e.g. waiting for permission).
    Status(String),
    /// A progress event from the engine.
    Event(Event),
    /// The run finished (or could not start).
    Done(Result<Box<Report>, String>),
}

/// A validation running in the background.
#[derive(Debug)]
pub struct Worker {
    rx: Receiver<Message>,
    cancel: CancelToken,
    link: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts validating `device`, requesting elevated rights if needed.
    pub fn spawn(device: DeviceInfo, cfg: Config, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::default();
        let link = Arc::new(Mutex::new(None));
        let (token, link2) = (cancel.clone(), link.clone());
        let thread = std::thread::Builder::new()
            .name("flashvalidator-worker".into())
            .spawn(move || {
                let send = |m: Message| {
                    let _ = tx.send(m);
                    ctx.request_repaint();
                };
                if privilege::can_open_raw(&device.path, cfg.mode.writes()) {
                    run_in_process(device, cfg, token, &send);
                } else {
                    run_elevated(device, cfg, token, link2, &send);
                }
            })
            .expect("spawn worker thread");
        Self { rx, cancel, link, thread: Some(thread) }
    }

    /// Asks the run to stop (original data is still restored).
    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Ok(mut guard) = self.link.lock() {
            if let Some(s) = guard.as_mut() {
                let _ = s.write_all(b"cancel\n");
            }
        }
    }

    /// Whether a stop was requested.
    pub fn is_cancelling(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Returns all messages received so far.
    pub fn drain(&self) -> Vec<Message> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Never abandon a run mid-region: stop it and wait so data is restored.
        self.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run_in_process(device: DeviceInfo, cfg: Config, token: CancelToken, send: &dyn Fn(Message)) {
    let access = if cfg.mode.writes() { Access::ReadWrite } else { Access::ReadOnly };
    let dev = match device.open(access) {
        Ok(d) => d,
        Err(e) => return send(Message::Done(Err(e.to_string()))),
    };
    let validator = Validator::new(dev, cfg).with_device_summary(device.summary()).with_cancel_token(token);
    let result = validator.run(&mut |e| send(Message::Event(e)));
    send(Message::Done(result.map(Box::new).map_err(|e| e.to_string())));
}

fn run_elevated(
    device: DeviceInfo,
    cfg: Config,
    token: CancelToken,
    link: Arc<Mutex<Option<TcpStream>>>,
    send: &dyn Fn(Message),
) {
    let fail = |msg: String| send(Message::Done(Err(msg)));
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => return fail(format!("could not start the helper channel: {e}")),
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => return fail(e.to_string()),
    };
    let secret = format!("{:016x}{:016x}", entropy_seed(), entropy_seed());
    let args = HelperArgs {
        connect: addr.to_string(),
        token: secret.clone(),
        device: device.path.clone(),
        mode: cfg.mode,
        calibrate: cfg.calibrate,
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail(e.to_string()),
    };

    send(Message::Status(
        if cfg!(windows) { "Waiting for administrator permission..." } else { "Waiting for authentication..." }.into(),
    ));
    let mut child = match privilege::spawn_elevated(&exe, &args.to_args(), Prompt::Graphical) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return fail("permission was not granted, so the drive cannot be accessed".into());
        }
        Err(e) => return fail(format!("could not request elevated rights: {e}")),
    };

    let stream = match accept_helper(&listener, &mut child, &token) {
        Ok(s) => s,
        Err(msg) => return fail(msg),
    };
    let mut lines = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    })
    .lines();

    // Authenticate the helper before trusting anything it says.
    match lines.next().and_then(Result::ok).and_then(|l| serde_json::from_str::<HelperMsg>(&l).ok()) {
        Some(HelperMsg::Hello { token: t }) if t == secret => {}
        _ => {
            child.kill();
            return fail("the elevated helper could not be verified".into());
        }
    }
    if let Ok(mut g) = link.lock() {
        *g = Some(stream);
        if token.is_cancelled() {
            if let Some(s) = g.as_mut() {
                let _ = s.write_all(b"cancel\n");
            }
        }
    }
    send(Message::Status(String::new()));

    let mut finished = false;
    for line in lines {
        let Ok(line) = line else { break };
        match serde_json::from_str::<HelperMsg>(&line) {
            Ok(HelperMsg::Event(e)) => send(Message::Event(e)),
            Ok(HelperMsg::Done(r)) => {
                send(Message::Done(r));
                finished = true;
                break;
            }
            Ok(HelperMsg::Hello { .. }) | Err(_) => {}
        }
    }
    if let Ok(mut g) = link.lock() {
        *g = None;
    }
    let code = child.wait().unwrap_or(-1);
    if !finished {
        fail(format!("the elevated helper stopped unexpectedly (exit code {code})"));
    }
}

/// Waits for the helper to connect, watching for a declined prompt.
fn accept_helper(
    listener: &TcpListener,
    child: &mut ElevatedProcess,
    token: &CancelToken,
) -> Result<TcpStream, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => {
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
        if let Ok(Some(code)) = child.try_wait() {
            return Err(if ElevatedProcess::was_denied(code) {
                "authentication was cancelled or failed, so the drive cannot be accessed".into()
            } else {
                format!("the elevated helper exited before starting (exit code {code})")
            });
        }
        // Until the helper connects it has not touched the drive, so it is
        // safe to stop it if the user cancels or never answers the prompt.
        if token.is_cancelled() || started.elapsed() > AUTH_TIMEOUT {
            child.kill();
            return Err("cancelled while waiting for permission".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
