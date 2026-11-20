//! In-memory simulated drives (healthy and counterfeit), used by the tests.
//!
//! Storage is sparse: only 4 KiB blocks that were written use memory.
//! Unwritten blocks read back a fixed "factory" pattern derived from their
//! offset.

use crate::blockdev::BlockDevice;
use std::collections::HashMap;
use std::io;

const BLOCK: u64 = 4096;

/// How a simulated drive behaves past its real capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    /// A genuine drive: real capacity equals declared capacity.
    Healthy,
    /// Writes past the real capacity are silently dropped; reads return zeros.
    Discard,
    /// Addresses wrap modulo the real capacity (high blocks alias low ones).
    Wrap,
    /// Reads past the real capacity fail with an I/O error.
    ReadErrors,
    /// Reads past the real capacity return different garbage every time.
    Unstable,
}

/// Parameters of a simulated drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimSpec {
    /// Display name.
    pub name: String,
    /// Capacity the drive claims, in bytes.
    pub declared: u64,
    /// Capacity that really stores data, in bytes.
    pub real: u64,
    /// What happens past `real`.
    pub behavior: Behavior,
}

impl SimSpec {
    /// A genuine drive of `size` bytes.
    pub fn healthy(size: u64) -> Self {
        Self { name: "simulated healthy drive".into(), declared: size, real: size, behavior: Behavior::Healthy }
    }

    /// A counterfeit drive that claims `declared` bytes but stores `real`.
    pub fn fake(declared: u64, real: u64, behavior: Behavior) -> Self {
        Self { name: "simulated counterfeit drive".into(), declared, real: real.min(declared), behavior }
    }
}

/// A simulated block device.
#[derive(Debug)]
pub struct SimDevice {
    spec: SimSpec,
    blocks: HashMap<u64, Box<[u8]>>,
    reads: u64,
    writes: u64,
}

impl SimDevice {
    /// Creates a drive with "factory" contents.
    pub fn new(spec: SimSpec) -> Self {
        Self { spec, blocks: HashMap::new(), reads: 0, writes: 0 }
    }

    /// Number of read and write calls served so far.
    pub fn op_counts(&self) -> (u64, u64) {
        (self.reads, self.writes)
    }

    /// Hash over every block whose content differs from the factory content.
    /// Equal fingerprints before and after a run prove the run changed nothing.
    pub fn content_fingerprint(&self) -> u64 {
        let mut keys: Vec<_> = self.blocks.keys().copied().collect();
        keys.sort_unstable();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for k in keys {
            let data = &self.blocks[&k];
            if data.as_ref() == factory_block(k).as_slice() {
                continue;
            }
            for b in k.to_le_bytes().iter().chain(data.iter()) {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        h
    }

    /// Where a logical block is stored, or `None` if it is not stored at all.
    fn physical(&self, block_off: u64) -> Option<u64> {
        if block_off < self.spec.real {
            return Some(block_off);
        }
        match self.spec.behavior {
            Behavior::Healthy => Some(block_off),
            Behavior::Wrap if self.spec.real >= BLOCK => Some(block_off % (self.spec.real / BLOCK * BLOCK)),
            _ => None,
        }
    }

    fn check(&self, offset: u64, len: usize) -> io::Result<()> {
        if offset % BLOCK != 0 || len as u64 % BLOCK != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unaligned access"));
        }
        if offset + len as u64 > self.spec.declared {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "access past end of device"));
        }
        Ok(())
    }
}

fn factory_block(phys: u64) -> Vec<u8> {
    let mut rng = crate::rng::Xorshift128::from_seed(phys ^ 0xFAC7_0000);
    let mut v = vec![0u8; BLOCK as usize];
    rng.fill(&mut v);
    v
}

impl BlockDevice for SimDevice {
    fn size_bytes(&self) -> u64 {
        self.spec.declared
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.check(offset, buf.len())?;
        self.reads += 1;
        for (i, chunk) in buf.chunks_mut(BLOCK as usize).enumerate() {
            let logical = offset + i as u64 * BLOCK;
            match self.physical(logical) {
                Some(p) => match self.blocks.get(&p) {
                    Some(data) => chunk.copy_from_slice(data),
                    None => chunk.copy_from_slice(&factory_block(p)),
                },
                None => match self.spec.behavior {
                    Behavior::ReadErrors => return Err(io::Error::other("simulated media read error")),
                    Behavior::Unstable => {
                        crate::rng::Xorshift128::from_seed(logical ^ self.reads.wrapping_mul(0x9E37_79B9)).fill(chunk)
                    }
                    _ => chunk.fill(0),
                },
            }
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.check(offset, buf.len())?;
        self.writes += 1;
        for (i, chunk) in buf.chunks(BLOCK as usize).enumerate() {
            let logical = offset + i as u64 * BLOCK;
            if let Some(p) = self.physical(logical) {
                self.blocks.insert(p, chunk.to_vec().into_boxed_slice());
            }
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!("sim:{}:{}", self.spec.name, self.spec.declared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_round_trip() {
        let mut d = SimDevice::new(SimSpec::healthy(1 << 30));
        let data = vec![0xAB; 8192];
        d.write_at(4096, &data).unwrap();
        let mut back = vec![0; 8192];
        d.read_at(4096, &mut back).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn discard_drops_high_writes() {
        let mut d = SimDevice::new(SimSpec::fake(1 << 30, 1 << 20, Behavior::Discard));
        d.write_at(1 << 25, &[0xCD; 4096]).unwrap();
        let mut back = [1u8; 4096];
        d.read_at(1 << 25, &mut back).unwrap();
        assert_eq!(back, [0u8; 4096]);
    }

    #[test]
    fn wrap_aliases() {
        let mut d = SimDevice::new(SimSpec::fake(1 << 30, 1 << 20, Behavior::Wrap));
        d.write_at((1 << 20) + 4096, &[0xEE; 4096]).unwrap();
        let mut back = [0u8; 4096];
        d.read_at(4096, &mut back).unwrap();
        assert_eq!(back, [0xEE; 4096]);
    }

    #[test]
    fn unstable_reads_differ() {
        let mut d = SimDevice::new(SimSpec::fake(1 << 30, 1 << 20, Behavior::Unstable));
        let (mut a, mut b) = ([0u8; 4096], [0u8; 4096]);
        d.read_at(1 << 25, &mut a).unwrap();
        d.read_at(1 << 25, &mut b).unwrap();
        assert_ne!(a, b);
    }
}
