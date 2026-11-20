//! Windows backend: `\\.\PhysicalDriveN` discovery and unbuffered disk I/O.
//!
//! Windows refuses raw writes to sectors inside a mounted volume. Before
//! testing, FlashValidator locks and dismounts every volume with a drive letter
//! on the disk, then takes the disk offline (not persistent). Dropping the
//! device brings the disk back online.

use crate::blockdev::{Access, BlockDevice};
use crate::device::{Bus, DeviceInfo};
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetLogicalDrives,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

// Control codes (winioctl.h).
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const IOCTL_DISK_SET_DISK_ATTRIBUTES: u32 = 0x0007_C0F4;
const FSCTL_ALLOW_EXTENDED_DASD_IO: u32 = 0x0009_0083;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;

const FILE_DEVICE_DISK: u32 = 7;
const BUS_TYPE_USB: u32 = 7;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DISK_ATTRIBUTE_OFFLINE: u64 = 1;
const MAX_PHYSICAL_DRIVES: u32 = 64;

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PATH_NOT_FOUND: i32 = 3;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_WRITE_PROTECT: i32 = 19;
const ERROR_NOT_READY: i32 = 21;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Sends a control code to `file`; returns the number of output bytes.
fn ioctl(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<usize> {
    let mut returned: u32 = 0;
    // SAFETY: the buffers are valid for the lengths passed; synchronous call.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            code,
            if input.is_empty() { std::ptr::null() } else { input.as_ptr() as *const c_void },
            input.len() as u32,
            if output.is_empty() { std::ptr::null_mut() } else { output.as_mut_ptr() as *mut c_void },
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(returned as usize) }
}

/// Opens a device for IOCTL queries only. This needs no administrator rights.
fn open_query(path: &str) -> io::Result<File> {
    OpenOptions::new().access_mode(0).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path)
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

/// Reads a NUL-terminated ASCII string at `offset` inside a descriptor.
fn string_at(b: &[u8], offset: usize) -> Option<String> {
    if offset == 0 || offset >= b.len() {
        return None;
    }
    let end = b[offset..].iter().position(|&c| c == 0).map_or(b.len(), |p| offset + p);
    let s = String::from_utf8_lossy(&b[offset..end]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

struct Descriptor {
    vendor: Option<String>,
    product: Option<String>,
    serial: Option<String>,
    removable: bool,
    bus_type: u32,
}

/// `IOCTL_STORAGE_QUERY_PROPERTY` / `StorageDeviceProperty`.
fn query_descriptor(file: &File) -> io::Result<Descriptor> {
    let query = [0u8; 12]; // PropertyId = StorageDeviceProperty, QueryType = PropertyStandardQuery
    let mut out = vec![0u8; 1024];
    let n = ioctl(file, IOCTL_STORAGE_QUERY_PROPERTY, &query, &mut out)?;
    if n < 36 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short STORAGE_DEVICE_DESCRIPTOR"));
    }
    out.truncate(n);
    Ok(Descriptor {
        removable: out[10] != 0,
        vendor: string_at(&out, u32_at(&out, 12) as usize),
        product: string_at(&out, u32_at(&out, 16) as usize),
        serial: string_at(&out, u32_at(&out, 24) as usize),
        bus_type: u32_at(&out, 28),
    })
}

/// `(disk size in bytes, bytes per sector)`.
fn geometry(file: &File) -> io::Result<(u64, u32)> {
    let mut out = [0u8; 256];
    let n = ioctl(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut out)?;
    if n < 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short DISK_GEOMETRY_EX"));
    }
    Ok((u64_at(&out, 24), u32_at(&out, 20)))
}

/// `(device type, device number)` of a disk or volume handle.
fn device_number(file: &File) -> io::Result<(u32, u32)> {
    let mut out = [0u8; 12];
    ioctl(file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut out)?;
    Ok((u32_at(&out, 0), u32_at(&out, 4)))
}

/// Maps physical-drive numbers to their drive letters (`"E:"`).
fn drive_letters() -> HashMap<u32, Vec<String>> {
    let mut map: HashMap<u32, Vec<String>> = HashMap::new();
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    for i in 2..26u32 {
        // Skip A: and B: (floppy drives).
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i as u8);
        let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(Some(0)).collect();
        // SAFETY: `root` is a NUL-terminated UTF-16 string.
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if kind != DRIVE_REMOVABLE && kind != DRIVE_FIXED {
            continue;
        }
        if let Ok(f) = open_query(&format!(r"\\.\{letter}:")) {
            if let Ok((FILE_DEVICE_DISK, number)) = device_number(&f) {
                map.entry(number).or_default().push(format!("{letter}:"));
            }
        }
    }
    map
}

pub(crate) fn enumerate() -> Result<Vec<DeviceInfo>> {
    let letters = drive_letters();
    let mut out = Vec::new();
    for n in 0..MAX_PHYSICAL_DRIVES {
        let path = format!(r"\\.\PhysicalDrive{n}");
        let Ok(file) = open_query(&path) else { continue };
        let desc = query_descriptor(&file).ok();
        let (size_bytes, logical_block_size) = geometry(&file).unwrap_or((0, 512));
        out.push(DeviceInfo {
            path,
            vendor: desc.as_ref().and_then(|d| d.vendor.clone()),
            model: desc.as_ref().and_then(|d| d.product.clone()),
            serial: desc.as_ref().and_then(|d| d.serial.clone()),
            size_bytes,
            logical_block_size,
            bus: if desc.as_ref().is_some_and(|d| d.bus_type == BUS_TYPE_USB) { Bus::Usb } else { Bus::Other },
            removable: desc.as_ref().is_some_and(|d| d.removable),
            mount_points: letters.get(&n).cloned().unwrap_or_default(),
        });
    }
    Ok(out)
}

/// An open physical drive, plus the locks that keep it safe to write.
#[derive(Debug)]
struct WindowsDevice {
    file: File,
    size: u64,
    logical_block_size: u32,
    identity: String,
    offline: bool,
    /// Locked and dismounted volume handles; closing them unlocks the volumes.
    _volumes: Vec<File>,
}

pub(crate) fn open(info: &DeviceInfo, access: Access) -> Result<Box<dyn BlockDevice>> {
    if access == Access::ReadOnly {
        // GENERIC_READ only: Windows rejects any write on this handle. The
        // volumes stay mounted and the disk stays online.
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_NO_BUFFERING)
            .open(&info.path)
            .map_err(|e| map_open_error(e, &info.path))?;
        let (size, logical_block_size) = geometry(&file)?;
        return Ok(Box::new(WindowsDevice {
            file,
            size,
            logical_block_size,
            identity: info.identity(),
            offline: false,
            _volumes: Vec::new(),
        }));
    }

    // 1. Lock and dismount every lettered volume on this disk.
    let mut volumes = Vec::new();
    for mp in &info.mount_points {
        let vpath = format!(r"\\.\{}", mp.trim_end_matches('\\'));
        if let Ok(v) =
            OpenOptions::new().read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(&vpath)
        {
            // Locking fails while files are open; dismounting still forces
            // the file system off the volume.
            let _ = ioctl(&v, FSCTL_LOCK_VOLUME, &[], &mut []);
            let _ = ioctl(&v, FSCTL_DISMOUNT_VOLUME, &[], &mut []);
            volumes.push(v);
        }
    }

    // 2. Open the disk itself, unbuffered and write-through.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH)
        .open(&info.path)
        .map_err(|e| map_open_error(e, &info.path))?;
    let _ = ioctl(&file, FSCTL_ALLOW_EXTENDED_DASD_IO, &[], &mut []);

    let (size, logical_block_size) = geometry(&file)?;

    // 3. Take the disk offline (not persistent) so nothing remounts it.
    let offline = set_offline(&file, true).is_ok();

    Ok(Box::new(WindowsDevice {
        file,
        size,
        logical_block_size,
        identity: info.identity(),
        offline,
        _volumes: volumes,
    }))
}

/// `IOCTL_DISK_SET_DISK_ATTRIBUTES` with `DISK_ATTRIBUTE_OFFLINE`.
fn set_offline(file: &File, offline: bool) -> io::Result<()> {
    let mut attrs = [0u8; 40]; // SET_DISK_ATTRIBUTES
    attrs[0..4].copy_from_slice(&40u32.to_le_bytes()); // Version = sizeof
    // attrs[4] = Persist = FALSE
    attrs[8..16].copy_from_slice(&(if offline { DISK_ATTRIBUTE_OFFLINE } else { 0 }).to_le_bytes());
    attrs[16..24].copy_from_slice(&DISK_ATTRIBUTE_OFFLINE.to_le_bytes()); // AttributesMask
    ioctl(file, IOCTL_DISK_SET_DISK_ATTRIBUTES, &attrs, &mut []).map(|_| ())
}

fn map_open_error(e: io::Error, path: &str) -> Error {
    match e.raw_os_error() {
        Some(ERROR_ACCESS_DENIED) => Error::PermissionDenied {
            path: path.to_owned(),
            hint: "FlashValidator needs administrator rights for raw drive access (accept the Windows prompt).",
        },
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => Error::NotFound(path.to_owned()),
        Some(ERROR_SHARING_VIOLATION) => Error::Busy(path.to_owned()),
        Some(ERROR_WRITE_PROTECT) => Error::WriteProtected,
        Some(ERROR_NOT_READY) => Error::NoStorage,
        _ => Error::Io(e),
    }
}

impl BlockDevice for WindowsDevice {
    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn logical_block_size(&self) -> u32 {
        self.logical_block_size
    }

    fn read_at(&mut self, offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        let mut pos = offset;
        while !buf.is_empty() {
            match self.file.seek_read(buf, pos) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of disk")),
                Ok(n) => {
                    buf = &mut buf[n..];
                    pos += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, mut buf: &[u8]) -> io::Result<()> {
        let mut pos = offset;
        while !buf.is_empty() {
            match self.file.seek_write(buf, pos) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned zero bytes")),
                Ok(n) => {
                    buf = &buf[n..];
                    pos += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn identity(&self) -> String {
        self.identity.clone()
    }
}

impl Drop for WindowsDevice {
    fn drop(&mut self) {
        if self.offline {
            let _ = set_offline(&self.file, false);
            let _ = ioctl(&self.file, IOCTL_DISK_UPDATE_PROPERTIES, &[], &mut []);
        }
        // `_volumes` is dropped after this, which unlocks the volumes.
    }
}
