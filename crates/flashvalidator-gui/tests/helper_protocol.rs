//! End-to-end test of the elevated-helper protocol, run without elevation.
//!
//! The helper binary is started as a normal process for a device that does
//! not exist. It must connect back, send the token first, and finish with an
//! error result, all without touching any real drive.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

#[test]
fn helper_connects_authenticates_and_reports() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_flashvalidator-gui"))
        .args(["--helper", "--connect", &addr, "--token", "s3cret", "--device", "no-such-device-xyz"])
        .args(["--mode", "read-only"])
        .spawn()
        .unwrap();

    let (stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut lines = BufReader::new(stream).lines();

    let hello: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(hello["hello"]["token"], "s3cret");

    let done: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    let err = done["done"]["Err"].as_str().expect("an error result");
    assert!(err.contains("not found"), "{err}");

    assert!(child.wait().unwrap().success());
}

#[test]
fn helper_rejects_bad_arguments() {
    let status = Command::new(env!("CARGO_BIN_EXE_flashvalidator-gui"))
        .args(["--helper", "--connect", "192.168.1.1:9", "--token", "t", "--device", "d"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}
