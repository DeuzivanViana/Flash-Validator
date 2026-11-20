//! Error type shared by the whole crate.

use std::io;

/// Errors that stop a validation run or a device operation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O error that does not fit a more specific variant.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The process may not open the device for raw access.
    #[error("permission denied opening {path}. {hint}")]
    PermissionDenied {
        /// Device path that could not be opened.
        path: String,
        /// Platform-specific advice for the user.
        hint: &'static str,
    },

    /// The device is mounted or held open by another program.
    #[error("{0} is busy: unmount all of its partitions (or close programs using it) and retry")]
    Busy(String),

    /// The requested device does not exist.
    #[error("device not found: {0}")]
    NotFound(String),

    /// The device refused writes because it is write protected.
    #[error("the drive appears to be write protected; disable write protection and retry")]
    WriteProtected,

    /// None of the first sectors could be read.
    #[error("repeated test reads of the drive's first sectors all failed; the drive may be dead")]
    ReadFailure,

    /// None of the first sectors could be written (and not because of write protection).
    #[error("repeated test writes to the drive's first sectors all failed; the drive may be dead")]
    WriteFailure,

    /// The device reports no storage (for example an empty card reader or a hub).
    #[error("the device reports no storage space (it may be a card reader without media or an interface)")]
    NoStorage,

    /// The device is too small for the configured number of regions.
    #[error("the device is too small ({size} bytes) to be tested with {regions} regions of {block} bytes")]
    TooSmall {
        /// Device size in bytes.
        size: u64,
        /// Configured region count.
        regions: usize,
        /// Configured block size.
        block: usize,
    },

    /// The device disappeared while it was being tested.
    #[error("the drive was removed or stopped responding during the test")]
    DeviceRemoved,

    /// The user cancelled the run.
    #[error("the operation was cancelled")]
    Cancelled,

    /// Raw device access is not implemented on this operating system.
    #[error("raw device access is not supported on this operating system")]
    Unsupported,

    /// A backup journal is malformed or does not match the device.
    #[error("journal error: {0}")]
    Journal(String),

    /// The configuration is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
