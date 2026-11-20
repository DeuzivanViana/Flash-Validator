//! Detecting and requesting elevated rights (root on Linux, Administrator on Windows).
//!
//! Raw drive access needs elevated rights. Rather than asking users to start
//! FlashValidator "as root" or "as administrator", the front ends start a copy of
//! themselves with elevated rights only when a test begins:
//!
//! - Linux: `sudo` in a terminal ([`Prompt::Terminal`]) or `pkexec`, which
//!   shows the desktop's polkit password dialog ([`Prompt::Graphical`]).
//! - Windows: the UAC prompt (`ShellExecuteExW` with the `runas` verb).

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// How the user is asked for their password or consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// In the current terminal (`sudo`; on Windows a new elevated console window).
    Terminal,
    /// A graphical dialog (`pkexec` / UAC); the elevated process has no visible window.
    Graphical,
}

/// Whether this process already runs with elevated rights.
pub fn is_elevated() -> bool {
    imp::is_elevated()
}

/// Whether this process can open `path` for raw access without elevation
/// (for example as root, or as a member of the `disk` group on Linux).
pub fn can_open_raw(path: &str, write: bool) -> bool {
    imp::can_open_raw(path, write)
}

/// Starts `program` with `args` and elevated rights.
///
/// Fails with [`io::ErrorKind::PermissionDenied`] if the user declines the
/// Windows prompt, or [`io::ErrorKind::NotFound`] if no elevation tool is
/// available. On Linux a declined `pkexec` dialog shows up as exit code 126
/// or 127 (see [`ElevatedProcess::was_denied`]).
pub fn spawn_elevated(program: &Path, args: &[OsString], prompt: Prompt) -> io::Result<ElevatedProcess> {
    imp::spawn(program, args, prompt)
}

/// A process started with elevated rights.
#[derive(Debug)]
pub struct ElevatedProcess {
    inner: imp::Child,
}

impl ElevatedProcess {
    /// Exit code if the process has finished, without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.inner.try_wait()
    }

    /// Waits for the process and returns its exit code.
    pub fn wait(&mut self) -> io::Result<i32> {
        self.inner.wait()
    }

    /// Forcefully terminates the process (last resort; it cannot restore data).
    pub fn kill(&mut self) {
        self.inner.kill();
    }

    /// Whether `code` means the user declined or failed authentication.
    pub fn was_denied(code: i32) -> bool {
        // pkexec: 126 = dialog dismissed, 127 = not authorized.
        cfg!(target_os = "linux") && (code == 126 || code == 127)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Prompt;
    use std::ffi::{CString, OsString};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    pub fn is_elevated() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    pub fn can_open_raw(path: &str, write: bool) -> bool {
        if is_elevated() {
            return true;
        }
        let Ok(c) = CString::new(path) else { return false };
        let mode = if write { libc::R_OK | libc::W_OK } else { libc::R_OK };
        // SAFETY: `c` is a valid NUL-terminated string.
        unsafe { libc::access(c.as_ptr(), mode) == 0 }
    }

    fn find_in_path(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .chain(["/usr/bin".into(), "/bin".into(), "/usr/local/bin".into()])
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }

    pub fn spawn(program: &Path, args: &[OsString], prompt: Prompt) -> io::Result<super::ElevatedProcess> {
        let order: &[&str] = match prompt {
            Prompt::Terminal => &["sudo", "pkexec"],
            Prompt::Graphical => &["pkexec"],
        };
        let Some((tool, path)) = order.iter().find_map(|t| find_in_path(t).map(|p| (*t, p))) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "neither pkexec nor sudo was found; install polkit (pkexec) or run FlashValidator as root",
            ));
        };
        let mut cmd = Command::new(path);
        if tool == "sudo" {
            cmd.arg("--");
        }
        cmd.arg(program).args(args);
        if prompt == Prompt::Graphical {
            cmd.stdin(Stdio::null());
        }
        Ok(super::ElevatedProcess { inner: Child(cmd.spawn()?) })
    }

    #[derive(Debug)]
    pub struct Child(std::process::Child);

    impl Child {
        pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
            Ok(self.0.try_wait()?.map(|s| s.code().unwrap_or(1)))
        }
        pub fn wait(&mut self) -> io::Result<i32> {
            Ok(self.0.wait()?.code().unwrap_or(1))
        }
        pub fn kill(&mut self) {
            let _ = self.0.kill();
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Prompt;
    use std::ffi::{OsStr, OsString, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, TerminateProcess, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    const SW_HIDE: i32 = 0;
    const SW_SHOWNORMAL: i32 = 1;

    pub fn is_elevated() -> bool {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: valid out-pointer; the pseudo handle needs no closing.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        // SAFETY: the buffer is a TOKEN_ELEVATION of the size passed.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut c_void,
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
        };
        // SAFETY: `token` was opened above.
        unsafe { CloseHandle(token) };
        ok != 0 && elevation.TokenIsElevated != 0
    }

    pub fn can_open_raw(_path: &str, _write: bool) -> bool {
        is_elevated()
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// Quotes one argument following the rules of `CommandLineToArgvW`.
    pub(super) fn quote(arg: &OsStr) -> String {
        let s = arg.to_string_lossy();
        if !s.is_empty() && !s.contains([' ', '\t', '\n', '\x0b', '"']) {
            return s.into_owned();
        }
        let mut out = String::from('"');
        let mut backslashes = 0;
        for c in s.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                _ => {
                    out.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(backslashes * 2));
        out.push('"');
        out
    }

    pub fn spawn(program: &Path, args: &[OsString], prompt: Prompt) -> io::Result<super::ElevatedProcess> {
        let params: Vec<String> = args.iter().map(|a| quote(a)).collect();
        let verb = wide(OsStr::new("runas"));
        let file = wide(program.as_os_str());
        let params = wide(OsStr::new(&params.join(" ")));
        // SAFETY: SHELLEXECUTEINFOW is a plain C struct; all-zero is a valid initial state.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = if prompt == Prompt::Graphical { SW_HIDE } else { SW_SHOWNORMAL };
        // SAFETY: all strings outlive the call.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the administrator prompt was declined"));
            }
            return Err(err);
        }
        if info.hProcess.is_null() {
            return Err(io::Error::other("the elevated process could not be tracked"));
        }
        Ok(super::ElevatedProcess { inner: Child(info.hProcess as isize) })
    }

    /// Process handle stored as an integer so the type is `Send`.
    #[derive(Debug)]
    pub struct Child(isize);

    impl Child {
        fn handle(&self) -> HANDLE {
            self.0 as HANDLE
        }
        fn exit_code(&self) -> io::Result<i32> {
            let mut code = 0u32;
            // SAFETY: valid process handle and out-pointer.
            if unsafe { GetExitCodeProcess(self.handle(), &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(code as i32)
        }
        pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
            // SAFETY: valid process handle.
            if unsafe { WaitForSingleObject(self.handle(), 0) } == WAIT_OBJECT_0 {
                self.exit_code().map(Some)
            } else {
                Ok(None)
            }
        }
        pub fn wait(&mut self) -> io::Result<i32> {
            // SAFETY: valid process handle.
            unsafe { WaitForSingleObject(self.handle(), INFINITE) };
            self.exit_code()
        }
        pub fn kill(&mut self) {
            // SAFETY: valid process handle.
            unsafe { TerminateProcess(self.handle(), 1) };
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            // SAFETY: we own the handle returned by ShellExecuteExW.
            unsafe { CloseHandle(self.handle()) };
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::Prompt;
    use std::ffi::OsString;
    use std::io;
    use std::path::Path;

    pub fn is_elevated() -> bool {
        false
    }
    pub fn can_open_raw(_path: &str, _write: bool) -> bool {
        false
    }
    pub fn spawn(_program: &Path, _args: &[OsString], _prompt: Prompt) -> io::Result<super::ElevatedProcess> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "elevation is not supported on this platform"))
    }

    #[derive(Debug)]
    pub struct Child;

    impl Child {
        pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
            Ok(Some(1))
        }
        pub fn wait(&mut self) -> io::Result<i32> {
            Ok(1)
        }
        pub fn kill(&mut self) {}
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::imp::quote;
    use std::ffi::OsStr;

    #[test]
    fn windows_quoting() {
        assert_eq!(quote(OsStr::new(r"\\.\PhysicalDrive2")), r"\\.\PhysicalDrive2");
        assert_eq!(quote(OsStr::new("a b")), "\"a b\"");
        assert_eq!(quote(OsStr::new("")), "\"\"");
        assert_eq!(quote(OsStr::new(r#"say "hi""#)), r#""say \"hi\"""#);
        assert_eq!(quote(OsStr::new(r"C:\dir with space\")), r#""C:\dir with space\\""#);
    }
}
