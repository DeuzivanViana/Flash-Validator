//! Abstraction over a raw block device with positional, cache-bypassing I/O.

use std::io;

/// How a device is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Reads only. The OS handle has no write permission, so the drive's
    /// contents cannot be modified through it, even by a bug.
    ReadOnly,
    /// Reads and writes (exclusive; volumes are unmounted or locked).
    ReadWrite,
}

/// A device that can be read and written at arbitrary aligned offsets.
///
/// Implementations must bypass the operating system's page cache, so that a
/// read-back really comes from the drive and not from host memory. All
/// offsets and lengths passed by the engine are multiples of 4 KiB, and
/// buffers are 4 KiB-aligned.
pub trait BlockDevice: Send {
    /// Capacity the device declares, in bytes.
    fn size_bytes(&self) -> u64;

    /// Logical sector size in bytes (informational).
    fn logical_block_size(&self) -> u32 {
        512
    }

    /// Reads exactly `buf.len()` bytes at `offset`.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Writes all of `buf` at `offset`.
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;

    /// Asks the device to commit any data cached in the device itself.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Stable, human-readable identity (path plus serial if known). It is
    /// stored in journals so they can only be replayed onto the same drive.
    fn identity(&self) -> String;
}

impl<T: BlockDevice + ?Sized> BlockDevice for Box<T> {
    fn size_bytes(&self) -> u64 {
        (**self).size_bytes()
    }
    fn logical_block_size(&self) -> u32 {
        (**self).logical_block_size()
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        (**self).write_at(offset, buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (**self).flush()
    }
    fn identity(&self) -> String {
        (**self).identity()
    }
}

/// How an I/O error affects the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The device is gone (unplugged, or the kernel dropped it). The run stops.
    DeviceGone,
    /// The device rejected the write because it is write protected.
    WriteProtected,
    /// Any other error, counted against the region being tested.
    Other,
}

/// Classifies an I/O error: a vanished device stops the run, write protection
/// is reported by the pre-flight check, anything else counts against a region.
pub fn classify(err: &io::Error) -> FailureKind {
    if err.kind() == io::ErrorKind::NotFound {
        return FailureKind::DeviceGone;
    }
    match err.raw_os_error() {
        Some(code) => classify_os(code),
        None => FailureKind::Other,
    }
}

#[cfg(target_os = "linux")]
fn classify_os(code: i32) -> FailureKind {
    match code {
        libc::ENODEV | libc::ENXIO | libc::ENOMEDIUM | libc::ESHUTDOWN => FailureKind::DeviceGone,
        libc::EROFS => FailureKind::WriteProtected,
        _ => FailureKind::Other,
    }
}

#[cfg(windows)]
fn classify_os(code: i32) -> FailureKind {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_WRITE_PROTECT: i32 = 19;
    const ERROR_NOT_READY: i32 = 21;
    const ERROR_DEV_NOT_EXIST: i32 = 55;
    const ERROR_IO_DEVICE: i32 = 1117;
    const ERROR_DEVICE_NOT_CONNECTED: i32 = 1167;
    const ERROR_NO_SUCH_DEVICE: i32 = 433;
    match code {
        ERROR_FILE_NOT_FOUND
        | ERROR_DEV_NOT_EXIST
        | ERROR_NOT_READY
        | ERROR_DEVICE_NOT_CONNECTED
        | ERROR_NO_SUCH_DEVICE => FailureKind::DeviceGone,
        // Some USB bridges report write protection as ERROR_IO_DEVICE.
        ERROR_WRITE_PROTECT | ERROR_IO_DEVICE => FailureKind::WriteProtected,
        _ => FailureKind::Other,
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn classify_os(_code: i32) -> FailureKind {
    FailureKind::Other
}
