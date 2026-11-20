//! Crash-safe backup of the original data of every tested region.
//!
//! Before any test pattern is written to the drive, the region's original
//! bytes are appended to a journal file on the **host** computer and flushed
//! to stable storage. Once the original is written back, a "restored" marker
//! is appended. If the run is interrupted (crash, power loss, drive pulled),
//! [`replay`] writes every un-restored original back.
//!
//! # File format (little-endian)
//!
//! ```text
//! header : "FLVJRN01" | u16 identity_len | identity bytes | u64 device_size
//! backup : 0x01 | u64 offset | u32 len | data[len] | u64 fnv1a(offset, len, data)
//! restore: 0x02 | u64 offset | u32 len | u64 fnv1a(offset, len)
//! ```
//!
//! A truncated or corrupt trailing record (from a crash while writing it) is
//! ignored. The pattern write for that region had not started yet.

use crate::aligned::AlignedBuffer;
use crate::blockdev::BlockDevice;
use crate::error::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"FLVJRN01";
const KIND_BACKUP: u8 = 1;
const KIND_RESTORED: u8 = 2;
/// File extension used for journals.
pub const EXTENSION: &str = "fvj";

/// An open journal being written during a run.
#[derive(Debug)]
pub struct Journal {
    file: File,
    path: PathBuf,
    pending: usize,
}

impl Journal {
    /// Creates a new journal in `dir` for the device `identity` of `size` bytes.
    pub fn create(dir: &Path, identity: &str, size: u64) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let stamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        let path = dir.join(format!("flashvalidator-{stamp}-{}.{EXTENSION}", std::process::id()));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
        let id = identity.as_bytes();
        let id_len = u16::try_from(id.len()).map_err(|_| Error::Journal("identity too long".into()))?;
        let mut header = Vec::with_capacity(8 + 2 + id.len() + 8);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&id_len.to_le_bytes());
        header.extend_from_slice(id);
        header.extend_from_slice(&size.to_le_bytes());
        file.write_all(&header)?;
        file.sync_all()?;
        Ok(Self { file, path, pending: 0 })
    }

    /// Path of the journal file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of backups not yet marked as restored.
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Appends the original contents of `offset`. Call [`Journal::sync`]
    /// before writing anything to that region.
    pub fn record_backup(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let len = u32::try_from(data.len()).map_err(|_| Error::Journal("record too large".into()))?;
        let mut rec = Vec::with_capacity(1 + 8 + 4 + data.len() + 8);
        rec.push(KIND_BACKUP);
        rec.extend_from_slice(&offset.to_le_bytes());
        rec.extend_from_slice(&len.to_le_bytes());
        rec.extend_from_slice(data);
        rec.extend_from_slice(&checksum(offset, len, Some(data)).to_le_bytes());
        self.file.write_all(&rec)?;
        self.pending += 1;
        Ok(())
    }

    /// Records that the original contents of `offset` were written back.
    pub fn mark_restored(&mut self, offset: u64, len: usize) -> Result<()> {
        let len = u32::try_from(len).map_err(|_| Error::Journal("record too large".into()))?;
        let mut rec = Vec::with_capacity(1 + 8 + 4 + 8);
        rec.push(KIND_RESTORED);
        rec.extend_from_slice(&offset.to_le_bytes());
        rec.extend_from_slice(&len.to_le_bytes());
        rec.extend_from_slice(&checksum(offset, len, None).to_le_bytes());
        self.file.write_all(&rec)?;
        self.pending = self.pending.saturating_sub(1);
        Ok(())
    }

    /// Flushes the journal to stable storage.
    pub fn sync(&mut self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }

    /// Closes the journal. It is deleted if everything was restored;
    /// otherwise its path is returned so the user can run a restore.
    pub fn finish(self) -> Result<Option<PathBuf>> {
        self.file.sync_all()?;
        if self.pending == 0 {
            drop(self.file);
            fs::remove_file(&self.path)?;
            Ok(None)
        } else {
            Ok(Some(self.path))
        }
    }
}

/// A parsed journal.
#[derive(Debug, Clone)]
pub struct JournalContents {
    /// Identity of the device the journal belongs to.
    pub identity: String,
    /// Declared size of that device.
    pub device_size: u64,
    /// Backups not marked as restored, in recording order.
    pub pending: Vec<(u64, Vec<u8>)>,
    /// Whether a truncated or corrupt trailing record was skipped.
    pub truncated_tail: bool,
}

/// Parses a journal file.
pub fn read_journal(path: &Path) -> Result<JournalContents> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).map_err(|_| Error::Journal("file too short".into()))?;
    if &magic != MAGIC {
        return Err(Error::Journal("not a FlashValidator journal".into()));
    }
    let id_len = read_u16(&mut r)?.ok_or_else(|| Error::Journal("truncated header".into()))? as usize;
    let mut id = vec![0u8; id_len];
    r.read_exact(&mut id).map_err(|_| Error::Journal("truncated header".into()))?;
    let device_size = read_u64(&mut r)?.ok_or_else(|| Error::Journal("truncated header".into()))?;

    let mut entries: Vec<(u64, Vec<u8>, bool)> = Vec::new();
    let mut truncated_tail = false;
    loop {
        let mut kind = [0u8; 1];
        if r.read(&mut kind)? == 0 {
            break;
        }
        let parsed = match kind[0] {
            KIND_BACKUP => read_backup(&mut r)?,
            KIND_RESTORED => read_restored(&mut r)?,
            _ => None,
        };
        let Some(rec) = parsed else {
            truncated_tail = true;
            break;
        };
        match rec {
            Record::Backup(off, data) => entries.push((off, data, false)),
            Record::Restored(off, len) => {
                if let Some(e) = entries.iter_mut().rev().find(|e| !e.2 && e.0 == off && e.1.len() == len) {
                    e.2 = true;
                }
            }
        }
    }
    Ok(JournalContents {
        identity: String::from_utf8_lossy(&id).into_owned(),
        device_size,
        pending: entries.into_iter().filter(|e| !e.2).map(|e| (e.0, e.1)).collect(),
        truncated_tail,
    })
}

/// Writes every pending backup in `path` back onto `dev`.
///
/// Backups are applied newest first. Where records overlap (the calibration
/// blocks overlap the last region), the oldest record, which holds the true
/// original data, is written last. Returns the number of records written.
///
/// The device's size must match. Its identity must match too, unless
/// `ignore_identity` is set (for example when the drive has no serial number
/// and its path changed after reconnecting).
pub fn replay(path: &Path, dev: &mut dyn BlockDevice, ignore_identity: bool) -> Result<usize> {
    let j = read_journal(path)?;
    if j.device_size != dev.size_bytes() {
        return Err(Error::Journal(format!(
            "device size {} does not match journal ({})",
            dev.size_bytes(),
            j.device_size
        )));
    }
    if !ignore_identity && j.identity != dev.identity() {
        return Err(Error::Journal(format!(
            "device identity '{}' does not match journal '{}'",
            dev.identity(),
            j.identity
        )));
    }
    for (offset, data) in j.pending.iter().rev() {
        let mut buf = AlignedBuffer::new(data.len().next_multiple_of(crate::aligned::ALIGNMENT));
        buf[..data.len()].copy_from_slice(data);
        dev.write_at(*offset, &buf[..data.len()])?;
    }
    dev.flush()?;
    Ok(j.pending.len())
}

/// Default directory for journals: `$XDG_STATE_HOME/flashvalidator`,
/// `~/.local/state/flashvalidator`, or `%LOCALAPPDATA%\FlashValidator`, falling back
/// to the temporary directory.
pub fn default_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("FlashValidator").join("journals");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(d) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
            return PathBuf::from(d).join("flashvalidator");
        }
        if let Some(h) = std::env::var_os("HOME").filter(|d| !d.is_empty()) {
            return PathBuf::from(h).join(".local/state/flashvalidator");
        }
    }
    std::env::temp_dir().join("flashvalidator")
}

enum Record {
    Backup(u64, Vec<u8>),
    Restored(u64, usize),
}

fn read_backup(r: &mut impl Read) -> Result<Option<Record>> {
    let (Some(off), Some(len)) = (read_u64(r)?, read_u32(r)?) else { return Ok(None) };
    if len > 64 * 1024 * 1024 {
        return Ok(None);
    }
    let mut data = vec![0u8; len as usize];
    if r.read_exact(&mut data).is_err() {
        return Ok(None);
    }
    let Some(sum) = read_u64(r)? else { return Ok(None) };
    Ok((sum == checksum(off, len, Some(&data))).then_some(Record::Backup(off, data)))
}

fn read_restored(r: &mut impl Read) -> Result<Option<Record>> {
    let (Some(off), Some(len), Some(sum)) = (read_u64(r)?, read_u32(r)?, read_u64(r)?) else { return Ok(None) };
    Ok((sum == checksum(off, len, None)).then_some(Record::Restored(off, len as usize)))
}

fn read_exact_opt<const N: usize>(r: &mut impl Read) -> Result<Option<[u8; N]>> {
    let mut b = [0u8; N];
    match r.read_exact(&mut b) {
        Ok(()) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn read_u16(r: &mut impl Read) -> Result<Option<u16>> {
    Ok(read_exact_opt::<2>(r)?.map(u16::from_le_bytes))
}
fn read_u32(r: &mut impl Read) -> Result<Option<u32>> {
    Ok(read_exact_opt::<4>(r)?.map(u32::from_le_bytes))
}
fn read_u64(r: &mut impl Read) -> Result<Option<u64>> {
    Ok(read_exact_opt::<8>(r)?.map(u64::from_le_bytes))
}

fn checksum(offset: u64, len: u32, data: Option<&[u8]>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    };
    offset.to_le_bytes().into_iter().for_each(&mut eat);
    len.to_le_bytes().into_iter().for_each(&mut eat);
    if let Some(d) = data {
        d.iter().copied().for_each(&mut eat);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{SimDevice, SimSpec};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("flashvalidator-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn complete_run_deletes_journal() {
        let dir = tmpdir("complete");
        let mut j = Journal::create(&dir, "dev", 1 << 20).unwrap();
        j.record_backup(4096, &[1; 4096]).unwrap();
        j.sync().unwrap();
        j.mark_restored(4096, 4096).unwrap();
        assert_eq!(j.finish().unwrap(), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupted_run_is_replayed_oldest_last() {
        let dir = tmpdir("replay");
        let mut dev = SimDevice::new(SimSpec::healthy(1 << 24));
        let id = dev.identity();
        let mut j = Journal::create(&dir, &id, dev.size_bytes()).unwrap();
        // Overlapping records: the first holds the true original.
        j.record_backup(8192, &[0xAA; 8192]).unwrap();
        j.record_backup(12288, &[0xBB; 4096]).unwrap();
        j.record_backup(0, &[0xCC; 4096]).unwrap();
        j.mark_restored(0, 4096).unwrap();
        let path = j.finish().unwrap().expect("journal kept");
        // Simulate a crash in the middle of writing the next record.
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[KIND_BACKUP, 1, 2, 3]).unwrap();
        }
        let parsed = read_journal(&path).unwrap();
        assert!(parsed.truncated_tail);
        assert_eq!(parsed.pending.len(), 2);

        dev.write_at(8192, &[0u8; 8192]).unwrap();
        assert_eq!(replay(&path, &mut dev, false).unwrap(), 2);
        let mut back = vec![0u8; 8192];
        dev.read_at(8192, &mut back).unwrap();
        assert!(back.iter().all(|&b| b == 0xAA));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_foreign_device() {
        let dir = tmpdir("foreign");
        let mut j = Journal::create(&dir, "other-device", 1 << 24).unwrap();
        j.record_backup(0, &[1; 4096]).unwrap();
        let path = j.finish().unwrap().unwrap();
        let mut dev = SimDevice::new(SimSpec::healthy(1 << 24));
        assert!(matches!(replay(&path, &mut dev, false), Err(Error::Journal(_))));
        assert_eq!(replay(&path, &mut dev, true).unwrap(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
