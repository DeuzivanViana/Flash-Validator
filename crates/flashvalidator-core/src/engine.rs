//! The validation algorithm.
//!
//! # Quick mode
//!
//! 1. **Pre-flight**: read the first 4 KiB (trying up to 32 consecutive
//!    blocks), then write the same bytes back. This catches dead and
//!    write-protected drives before anything is changed.
//! 2. **Calibration**: run the region test on the last block of the drive
//!    with transfer sizes from 4 KiB to 8 MiB. These operations are not
//!    included in the statistics.
//! 3. **Validation**: test the 576 regions: the last one first, then the
//!    first one, then the rest in random order. Each region is read (original
//!    saved), written with a random pattern, read back, compared, and
//!    restored.
//!
//! # Thorough mode
//!
//! The 576 regions are combined with *alias probes* at `0`, `2^k` and `3·2^k`
//! (k ≥ 20). A counterfeit controller that wraps addresses usually drops
//! high address lines, so its wrap period is a power of two and some probe
//! lands on the same physical block as another. All blocks are read
//! (backed up), then all are written with a tagged pattern, then all are
//! verified, then all are restored. A block that reads back another
//! block's tag shows aliasing. Every region above the detected wrap period
//! is then marked as "no storage".
//!
//! In both writing modes each block's original data is saved in the
//! [`Journal`] on the host *before* the drive is written.
//!
//! # Read-only mode (zero risk)
//!
//! Nothing is ever written, and the device should be opened with
//! [`Access::ReadOnly`](crate::blockdev::Access), so the OS itself refuses
//! writes. Every region and alias probe is read twice, in two different
//! random orders:
//!
//! - **read errors** are reported as such;
//! - **unstable reads** (the same block returns different data) are a
//!   classic sign of missing flash and are reported as "no storage";
//! - **aliasing**: identical non-blank blocks at offsets separated by a
//!   consistent power-of-two period show wrap-around (this needs data on
//!   the drive);
//! - blank areas (all bytes equal) are counted and reported, but a read-only
//!   check cannot tell whether that space really exists.
//!
//! Read-only mode can find failing and many counterfeit drives. It cannot
//! *prove* the capacity is real; only a writing mode can.

use crate::aligned::AlignedBuffer;
use crate::blockdev::{BlockDevice, FailureKind, classify};
use crate::error::{Error, Result};
use crate::journal::{self, Journal};
use crate::layout;
use crate::report::{self, CalibrationStep, DeviceSummary, Mode, RegionStatus, Report, StatusCounts};
use crate::rng::Xorshift128;
use crate::stats::Samples;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Number of consecutive blocks tried by the pre-flight check.
const PREFLIGHT_ATTEMPTS: u64 = 32;
/// Smallest calibration transfer.
const CALIBRATION_MIN: usize = 4096;
/// Largest calibration transfer.
const CALIBRATION_MAX: usize = 8 * 1024 * 1024;
/// Smallest alias-probe exponent (1 MiB).
const PROBE_MIN_SHIFT: u32 = 20;
/// Magic at the start of every thorough-mode pattern block.
const PATTERN_MAGIC: &[u8; 8] = b"FLVALv01";

/// Where to keep the backup journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalSetting {
    /// Do not journal (not recommended).
    Disabled,
    /// Create the journal in this directory.
    Directory(PathBuf),
}

/// Run configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Test mode.
    pub mode: Mode,
    /// Number of regions (default 576).
    pub regions: usize,
    /// Transfer size per region; must be a multiple of 4096 (default 4096).
    pub block_size: usize,
    /// Run the calibration phase.
    pub calibrate: bool,
    /// Journal location.
    pub journal: JournalSetting,
    /// Fixed RNG seed, for reproducible tests. `None` uses OS entropy.
    pub seed: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Quick,
            regions: layout::DEFAULT_REGIONS,
            block_size: layout::DEFAULT_BLOCK_SIZE,
            calibrate: true,
            journal: JournalSetting::Directory(journal::default_dir()),
            seed: None,
        }
    }
}

/// Coarse phase of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Checking the first sectors.
    Preflight,
    /// Read-only mode: first read of every block.
    Reading,
    /// Read-only mode: second read, checking stability.
    Rereading,
    /// Warm-up with growing transfer sizes.
    Calibrating,
    /// Quick mode: testing regions one by one.
    Validating,
    /// Thorough mode: reading the original data.
    Backup,
    /// Thorough mode: writing patterns.
    Writing,
    /// Thorough mode: verifying patterns.
    Verifying,
    /// Thorough mode: writing the original data back.
    Restoring,
    /// Done.
    Finished,
}

impl Phase {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Preflight => "Checking first sectors",
            Self::Reading => "Reading regions",
            Self::Rereading => "Re-reading to check stability",
            Self::Calibrating => "Calibrating",
            Self::Validating => "Validating",
            Self::Backup => "Backing up regions",
            Self::Writing => "Writing patterns",
            Self::Verifying => "Verifying patterns",
            Self::Restoring => "Restoring original data",
            Self::Finished => "Finished",
        }
    }
}

/// What the drive is doing right now (for an activity indicator).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activity {
    /// No I/O in flight.
    Idle,
    /// Reading.
    Reading,
    /// Writing.
    Writing,
}

/// Progress notification sent to the observer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// The run started; gives the region layout.
    Started {
        /// Byte offset of each region.
        offsets: Vec<u64>,
        /// Transfer size per region.
        block_size: usize,
        /// Declared capacity.
        declared_size: u64,
    },
    /// Entering a new phase.
    Phase(Phase),
    /// The drive's I/O activity changed.
    Activity(Activity),
    /// A calibration step finished.
    Calibration {
        /// 1-based step number.
        step: usize,
        /// Number of planned steps.
        total: usize,
        /// The step's result.
        result: CalibrationStep,
    },
    /// A region's status changed.
    Region {
        /// Region index.
        index: usize,
        /// New status.
        status: RegionStatus,
    },
    /// Overall progress.
    Progress {
        /// Completed work units.
        done: usize,
        /// Total work units.
        total: usize,
    },
    /// A non-fatal problem.
    Warning(String),
}

/// A handle that can stop a running [`Validator`] from another thread.
///
/// The run stops at the next safe point. Data already written is always
/// restored first.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Asks the run to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether a stop was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Runs a validation on one device.
pub struct Validator {
    dev: Box<dyn BlockDevice>,
    cfg: Config,
    device: DeviceSummary,
    cancel: CancelToken,
}

impl std::fmt::Debug for Validator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Validator").field("device", &self.device).field("cfg", &self.cfg).finish_non_exhaustive()
    }
}

/// Why a run stopped early.
enum Stop {
    /// The device disappeared.
    Gone,
    /// The host-side journal failed. Writing more would be unsafe.
    Fatal(Error),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Self {
        Stop::Fatal(e)
    }
}

type StepResult<T> = std::result::Result<T, Stop>;

impl Validator {
    /// Creates a validator for `dev`.
    pub fn new(dev: Box<dyn BlockDevice>, cfg: Config) -> Self {
        let device = DeviceSummary { path: dev.identity(), ..DeviceSummary::default() };
        Self { dev, cfg, device, cancel: CancelToken::default() }
    }

    /// Sets the device information shown in the report.
    pub fn with_device_summary(mut self, device: DeviceSummary) -> Self {
        self.device = device;
        self
    }

    /// Returns a token that cancels this run.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Uses an existing token, so a run can be cancelled before it is created.
    pub fn with_cancel_token(mut self, token: CancelToken) -> Self {
        self.cancel = token;
        self
    }

    /// Runs the validation, sending progress events to `observer`.
    ///
    /// Problems found before anything is written (dead drive, write
    /// protection, too small, journal not creatable) return `Err`. Once
    /// testing has started, the result is always a [`Report`], even if it
    /// was cancelled or the drive was removed. `Report::completed` says
    /// whether every region was tested. The one exception is a failing
    /// journal, which stops the run for safety and returns
    /// [`Error::Journal`].
    pub fn run(mut self, observer: &mut dyn FnMut(Event)) -> Result<Report> {
        let started = Instant::now();
        let started_unix =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        let cfg = self.cfg.clone();
        validate_config(&cfg)?;
        let size = self.dev.size_bytes();
        if size == 0 {
            return Err(Error::NoStorage);
        }
        if !layout::fits(size, cfg.regions, cfg.block_size) {
            return Err(Error::TooSmall { size, regions: cfg.regions, block: cfg.block_size });
        }

        let offsets = layout::region_offsets(cfg.regions, size, cfg.block_size);
        observer(Event::Started { offsets: offsets.clone(), block_size: cfg.block_size, declared_size: size });

        let writes = cfg.mode.writes();
        let calibrate = cfg.calibrate && writes;
        let cal_max = if calibrate { calibration_sizes(size, cfg.regions).last().copied().unwrap_or(0) } else { 0 };
        let buf_len = cfg.block_size.max(cal_max).max(CALIBRATION_MIN);
        let mut run = Run {
            dev: self.dev.as_mut(),
            observer,
            cancel: &self.cancel,
            journal: None,
            rng: cfg.seed.map_or_else(Xorshift128::from_entropy, Xorshift128::from_seed),
            original: AlignedBuffer::new(buf_len),
            pattern: AlignedBuffer::new(buf_len),
            readback: AlignedBuffer::new(buf_len),
            reads: Samples::default(),
            writes: Samples::default(),
            unrestored: Vec::new(),
            warnings: Vec::new(),
            statuses: vec![RegionStatus::Pending; cfg.regions],
            wraparound: false,
        };

        (run.observer)(Event::Phase(Phase::Preflight));
        run.preflight(writes)?;

        if writes {
            if let JournalSetting::Directory(dir) = &cfg.journal {
                run.journal = Some(Journal::create(dir, &run.dev.identity(), size)?);
            }
        }

        let mut calibration = Vec::new();
        let mut stopped: Option<Stop> = None;
        if calibrate {
            (run.observer)(Event::Phase(Phase::Calibrating));
            if let Err(stop) = run.calibrate(&mut calibration) {
                stopped = Some(stop);
            }
        }
        if stopped.is_none() {
            let result = match cfg.mode {
                Mode::ReadOnly => run.read_only(&offsets, cfg.block_size),
                Mode::Quick => run.quick(&offsets, cfg.block_size),
                Mode::Thorough => run.thorough(&offsets, cfg.block_size),
            };
            if let Err(stop) = result {
                stopped = Some(stop);
            }
        }
        (run.observer)(Event::Activity(Activity::Idle));
        if !matches!(stopped, Some(Stop::Gone)) {
            if let Err(e) = run.dev.flush() {
                run.warnings.push(format!("final flush failed: {e}"));
            }
        }

        let mut fatal = None;
        match stopped {
            Some(Stop::Gone) => run.warnings.push(Error::DeviceRemoved.to_string()),
            Some(Stop::Fatal(e)) => fatal = Some(e),
            None => {}
        }
        if run.cancel.is_cancelled() {
            run.warnings.push("validation was stopped by the user".into());
        }
        let journal_path = match run.journal.take() {
            Some(j) => j.finish()?,
            None => None,
        };
        if let Some(e) = fatal {
            return Err(Error::Journal(format!(
                "{e}; testing was stopped for safety{}",
                journal_path.map(|p| format!(" (journal kept at {})", p.display())).unwrap_or_default()
            )));
        }

        let statuses = run.statuses;
        let completed = !statuses.contains(&RegionStatus::Pending);
        let validated = report::validated_size(&statuses, &offsets, cfg.block_size, size);
        let highest = statuses.iter().rposition(|s| s.is_good());
        (run.observer)(Event::Phase(Phase::Finished));
        Ok(Report {
            tool: format!("FlashValidator {}", crate::VERSION),
            started_at_unix: started_unix,
            duration_secs: started.elapsed().as_secs_f64(),
            device: self.device,
            mode: cfg.mode,
            block_size: cfg.block_size,
            declared_size: size,
            counts: StatusCounts::from_statuses(&statuses),
            validated_size: validated,
            highest_valid_region: highest,
            highest_valid_end: highest.map_or(0, |r| report::region_end(&offsets, r, cfg.block_size, size)),
            wraparound_detected: run.wraparound,
            completed,
            verdict: report::verdict(&statuses, completed, validated, cfg.mode),
            calibration,
            read_stats: run.reads.summary(),
            write_stats: run.writes.summary(),
            unrestored_offsets: run.unrestored,
            journal: journal_path,
            warnings: run.warnings,
            region_offsets: offsets,
            region_status: statuses,
        })
    }
}

fn validate_config(cfg: &Config) -> Result<()> {
    if cfg.regions < 2 {
        return Err(Error::InvalidConfig("at least two regions are required".into()));
    }
    if cfg.regions > u32::MAX as usize {
        return Err(Error::InvalidConfig("too many regions".into()));
    }
    if cfg.block_size == 0 || cfg.block_size % crate::aligned::ALIGNMENT != 0 || cfg.block_size > CALIBRATION_MAX {
        return Err(Error::InvalidConfig(format!(
            "block size must be a multiple of {} up to {CALIBRATION_MAX}",
            crate::aligned::ALIGNMENT
        )));
    }
    Ok(())
}

/// Calibration transfer sizes that leave room for `regions` blocks.
fn calibration_sizes(device_size: u64, regions: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut s = CALIBRATION_MIN;
    while s <= CALIBRATION_MAX && layout::fits(device_size, regions, s) {
        sizes.push(s);
        s *= 2;
    }
    sizes
}

/// One block tested in thorough or read-only mode.
#[derive(Debug, Clone, Copy)]
struct Probe {
    offset: u64,
    /// Region index, or `None` for an extra alias probe.
    region: Option<usize>,
}

/// Region blocks plus alias probes at `2^k` and `3·2^k`.
fn build_probes(offsets: &[u64], size: u64, block: usize) -> Vec<Probe> {
    let mut probes: Vec<Probe> =
        offsets.iter().enumerate().map(|(i, &offset)| Probe { offset, region: Some(i) }).collect();
    let mut extra = Vec::new();
    for shift in PROBE_MIN_SHIFT..64 {
        let base = 1u64 << shift;
        if base >= size {
            break;
        }
        for mult in [1u64, 3] {
            if let Some(o) = base.checked_mul(mult) {
                if o + block as u64 <= size {
                    extra.push(o);
                }
            }
        }
    }
    extra.sort_unstable();
    extra.dedup();
    probes.extend(
        extra.into_iter().filter(|o| offsets.binary_search(o).is_err()).map(|offset| Probe { offset, region: None }),
    );
    probes
}

struct Run<'a> {
    dev: &'a mut dyn BlockDevice,
    observer: &'a mut dyn FnMut(Event),
    cancel: &'a CancelToken,
    journal: Option<Journal>,
    rng: Xorshift128,
    original: AlignedBuffer,
    pattern: AlignedBuffer,
    readback: AlignedBuffer,
    reads: Samples,
    writes: Samples,
    unrestored: Vec<u64>,
    warnings: Vec<String>,
    statuses: Vec<RegionStatus>,
    wraparound: bool,
}

/// Selects one of the run's three buffers.
#[derive(Clone, Copy)]
enum Buf {
    Original,
    Pattern,
    Readback,
}

/// Per-probe bookkeeping for thorough mode.
struct ProbeState {
    original: Option<Vec<u8>>,
    written: bool,
    status: RegionStatus,
    /// Probe index found in the read-back tag, if it came from this run.
    tag: Option<usize>,
}

impl Run<'_> {
    fn emit(&mut self, e: Event) {
        (self.observer)(e);
    }

    fn set_status(&mut self, index: usize, status: RegionStatus) {
        if self.statuses[index] != status {
            self.statuses[index] = status;
            self.emit(Event::Region { index, status });
        }
    }

    /// Timed read into one of the buffers.
    fn read(&mut self, offset: u64, len: usize, into: Buf, timed: bool) -> io::Result<()> {
        self.emit(Event::Activity(Activity::Reading));
        let t = Instant::now();
        let res = match into {
            Buf::Original => self.dev.read_at(offset, &mut self.original[..len]),
            Buf::Pattern => self.dev.read_at(offset, &mut self.pattern[..len]),
            Buf::Readback => self.dev.read_at(offset, &mut self.readback[..len]),
        };
        if res.is_ok() && timed {
            self.reads.push(t.elapsed());
        }
        res
    }

    /// Timed write from one of the buffers.
    fn write(&mut self, offset: u64, len: usize, from: Buf, timed: bool) -> io::Result<()> {
        self.emit(Event::Activity(Activity::Writing));
        let t = Instant::now();
        let res = match from {
            Buf::Original => self.dev.write_at(offset, &self.original[..len]),
            Buf::Pattern => self.dev.write_at(offset, &self.pattern[..len]),
            Buf::Readback => self.dev.write_at(offset, &self.readback[..len]),
        };
        if res.is_ok() && timed {
            self.writes.push(t.elapsed());
        }
        res
    }

    fn gone(err: &io::Error) -> bool {
        classify(err) == FailureKind::DeviceGone
    }

    /// Pre-flight check: can the first sectors be read (and rewritten)?
    fn preflight(&mut self, write: bool) -> Result<()> {
        let len = CALIBRATION_MIN;
        let mut readable = None;
        for k in 0..PREFLIGHT_ATTEMPTS {
            match self.read(k * len as u64, len, Buf::Original, false) {
                Ok(()) => {
                    readable = Some(k);
                    break;
                }
                Err(e) if Self::gone(&e) => return Err(Error::DeviceRemoved),
                Err(_) => {}
            }
        }
        let Some(first) = readable else { return Err(Error::ReadFailure) };
        if !write {
            self.emit(Event::Activity(Activity::Idle));
            return Ok(());
        }
        for k in first..PREFLIGHT_ATTEMPTS {
            let off = k * len as u64;
            if k != first && self.read(off, len, Buf::Original, false).is_err() {
                continue;
            }
            match self.write(off, len, Buf::Original, false) {
                Ok(()) => {
                    self.emit(Event::Activity(Activity::Idle));
                    return Ok(());
                }
                Err(e) => match classify(&e) {
                    FailureKind::WriteProtected => return Err(Error::WriteProtected),
                    FailureKind::DeviceGone => return Err(Error::DeviceRemoved),
                    FailureKind::Other if e.kind() == io::ErrorKind::PermissionDenied => {
                        return Err(Error::WriteProtected);
                    }
                    FailureKind::Other => {}
                },
            }
        }
        Err(Error::WriteFailure)
    }

    fn journal_backup(&mut self, offset: u64, len: usize, sync: bool) -> StepResult<()> {
        if let Some(j) = self.journal.as_mut() {
            j.record_backup(offset, &self.original[..len])?;
            if sync {
                j.sync()?;
            }
        }
        Ok(())
    }

    fn journal_restored(&mut self, offset: u64, len: usize) -> StepResult<()> {
        if let Some(j) = self.journal.as_mut() {
            j.mark_restored(offset, len)?;
        }
        Ok(())
    }

    /// Fills the pattern buffer with random data that differs from the original.
    fn random_pattern(&mut self, len: usize) {
        loop {
            self.rng.fill(&mut self.pattern[..len]);
            if self.pattern[..len] != self.original[..len] {
                break;
            }
        }
    }

    /// Per-region test: read, write pattern, read back, compare, restore.
    fn test_region_quick(&mut self, offset: u64, len: usize, timed: bool) -> StepResult<RegionStatus> {
        if let Err(e) = self.read(offset, len, Buf::Original, timed) {
            self.emit(Event::Activity(Activity::Idle));
            return if Self::gone(&e) { Err(Stop::Gone) } else { Ok(RegionStatus::ReadError) };
        }
        self.random_pattern(len);
        self.journal_backup(offset, len, true)?;

        let mut status = RegionStatus::Ok;
        match self.write(offset, len, Buf::Pattern, timed) {
            Err(e) if Self::gone(&e) => return Err(Stop::Gone),
            Err(_) => status = RegionStatus::WriteError,
            Ok(()) => match self.read(offset, len, Buf::Readback, timed) {
                Err(e) if Self::gone(&e) => return Err(Stop::Gone),
                Err(_) => status = RegionStatus::ReadError,
                Ok(()) => {
                    if self.readback[..len] != self.pattern[..len] {
                        status = RegionStatus::Mismatch;
                    }
                }
            },
        }

        // Restore. A failed restore turns OK/read-error into write-error.
        match self.write(offset, len, Buf::Original, timed) {
            Err(e) if Self::gone(&e) => return Err(Stop::Gone),
            Err(_) => {
                self.unrestored.push(offset);
                if status < RegionStatus::WriteError {
                    status = RegionStatus::WriteError;
                }
            }
            Ok(()) => self.journal_restored(offset, len)?,
        }
        self.emit(Event::Activity(Activity::Idle));
        Ok(status)
    }

    /// Warm-up at the end of the drive with growing transfer sizes.
    fn calibrate(&mut self, steps: &mut Vec<CalibrationStep>) -> StepResult<()> {
        let size = self.dev.size_bytes();
        let sizes = calibration_sizes(size, self.statuses.len());
        let total = sizes.len();
        for (i, &bs) in sizes.iter().enumerate() {
            if self.cancel.is_cancelled() {
                break;
            }
            let offset = (size - bs as u64) & !(layout::OFFSET_ALIGN - 1);
            let status = self.test_region_quick(offset, bs, false)?;
            let result = CalibrationStep { block_size: bs, status };
            steps.push(result);
            self.emit(Event::Calibration { step: i + 1, total, result });
            if status != RegionStatus::Ok {
                self.warnings.push(format!(
                    "calibration with {} transfers ended with '{}' at the end of the drive",
                    crate::format::human_bytes_binary(bs as u64),
                    status.label()
                ));
                break;
            }
        }
        Ok(())
    }

    fn quick(&mut self, offsets: &[u64], block: usize) -> StepResult<()> {
        self.emit(Event::Phase(Phase::Validating));
        let order = layout::test_order(offsets.len(), &mut self.rng);
        let total = order.len();
        for (done, &region) in order.iter().enumerate() {
            if self.cancel.is_cancelled() {
                break;
            }
            let status = self.test_region_quick(offsets[region], block, true)?;
            self.set_status(region, status);
            self.emit(Event::Progress { done: done + 1, total });
        }
        Ok(())
    }

    fn thorough(&mut self, offsets: &[u64], block: usize) -> StepResult<()> {
        let size = self.dev.size_bytes();
        let probes = build_probes(offsets, size, block);
        let mut order: Vec<usize> = (0..probes.len()).collect();
        for i in (1..order.len()).rev() {
            let j = self.rng.below(i + 1);
            order.swap(i, j);
        }
        let nonce = self.rng.next_u64();
        let total = 4 * probes.len();
        let mut done = 0;
        let mut st: Vec<ProbeState> = probes
            .iter()
            .map(|_| ProbeState { original: None, written: false, status: RegionStatus::Pending, tag: None })
            .collect();

        let result = self.thorough_passes(&probes, &order, nonce, block, &mut st, &mut done, total);
        let restore = if matches!(result, Err(Stop::Gone)) {
            Ok(()) // nothing can be written; the journal keeps the data
        } else {
            self.restore_all(&probes, &order, block, &mut st, &mut done, total)
        };
        if result.is_ok() && restore.is_ok() && !self.cancel.is_cancelled() {
            self.analyse_aliases(&probes, &mut st);
        }
        for (p, s) in probes.iter().zip(&st) {
            if let Some(r) = p.region {
                if s.status != RegionStatus::Pending || self.statuses[r] != RegionStatus::Pending {
                    self.set_status(r, s.status);
                }
            }
        }
        result.and(restore)
    }

    /// Backup, write and verify passes.
    #[allow(clippy::too_many_arguments)]
    fn thorough_passes(
        &mut self,
        probes: &[Probe],
        order: &[usize],
        nonce: u64,
        block: usize,
        st: &mut [ProbeState],
        done: &mut usize,
        total: usize,
    ) -> StepResult<()> {
        let progress = |run: &mut Self, done: &mut usize| {
            *done += 1;
            run.emit(Event::Progress { done: *done, total });
        };
        let publish = |run: &mut Self, p: &Probe, status: RegionStatus| {
            if let Some(r) = p.region {
                run.set_status(r, status);
            }
        };

        self.emit(Event::Phase(Phase::Backup));
        for &i in order {
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            match self.read(probes[i].offset, block, Buf::Original, true) {
                Ok(()) => {
                    self.journal_backup(probes[i].offset, block, false)?;
                    st[i].original = Some(self.original[..block].to_vec());
                }
                Err(e) if Self::gone(&e) => return Err(Stop::Gone),
                Err(_) => {
                    st[i].status = RegionStatus::ReadError;
                    publish(self, &probes[i], RegionStatus::ReadError);
                }
            }
            progress(self, done);
        }
        if let Some(j) = self.journal.as_mut() {
            j.sync()?;
        }

        self.emit(Event::Phase(Phase::Writing));
        for &i in order {
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            if st[i].original.is_some() {
                fill_tagged(&mut self.pattern[..block], nonce, i, probes[i].offset);
                st[i].written = true;
                match self.write(probes[i].offset, block, Buf::Pattern, true) {
                    Ok(()) => {}
                    Err(e) if Self::gone(&e) => return Err(Stop::Gone),
                    Err(_) => {
                        st[i].status = RegionStatus::WriteError;
                        publish(self, &probes[i], RegionStatus::WriteError);
                    }
                }
            }
            progress(self, done);
        }
        if let Err(e) = self.dev.flush() {
            if Self::gone(&e) {
                return Err(Stop::Gone);
            }
            self.warnings.push(format!("flush after writing patterns failed: {e}"));
        }

        self.emit(Event::Phase(Phase::Verifying));
        for &i in order {
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            if st[i].written && st[i].status == RegionStatus::Pending {
                match self.read(probes[i].offset, block, Buf::Readback, true) {
                    Err(e) if Self::gone(&e) => return Err(Stop::Gone),
                    Err(_) => st[i].status = RegionStatus::ReadError,
                    Ok(()) => {
                        fill_tagged(&mut self.pattern[..block], nonce, i, probes[i].offset);
                        st[i].tag = tag_index(&self.readback[..block], nonce).filter(|&t| t < probes.len());
                        st[i].status = if self.readback[..block] == self.pattern[..block] {
                            RegionStatus::Ok
                        } else {
                            RegionStatus::Mismatch
                        };
                    }
                }
                let status = st[i].status;
                publish(self, &probes[i], status);
            }
            progress(self, done);
        }
        Ok(())
    }

    /// Writes back every original that was overwritten (or might have been).
    fn restore_all(
        &mut self,
        probes: &[Probe],
        order: &[usize],
        block: usize,
        st: &mut [ProbeState],
        done: &mut usize,
        total: usize,
    ) -> StepResult<()> {
        self.emit(Event::Phase(Phase::Restoring));
        for &i in order {
            let Some(orig) = st[i].original.take() else { continue };
            if st[i].written {
                self.original[..block].copy_from_slice(&orig);
                match self.write(probes[i].offset, block, Buf::Original, true) {
                    Err(e) if Self::gone(&e) => return Err(Stop::Gone),
                    Err(_) => {
                        self.unrestored.push(probes[i].offset);
                        if st[i].status < RegionStatus::WriteError {
                            st[i].status = RegionStatus::WriteError;
                        }
                        continue;
                    }
                    Ok(()) => {}
                }
            }
            self.journal_restored(probes[i].offset, block)?;
            *done += 1;
            self.emit(Event::Progress { done: (*done).min(total), total });
        }
        Ok(())
    }

    /// Read-only mode: two read passes, stability check, aliasing and blank analysis.
    fn read_only(&mut self, offsets: &[u64], block: usize) -> StepResult<()> {
        let size = self.dev.size_bytes();
        let probes = build_probes(offsets, size, block);
        let n = probes.len();
        let total = 2 * n;
        let mut done = 0;
        let mut first: Vec<Option<(u64, bool)>> = vec![None; n]; // (hash, blank)
        let mut status = vec![RegionStatus::Pending; n];

        for (pass, phase) in [(0, Phase::Reading), (1, Phase::Rereading)] {
            self.emit(Event::Phase(phase));
            // A fresh random order per pass, so the drive cannot serve the
            // second read of a block straight from its own small cache.
            let mut order: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = self.rng.below(i + 1);
                order.swap(i, j);
            }
            for &i in &order {
                if self.cancel.is_cancelled() {
                    return Ok(());
                }
                if status[i] == RegionStatus::ReadError {
                    done += 1;
                    continue;
                }
                match self.read(probes[i].offset, block, Buf::Readback, true) {
                    Err(e) if Self::gone(&e) => return Err(Stop::Gone),
                    Err(_) => status[i] = RegionStatus::ReadError,
                    Ok(()) => {
                        let data = &self.readback[..block];
                        let h = fnv1a(data);
                        if pass == 0 {
                            first[i] = Some((h, data.iter().all(|&b| b == data[0])));
                        } else if first[i].is_some_and(|(h1, _)| h1 != h) {
                            status[i] = RegionStatus::Mismatch; // unstable read
                        } else {
                            status[i] = RegionStatus::Readable;
                        }
                    }
                }
                if pass == 1 || status[i] == RegionStatus::ReadError {
                    if let Some(r) = probes[i].region {
                        self.set_status(r, status[i]);
                    }
                }
                done += 1;
                self.emit(Event::Progress { done, total });
            }
        }
        self.emit(Event::Activity(Activity::Idle));

        let unstable = status.iter().filter(|s| **s == RegionStatus::Mismatch).count();
        if unstable > 0 {
            self.warnings.push(format!(
                "{unstable} block(s) returned different data on two reads; real flash memory does not do that"
            ));
        }

        // Aliasing: identical non-blank blocks at different offsets.
        let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
        for i in 0..n {
            if let (RegionStatus::Readable, Some((h, false))) = (status[i], first[i]) {
                by_hash.entry(h).or_default().push(i);
            }
        }
        let mut periods: HashMap<u64, usize> = HashMap::new();
        let mut groups = Vec::new();
        for members in by_hash.into_values().filter(|m| m.len() > 1) {
            let lowest = *members.iter().min_by_key(|&&i| probes[i].offset).expect("non-empty");
            let period = members
                .iter()
                .filter(|&&m| m != lowest)
                .fold(0, |g, &m| gcd(g, probes[m].offset - probes[lowest].offset));
            *periods.entry(period).or_default() += 1;
            groups.push((members, lowest, period));
        }
        // Accept a period only with corroboration: two groups agreeing, or one
        // group of three or more. Identical blocks do occur on genuine drives.
        let confirmed = groups
            .iter()
            .filter(|(m, _, p)| periods[p] >= 2 || m.len() >= 3)
            .map(|(_, _, p)| *p)
            .filter(|p| *p >= 1 << PROBE_MIN_SHIFT)
            .min();
        if let Some(period) = confirmed {
            self.wraparound = true;
            for (p, s) in probes.iter().zip(status.iter_mut()) {
                if p.offset >= period && *s == RegionStatus::Readable {
                    *s = RegionStatus::Mismatch;
                }
            }
            self.warnings.push(format!(
                "the same data appears every {} ({} bytes): the drive wraps addresses and storage above that is not real",
                crate::format::human_bytes_binary(period),
                crate::format::grouped(period)
            ));
        } else if !groups.is_empty() {
            self.warnings.push(format!(
                "{} set(s) of identical blocks were found at different offsets; this can be normal (duplicated data) \
                 but may also indicate address wrap-around. Use Thorough mode to be sure",
                groups.len()
            ));
        }

        // Blank tail: everything after the last block that holds data is blank.
        let region_blank =
            |r: usize| probes.iter().position(|p| p.region == Some(r)).and_then(|i| first[i]).map(|f| f.1);
        let regions = offsets.len();
        if let Some(last_data) = (0..regions).rev().find(|&r| region_blank(r) == Some(false)) {
            let blank_after = (last_data + 1..regions).filter(|&r| region_blank(r) == Some(true)).count();
            if blank_after > 0 && last_data + 1 < regions {
                self.warnings.push(format!(
                    "the drive holds no data beyond {} ({} blank region(s) up to the end); a read-only check cannot \
                     tell whether that space is real",
                    crate::format::human_bytes(offsets[last_data] + block as u64),
                    blank_after
                ));
            }
        } else {
            self.warnings.push(
                "every region is blank (the drive looks empty); a read-only check cannot tell whether that space \
                 is real"
                    .into(),
            );
        }

        for (p, s) in probes.iter().zip(&status) {
            if let Some(r) = p.region {
                self.set_status(r, *s);
            }
        }
        Ok(())
    }

    /// Groups probes that share physical storage and marks the aliases.
    ///
    /// When probe A reads back probe B's tag, A and B are the same physical
    /// block. In each group the lowest offset is the real location and the
    /// others are aliases. The greatest common divisor of the offset
    /// differences is the wrap period, and every region at or above it is an
    /// alias as well.
    fn analyse_aliases(&mut self, probes: &[Probe], st: &mut [ProbeState]) {
        let n = probes.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        let mut any = false;
        for (i, s) in st.iter().enumerate() {
            if let Some(t) = s.tag {
                if t != i {
                    any = true;
                    let (a, b) = (find(&mut parent, i), find(&mut parent, t));
                    parent[a] = b;
                }
            }
        }
        if !any {
            return;
        }
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let root = find(&mut parent, i);
            groups.entry(root).or_default().push(i);
        }
        let mut period: u64 = 0;
        for members in groups.values().filter(|m| m.len() > 1) {
            let lowest = *members.iter().min_by_key(|&&i| probes[i].offset).expect("non-empty");
            for &m in members {
                if m == lowest {
                    // Real storage: it holds a valid tag from this run.
                    if st[m].status == RegionStatus::Mismatch && st[m].tag.is_some() {
                        st[m].status = RegionStatus::Ok;
                    }
                } else {
                    period = gcd(period, probes[m].offset - probes[lowest].offset);
                    if st[m].status == RegionStatus::Ok || st[m].status == RegionStatus::Mismatch {
                        st[m].status = RegionStatus::Mismatch;
                    }
                }
            }
        }
        self.wraparound = true;
        if period > 0 {
            for (p, s) in probes.iter().zip(st.iter_mut()) {
                if p.offset >= period && s.status == RegionStatus::Ok {
                    s.status = RegionStatus::Mismatch;
                }
            }
            self.warnings.push(format!(
                "address wrap-around detected: the drive repeats at least every {} ({} bytes); storage above that is not real",
                crate::format::human_bytes_binary(period),
                crate::format::grouped(period)
            ));
        }
    }
}

/// 64-bit FNV-1a hash of a block (for comparing reads; not cryptographic).
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for chunk in data.chunks_exact(8) {
        h ^= u64::from_le_bytes(chunk.try_into().expect("8 bytes"));
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Fills `buf` with the thorough-mode pattern for probe `index`: a 32-byte
/// tag followed by pseudo-random data derived from (nonce, index).
fn fill_tagged(buf: &mut [u8], nonce: u64, index: usize, offset: u64) {
    let mut rng = Xorshift128::from_seed(nonce ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    rng.fill(buf);
    buf[0..8].copy_from_slice(PATTERN_MAGIC);
    buf[8..16].copy_from_slice(&nonce.to_le_bytes());
    buf[16..24].copy_from_slice(&(index as u64).to_le_bytes());
    buf[24..32].copy_from_slice(&offset.to_le_bytes());
}

/// If `buf` starts with a tag from this run, returns the probe it was written for.
fn tag_index(buf: &[u8], nonce: u64) -> Option<usize> {
    (buf.len() >= 32 && &buf[0..8] == PATTERN_MAGIC && buf[8..16] == nonce.to_le_bytes())
        .then(|| u64::from_le_bytes(buf[16..24].try_into().expect("8 bytes")) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Verdict;
    use crate::sim::{Behavior, SimDevice, SimSpec};
    use std::sync::{Arc, Mutex};

    const MB: u64 = 1_000_000;
    const MIB: u64 = 1 << 20;

    /// A device wrapper that shares the simulated drive with the test.
    struct Shared(Arc<Mutex<SimDevice>>);
    impl BlockDevice for Shared {
        fn size_bytes(&self) -> u64 {
            self.0.lock().unwrap().size_bytes()
        }
        fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<()> {
            self.0.lock().unwrap().read_at(o, b)
        }
        fn write_at(&mut self, o: u64, b: &[u8]) -> io::Result<()> {
            self.0.lock().unwrap().write_at(o, b)
        }
        fn identity(&self) -> String {
            self.0.lock().unwrap().identity()
        }
    }

    fn cfg(mode: Mode) -> Config {
        Config { mode, journal: JournalSetting::Disabled, seed: Some(1234), ..Config::default() }
    }

    fn pristine() -> u64 {
        SimDevice::new(SimSpec::healthy(1)).content_fingerprint()
    }

    fn run(spec: SimSpec, cfg: Config) -> (Report, Arc<Mutex<SimDevice>>, Vec<Event>) {
        let dev = Arc::new(Mutex::new(SimDevice::new(spec)));
        let v = Validator::new(Box::new(Shared(dev.clone())), cfg);
        let mut events = Vec::new();
        let report = v.run(&mut |e| events.push(e)).expect("run");
        (report, dev, events)
    }

    #[test]
    fn healthy_drive_passes_and_is_unchanged() {
        for mode in [Mode::Quick, Mode::Thorough] {
            let (r, dev, events) = run(SimSpec::healthy(512 * MB), cfg(mode));
            assert_eq!(r.verdict, Verdict::Pass, "{mode}");
            assert_eq!(r.counts.ok, 576);
            assert_eq!(r.validated_size, 512 * MB);
            assert_eq!(r.highest_valid_region, Some(575));
            assert!(r.unrestored_offsets.is_empty());
            assert!(!r.wraparound_detected);
            assert_eq!(dev.lock().unwrap().content_fingerprint(), pristine(), "{mode}: data changed");
            assert!(events.iter().any(|e| matches!(e, Event::Phase(Phase::Finished))));
            assert!(r.read_stats.unwrap().samples >= 2 * 576, "{mode}: at least two reads per region");
        }
    }

    #[test]
    fn discarding_fake_is_detected_with_real_capacity() {
        for mode in [Mode::Quick, Mode::Thorough] {
            let (r, dev, _) = run(SimSpec::fake(1000 * MB, 250 * MB, Behavior::Discard), cfg(mode));
            match r.verdict {
                Verdict::Counterfeit { validated_size } => {
                    assert!(validated_size <= 250 * MB && validated_size > 240 * MB, "{mode}: {validated_size}")
                }
                v => panic!("{mode}: unexpected verdict {v:?}"),
            }
            assert!(r.counts.mismatches > 400);
            assert_eq!(dev.lock().unwrap().content_fingerprint(), pristine(), "{mode}: data changed");
        }
    }

    #[test]
    fn wrapping_fake_needs_thorough_mode() {
        let spec = SimSpec::fake(1000 * MB, 128 * MIB, Behavior::Wrap);
        let (quick, _, _) = run(spec.clone(), cfg(Mode::Quick));
        assert_eq!(quick.verdict, Verdict::Pass, "documented limitation of the per-region method");

        let (thorough, dev, _) = run(spec, cfg(Mode::Thorough));
        assert!(matches!(thorough.verdict, Verdict::Counterfeit { .. }), "{:?}", thorough.verdict);
        assert!(thorough.wraparound_detected);
        assert!(thorough.validated_size <= 128 * MIB, "{}", thorough.validated_size);
        assert!(thorough.validated_size > 120 * MIB, "{}", thorough.validated_size);
        // Even with aliasing, original data is restored.
        assert_eq!(dev.lock().unwrap().content_fingerprint(), pristine());
    }

    #[test]
    fn read_errors_are_reported() {
        let (r, _, _) = run(SimSpec::fake(800 * MB, 600 * MB, Behavior::ReadErrors), cfg(Mode::Quick));
        assert_eq!(r.verdict, Verdict::Errors);
        assert!(r.counts.read_errors > 100);
        assert!(r.validated_size <= 600 * MB);
        assert_eq!(r.calibration[0].status, RegionStatus::ReadError);
    }

    #[test]
    fn rejects_tiny_and_empty_devices() {
        let v = Validator::new(Box::new(SimDevice::new(SimSpec::healthy(100 * 4096))), cfg(Mode::Quick));
        assert!(matches!(v.run(&mut |_| {}), Err(Error::TooSmall { .. })));
        let v = Validator::new(Box::new(SimDevice::new(SimSpec::healthy(0))), cfg(Mode::Quick));
        assert!(matches!(v.run(&mut |_| {}), Err(Error::NoStorage)));
    }

    #[test]
    fn cancellation_restores_and_reports_incomplete() {
        for (mode, at) in [(Mode::Quick, 100), (Mode::Thorough, 700)] {
            let dev = Arc::new(Mutex::new(SimDevice::new(SimSpec::healthy(512 * MB))));
            let v = Validator::new(Box::new(Shared(dev.clone())), cfg(mode));
            let token = v.cancel_token();
            let mut n = 0;
            let r = v
                .run(&mut |e| {
                    if let Event::Progress { .. } = e {
                        n += 1;
                        if n == at {
                            token.cancel();
                        }
                    }
                })
                .unwrap();
            assert_eq!(r.verdict, Verdict::Incomplete, "{mode}");
            assert!(!r.completed);
            assert_eq!(dev.lock().unwrap().content_fingerprint(), pristine(), "{mode}: data changed");
        }
    }

    #[test]
    fn journal_is_removed_after_clean_run() {
        let dir = std::env::temp_dir().join(format!("flashvalidator-engine-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let c = Config { journal: JournalSetting::Directory(dir.clone()), ..cfg(Mode::Thorough) };
        let (r, _, _) = run(SimSpec::healthy(512 * MB), c);
        assert!(r.journal.is_none());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probes_include_powers_of_two() {
        let size = 1000 * MB;
        let offs = layout::region_offsets(576, size, 4096);
        let probes = build_probes(&offs, size, 4096);
        assert!(probes.iter().any(|p| p.offset == 128 * MIB && p.region.is_none()));
        assert!(probes.iter().any(|p| p.offset == 3 * 64 * MIB));
        assert!(probes.iter().all(|p| p.offset + 4096 <= size && p.offset % 4096 == 0));
    }

    fn writes_issued(dev: &Arc<Mutex<SimDevice>>) -> u64 {
        dev.lock().unwrap().op_counts().1
    }

    #[test]
    fn read_only_never_writes_and_passes_healthy_drive() {
        let (r, dev, _) = run(SimSpec::healthy(512 * MB), cfg(Mode::ReadOnly));
        assert_eq!(writes_issued(&dev), 0, "read-only mode must not write");
        assert_eq!(r.verdict, Verdict::ReadOnlyClean);
        assert_eq!(r.counts.readable, 576);
        assert_eq!(r.highest_valid_region, Some(575));
        assert!(r.calibration.is_empty() && r.journal.is_none() && r.write_stats.is_none());
        assert!(!r.wraparound_detected);
    }

    #[test]
    fn read_only_detects_wraparound_from_existing_data() {
        let (r, dev, _) = run(SimSpec::fake(1000 * MB, 128 * MIB, Behavior::Wrap), cfg(Mode::ReadOnly));
        assert_eq!(writes_issued(&dev), 0);
        assert!(r.wraparound_detected, "{:?}", r.warnings);
        assert!(matches!(r.verdict, Verdict::Counterfeit { validated_size } if validated_size <= 128 * MIB));
    }

    #[test]
    fn read_only_detects_unstable_reads() {
        let (r, dev, _) = run(SimSpec::fake(1000 * MB, 300 * MB, Behavior::Unstable), cfg(Mode::ReadOnly));
        assert_eq!(writes_issued(&dev), 0);
        assert!(matches!(r.verdict, Verdict::Counterfeit { validated_size } if validated_size <= 300 * MB));
        assert!(r.counts.mismatches > 300);
    }

    #[test]
    fn read_only_reports_blank_tail_but_cannot_prove_it() {
        let (r, dev, _) = run(SimSpec::fake(1000 * MB, 300 * MB, Behavior::Discard), cfg(Mode::ReadOnly));
        assert_eq!(writes_issued(&dev), 0);
        assert_eq!(r.verdict, Verdict::ReadOnlyClean, "zeros are indistinguishable from an empty area");
        assert!(r.warnings.iter().any(|w| w.contains("no data beyond")), "{:?}", r.warnings);
    }

    #[test]
    fn read_only_reports_read_errors() {
        let (r, dev, _) = run(SimSpec::fake(800 * MB, 600 * MB, Behavior::ReadErrors), cfg(Mode::ReadOnly));
        assert_eq!(writes_issued(&dev), 0);
        assert_eq!(r.verdict, Verdict::Errors);
    }

    #[test]
    fn events_round_trip_through_json() {
        let e = Event::Region { index: 3, status: RegionStatus::Mismatch };
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert!(matches!(back, Event::Region { index: 3, status: RegionStatus::Mismatch }));
    }

    #[test]
    fn tags_round_trip() {
        let mut b = vec![0u8; 4096];
        fill_tagged(&mut b, 77, 12, 4096 * 9);
        assert_eq!(tag_index(&b, 77), Some(12));
        assert_eq!(tag_index(&b, 78), None);
    }
}
