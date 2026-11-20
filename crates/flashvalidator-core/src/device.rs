//! Listing and opening drives.

use crate::blockdev::{Access, BlockDevice};
use crate::error::{Error, Result};
use crate::format::human_bytes;
use crate::platform;
use crate::report::DeviceSummary;
use serde::{Deserialize, Serialize};

/// How a drive is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bus {
    /// USB mass storage.
    Usb,
    /// Any other bus (SATA, NVMe, SD host controller, ...).
    Other,
}

/// A drive attached to this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// OS path: `/dev/sdb` or `\\.\PhysicalDrive2`.
    pub path: String,
    /// Vendor of the drive or of the USB bridge.
    pub vendor: Option<String>,
    /// Product name.
    pub model: Option<String>,
    /// Serial number, if reported.
    pub serial: Option<String>,
    /// Declared capacity in bytes.
    pub size_bytes: u64,
    /// Logical sector size in bytes.
    pub logical_block_size: u32,
    /// How the drive is attached.
    pub bus: Bus,
    /// Whether the OS reports removable media.
    pub removable: bool,
    /// Mount points (Linux) or drive letters (Windows) of its partitions.
    pub mount_points: Vec<String>,
}

impl DeviceInfo {
    /// Whether the drive is attached through USB.
    pub fn is_usb(&self) -> bool {
        self.bus == Bus::Usb
    }

    /// `Vendor Model`, or a placeholder.
    pub fn product_name(&self) -> String {
        let parts: Vec<&str> = [self.vendor.as_deref(), self.model.as_deref()].into_iter().flatten().collect();
        if parts.is_empty() { "Unknown drive".to_owned() } else { parts.join(" ") }
    }

    /// One-line label: `/dev/sdb - Kingston DataTraveler (31.9 GB)`.
    pub fn label(&self) -> String {
        format!("{} - {} ({})", self.path, self.product_name(), human_bytes(self.size_bytes))
    }

    /// Identifying information for reports.
    pub fn summary(&self) -> DeviceSummary {
        DeviceSummary {
            path: self.path.clone(),
            vendor: self.vendor.clone(),
            model: self.model.clone(),
            serial: self.serial.clone(),
        }
    }

    /// Identity stored in journals. Based on the serial number when there is
    /// one, so a journal still matches after the drive gets a new path.
    pub fn identity(&self) -> String {
        match &self.serial {
            Some(s) if !s.is_empty() => format!("serial:{s}:{}", self.size_bytes),
            _ => format!("path:{}:{}", self.path, self.size_bytes),
        }
    }

    /// Opens the drive for raw, cache-bypassing access.
    ///
    /// With [`Access::ReadWrite`] the drive is opened exclusively on Linux
    /// (this fails while a partition is mounted). On Windows its volumes are
    /// locked and dismounted and the disk is taken offline until the returned
    /// device is dropped. [`Access::ReadOnly`] never takes any of these steps
    /// and the device cannot be written through the returned handle.
    pub fn open(&self, access: Access) -> Result<Box<dyn BlockDevice>> {
        platform::open(self, access)
    }
}

/// Lists the drives on this machine (USB and others), sorted by path.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut v = platform::enumerate()?;
    v.sort_by_key(|d| natural_key(&d.path));
    Ok(v)
}

/// Finds a drive by path.
pub fn find_device(path: &str) -> Result<DeviceInfo> {
    let wanted = normalize(path);
    list_devices()?.into_iter().find(|d| normalize(&d.path) == wanted).ok_or_else(|| Error::NotFound(path.into()))
}

fn normalize(path: &str) -> String {
    let p = path.trim();
    #[cfg(windows)]
    {
        // Accept "PhysicalDrive2", "\\.\PhysicalDrive2" or a bare "2".
        let lower = p.to_ascii_lowercase();
        if let Ok(n) = lower.parse::<u32>() {
            return format!(r"\\.\physicaldrive{n}");
        }
        let lower = lower.trim_start_matches(r"\\.\").trim_start_matches(r"\\?\");
        format!(r"\\.\{lower}")
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_owned())
    }
}

/// Sort key that orders `sdb` < `sdc` < `sdaa` and `PhysicalDrive2` < `PhysicalDrive10`.
fn natural_key(s: &str) -> (usize, String, u64) {
    let digits: String =
        s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let stem = &s[..s.len() - digits.len()];
    (stem.len(), stem.to_owned(), digits.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sorting() {
        let mut v = vec!["PhysicalDrive10", "PhysicalDrive2", "sdc", "sdb"];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(v, ["sdb", "sdc", "PhysicalDrive2", "PhysicalDrive10"]);
    }

    #[test]
    fn identity_prefers_serial() {
        let mut d = DeviceInfo {
            path: "/dev/sdb".into(),
            vendor: None,
            model: None,
            serial: Some("ABC".into()),
            size_bytes: 10,
            logical_block_size: 512,
            bus: Bus::Usb,
            removable: true,
            mount_points: vec![],
        };
        assert_eq!(d.identity(), "serial:ABC:10");
        d.serial = None;
        assert_eq!(d.identity(), "path:/dev/sdb:10");
    }
}
