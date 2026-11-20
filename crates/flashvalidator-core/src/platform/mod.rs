//! Operating-system specific device discovery and raw I/O.
//!
//! Each backend provides:
//!
//! - `enumerate() -> Result<Vec<DeviceInfo>>`
//! - `open(&DeviceInfo) -> Result<Box<dyn BlockDevice>>`

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{enumerate, open};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{enumerate, open};

#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) use unsupported::{enumerate, open};
