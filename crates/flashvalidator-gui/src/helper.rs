//! Elevated helper process.
//!
//! The window always runs with normal user rights. When a test needs raw
//! drive access that the user does not have, the GUI starts a copy of its
//! own executable with elevated rights (polkit/pkexec on Linux, UAC on
//! Windows) in *helper mode*:
//!
//! ```text
//! flashvalidator-gui --helper --connect 127.0.0.1:PORT --token HEX --device PATH --mode MODE [--no-calibrate]
//! ```
//!
//! The helper connects back over loopback TCP and proves it is the process
//! the GUI started by echoing the random token. It then runs the validation
//! and streams one JSON [`HelperMsg`] per line. The GUI may send `cancel\n`.
//! If the connection drops (for example, the window was closed), the helper
//! cancels the run itself. Cancelling always restores the original data
//! before the helper exits.

use flashvalidator_core::blockdev::Access;
use flashvalidator_core::device;
use flashvalidator_core::engine::{CancelToken, Config, Event, Validator};
use flashvalidator_core::report::{Mode, Report};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::Duration;

/// First argument that selects helper mode.
pub const HELPER_FLAG: &str = "--helper";

/// One line of the helper protocol.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperMsg {
    /// Sent first: the token from the command line.
    Hello {
        /// Token proving the helper's identity.
        token: String,
    },
    /// A progress event.
    Event(Event),
    /// The final result.
    Done(Result<Box<Report>, String>),
}

/// Parameters passed on the helper command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    /// `127.0.0.1:PORT` to connect to.
    pub connect: String,
    /// Token to echo.
    pub token: String,
    /// Device path.
    pub device: String,
    /// Test mode.
    pub mode: Mode,
    /// Whether to run calibration.
    pub calibrate: bool,
}

impl HelperArgs {
    /// Command-line arguments (after the executable) that start a helper.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut v: Vec<OsString> = [
            HELPER_FLAG,
            "--connect",
            &self.connect,
            "--token",
            &self.token,
            "--device",
            &self.device,
            "--mode",
            self.mode.as_str(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        if !self.calibrate {
            v.push("--no-calibrate".into());
        }
        v
    }

    /// Parses the arguments that follow [`HELPER_FLAG`].
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = Self {
            connect: String::new(),
            token: String::new(),
            device: String::new(),
            mode: Mode::Quick,
            calibrate: true,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = || it.next().cloned().ok_or_else(|| format!("missing value for {a}"));
            match a.as_str() {
                "--connect" => out.connect = value()?,
                "--token" => out.token = value()?,
                "--device" => out.device = value()?,
                "--mode" => out.mode = value()?.parse()?,
                "--no-calibrate" => out.calibrate = false,
                other => return Err(format!("unknown helper argument {other}")),
            }
        }
        if out.connect.is_empty() || out.token.is_empty() || out.device.is_empty() {
            return Err("incomplete helper arguments".into());
        }
        if !out.connect.starts_with("127.0.0.1:") {
            return Err("the helper only connects to the local machine".into());
        }
        Ok(out)
    }
}

/// Entry point of helper mode. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let args = match HelperArgs::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("flashvalidator helper: {e}");
            return 2;
        }
    };
    let Ok(addr) = args.connect.parse() else { return 2 };
    let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(10)) else { return 3 };
    let _ = stream.set_nodelay(true);
    let Ok(reader) = stream.try_clone() else { return 3 };
    let writer = Mutex::new(stream);
    let send = |msg: &HelperMsg| -> bool {
        let Ok(mut line) = serde_json::to_string(msg) else { return false };
        line.push('\n');
        writer.lock().map(|mut w| w.write_all(line.as_bytes()).is_ok()).unwrap_or(false)
    };
    if !send(&HelperMsg::Hello { token: args.token.clone() }) {
        return 3;
    }

    let token = CancelToken::default();
    {
        // Cancel on request, or when the GUI goes away.
        let token = token.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                match line {
                    Ok(l) if l.trim() == "cancel" => token.cancel(),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            token.cancel();
        });
    }

    let result = (|| -> Result<Report, String> {
        let info = device::find_device(&args.device).map_err(|e| e.to_string())?;
        let access = if args.mode.writes() { Access::ReadWrite } else { Access::ReadOnly };
        let dev = info.open(access).map_err(|e| e.to_string())?;
        let cfg = Config { mode: args.mode, calibrate: args.calibrate, ..Config::default() };
        let validator = Validator::new(dev, cfg).with_device_summary(info.summary()).with_cancel_token(token.clone());
        validator
            .run(&mut |e| {
                if !send(&HelperMsg::Event(e)) {
                    token.cancel();
                }
            })
            .map_err(|e| e.to_string())
    })();
    send(&HelperMsg::Done(result.map(Box::new)));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        let a = HelperArgs {
            connect: "127.0.0.1:4000".into(),
            token: "abc".into(),
            device: r"\\.\PhysicalDrive2".into(),
            mode: Mode::ReadOnly,
            calibrate: false,
        };
        let strings: Vec<String> = a.to_args().into_iter().skip(1).map(|s| s.into_string().unwrap()).collect();
        assert_eq!(HelperArgs::parse(&strings).unwrap(), a);
    }

    #[test]
    fn refuses_remote_addresses() {
        let args: Vec<String> =
            ["--connect", "10.0.0.1:1", "--token", "t", "--device", "d"].iter().map(|s| s.to_string()).collect();
        assert!(HelperArgs::parse(&args).is_err());
    }
}
