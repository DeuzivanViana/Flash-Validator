//! FlashValidator core library.
//!
//! FlashValidator quickly checks a USB mass-storage device for counterfeit
//! capacity and read/write failures without destroying its contents. It
//! tests 576 regions spread evenly over the declared capacity. For each one it
//! saves the original bytes, writes a random pattern, reads it back, compares,
//! and restores the original data.
//!
//! Three modes are available: *quick* (per-region write/verify/restore),
//! *thorough* (write everything, then verify, which also detects drives that
//! wrap addresses) and *read-only* (never writes to the drive).
//!
//! # Crate layout
//!
//! - [`device`]: list the drives attached to this machine and open them.
//! - [`blockdev`]: the [`BlockDevice`](blockdev::BlockDevice) abstraction
//!   over raw, cache-bypassing positional I/O.
//! - [`engine`]: the validation algorithm ([`Validator`](engine::Validator)).
//! - [`layout`]: where the regions are and in what order they are tested.
//! - [`journal`]: crash-safe backup of the original data on the host.
//! - [`report`] / [`stats`]: results and performance summaries.
//! - [`privilege`]: detecting and requesting root / administrator rights.

pub mod aligned;
pub mod blockdev;
pub mod device;
pub mod engine;
pub mod error;
pub mod format;
pub mod journal;
pub mod layout;
pub mod privilege;
pub mod report;
pub mod rng;
pub mod stats;

mod platform;
#[cfg(test)]
pub(crate) mod sim;

pub use error::{Error, Result};

/// Version of this crate, embedded in reports and journals.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
