//! Linux backend: sysfs discovery and `O_DIRECT` block-device I/O.

use crate::blockdev::{Access, BlockDevice};
use crate::device::{Bus, DeviceInfo};
use crate::error::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// `BLKSSZGET`: logical sector size.
const BLKSSZGET: u64 = 0x1268;
/// `BLKFLSBUF`: flush (and drop) the kernel buffer cache for the device.
const BLKFLSBUF: u64 = 0x1261;

/// Kernel device-name prefixes that are never physical drives.
const IGNORED_PREFIXES: &[&str] = &["loop", "ram", "zram", "dm-", "md", "sr", "nbd", "fd", "zd"];

pub(crate) fn enumerate() -> Result<Vec<DeviceInfo>> {
    let mounts = read_mounts();
    let mut out = Vec::new();
    for entry in fs::read_dir("/sys/block")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if IGNORED_PREFIXES.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let sys = Path::new("/sys/block").join(&name);
        out.push(describe(&name, &sys, &mounts));
    }
    Ok(out)
}

fn describe(name: &str, sys: &Path, mounts: &[(String, String)]) -> DeviceInfo {
    let real = fs::canonicalize(sys).unwrap_or_else(|_| sys.to_path_buf());
    let is_usb = real.to_string_lossy().contains("/usb");
    // sysfs "size" is always in 512-byte units, whatever the sector size.
    let sectors: u64 = read_trim(sys.join("size")).and_then(|s| s.parse().ok()).unwrap_or(0);
    let logical_block_size =
        read_trim(sys.join("queue/logical_block_size")).and_then(|s| s.parse().ok()).unwrap_or(512);
    let removable = read_trim(sys.join("removable")).as_deref() == Some("1");

    let mut vendor = read_trim(sys.join("device/vendor"));
    let mut model = read_trim(sys.join("device/model"));
    let mut serial = read_trim(sys.join("device/serial")).or_else(|| read_trim(sys.join("serial")));
    if let Some(usb) = usb_device_dir(sys) {
        serial = serial.or_else(|| read_trim(usb.join("serial")));
        if vendor.is_none() {
            vendor = read_trim(usb.join("manufacturer"));
        }
        if model.is_none() {
            model = read_trim(usb.join("product"));
        }
    }

    let mut nodes = vec![format!("/dev/{name}")];
    if let Ok(rd) = fs::read_dir(sys) {
        for e in rd.flatten() {
            if e.path().join("partition").exists() {
                nodes.push(format!("/dev/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    let mount_points = mounts.iter().filter(|(dev, _)| nodes.contains(dev)).map(|(_, mp)| mp.clone()).collect();

    DeviceInfo {
        path: format!("/dev/{name}"),
        vendor,
        model,
        serial,
        size_bytes: sectors * 512,
        logical_block_size,
        bus: if is_usb { Bus::Usb } else { Bus::Other },
        removable,
        mount_points,
    }
}

/// Walks up from the block device to the USB device directory (the one with `idVendor`).
fn usb_device_dir(sys: &Path) -> Option<PathBuf> {
    let dev = fs::canonicalize(sys.join("device")).ok()?;
    dev.ancestors().find(|a| a.join("idVendor").exists()).map(Path::to_path_buf)
}

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// `(device node, mount point)` pairs from `/proc/self/mounts`.
fn read_mounts() -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string("/proc/self/mounts") else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let dev = unescape(it.next()?);
            let mp = unescape(it.next()?);
            if !dev.starts_with("/dev/") {
                return None;
            }
            let dev = fs::canonicalize(&dev).map(|p| p.to_string_lossy().into_owned()).unwrap_or(dev);
            Some((dev, mp))
        })
        .collect()
}

/// Decodes the octal escapes (`\040` = space) used in `/proc/mounts`.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An open block device.
#[derive(Debug)]
struct LinuxDevice {
    file: File,
    size: u64,
    logical_block_size: u32,
    identity: String,
}

pub(crate) fn open(info: &DeviceInfo, access: Access) -> Result<Box<dyn BlockDevice>> {
    let mut opts = OpenOptions::new();
    match access {
        // O_RDONLY + O_DIRECT: the kernel rejects every write on this descriptor.
        // Reading is safe while partitions are mounted, so no O_EXCL.
        Access::ReadOnly => opts.read(true).custom_flags(libc::O_DIRECT),
        // O_DIRECT: bypass the page cache. O_SYNC: writes reach the device.
        // O_EXCL on a block device: fail with EBUSY if mounted or in use.
        Access::ReadWrite => opts.read(true).write(true).custom_flags(libc::O_DIRECT | libc::O_SYNC | libc::O_EXCL),
    };
    let file = opts.open(&info.path).map_err(|e| map_open_error(e, &info.path))?;

    let size = (&file).seek(SeekFrom::End(0))?;
    (&file).seek(SeekFrom::Start(0))?;

    let fd = file.as_raw_fd();
    let mut lbs: libc::c_int = 0;
    // SAFETY: BLKSSZGET writes one c_int to the pointer we pass.
    let logical_block_size = if unsafe { libc::ioctl(fd, BLKSSZGET as _, &mut lbs as *mut libc::c_int) } == 0 && lbs > 0
    {
        lbs as u32
    } else {
        info.logical_block_size
    };
    // Drop any cached pages from earlier buffered access (needs CAP_SYS_ADMIN; best effort).
    // SAFETY: BLKFLSBUF takes no argument.
    unsafe { libc::ioctl(fd, BLKFLSBUF as _, 0) };

    Ok(Box::new(LinuxDevice { file, size, logical_block_size, identity: info.identity() }))
}

fn map_open_error(e: io::Error, path: &str) -> Error {
    match e.raw_os_error() {
        Some(libc::EBUSY) => Error::Busy(path.to_owned()),
        Some(libc::EACCES | libc::EPERM) => Error::PermissionDenied {
            path: path.to_owned(),
            hint: "FlashValidator needs root rights for raw drive access (it asks for them automatically via sudo or pkexec).",
        },
        Some(libc::EROFS) => Error::WriteProtected,
        Some(libc::ENOENT | libc::ENXIO | libc::ENODEV) => Error::NotFound(path.to_owned()),
        Some(libc::ENOMEDIUM) => Error::NoStorage,
        _ => Error::Io(e),
    }
}

impl BlockDevice for LinuxDevice {
    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn logical_block_size(&self) -> u32 {
        self.logical_block_size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.file.read_exact_at(buf, offset)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.file.write_all_at(buf, offset)
    }

    fn flush(&mut self) -> io::Result<()> {
        // fsync on a block device also sends a cache-flush command to the drive.
        self.file.sync_all()
    }

    fn identity(&self) -> String {
        self.identity.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_mount_paths() {
        assert_eq!(unescape(r"/media/my\040stick"), "/media/my stick");
        assert_eq!(unescape("/plain"), "/plain");
        assert_eq!(unescape(r"trailing\04"), r"trailing\04");
    }
}
