//! Results of a validation run.

use crate::format::{bytes_detailed, human_bytes, human_bytes_binary};
use crate::stats::Summary;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Outcome of one region.
///
/// The variants are in severity order, which matters when a restore fails
/// (see [`crate::engine`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionStatus {
    /// Not tested (yet).
    Pending,
    /// Data was written, read back intact and restored.
    Ok,
    /// Read-only mode: the region read back consistently. Storage is *not* proven.
    Readable,
    /// The drive returned an error when reading.
    ReadError,
    /// The drive returned an error when writing (or restoring).
    WriteError,
    /// No error was returned, but the data does not behave like real storage
    /// (pattern not kept, unstable reads, or aliasing another region).
    Mismatch,
}

impl RegionStatus {
    /// Single-character symbol used in text maps.
    pub fn symbol(self) -> char {
        match self {
            Self::Pending => '?',
            Self::Ok => '.',
            Self::Readable => 'o',
            Self::ReadError => 'R',
            Self::WriteError => 'W',
            Self::Mismatch => 'X',
        }
    }

    /// Short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "untested",
            Self::Ok => "validated",
            Self::Readable => "readable",
            Self::ReadError => "read error",
            Self::WriteError => "write error",
            Self::Mismatch => "no storage",
        }
    }

    /// Whether the region counts as good (validated, or readable in read-only mode).
    pub fn is_good(self) -> bool {
        matches!(self, Self::Ok | Self::Readable)
    }
}

/// How regions were tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Never writes. Checks that every region reads back consistently, and
    /// looks for aliasing in existing data. Cannot prove the capacity is real.
    ReadOnly,
    /// Each region is written, verified and restored before the next one.
    /// Fast, but cannot detect address wrap-around.
    #[default]
    Quick,
    /// Every region is written first and verified afterwards, which also
    /// detects drives whose high addresses alias low ones.
    Thorough,
}

impl Mode {
    /// Whether this mode writes to the drive.
    pub fn writes(self) -> bool {
        self != Self::ReadOnly
    }

    /// Stable lower-case name (`read-only`, `quick`, `thorough`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Quick => "quick",
            Self::Thorough => "thorough",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read-only" | "readonly" => Ok(Self::ReadOnly),
            "quick" => Ok(Self::Quick),
            "thorough" => Ok(Self::Thorough),
            other => Err(format!("unknown mode '{other}'")),
        }
    }
}

/// Identifying information about the tested drive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    /// OS path (`/dev/sdb`, `\\.\PhysicalDrive2`, ...).
    pub path: String,
    /// Vendor of the drive or of the USB bridge.
    pub vendor: Option<String>,
    /// Product name.
    pub model: Option<String>,
    /// Serial number, if reported.
    pub serial: Option<String>,
}

/// Per-status counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCounts {
    /// Regions that passed.
    pub ok: usize,
    /// Regions that read back consistently (read-only mode).
    pub readable: usize,
    /// Regions with read errors.
    pub read_errors: usize,
    /// Regions with write errors.
    pub write_errors: usize,
    /// Regions that did not behave like real storage.
    pub mismatches: usize,
    /// Regions not tested.
    pub untested: usize,
}

impl StatusCounts {
    /// Counts the statuses in `statuses`.
    pub fn from_statuses(statuses: &[RegionStatus]) -> Self {
        let mut c = Self::default();
        for s in statuses {
            match s {
                RegionStatus::Pending => c.untested += 1,
                RegionStatus::Ok => c.ok += 1,
                RegionStatus::Readable => c.readable += 1,
                RegionStatus::ReadError => c.read_errors += 1,
                RegionStatus::WriteError => c.write_errors += 1,
                RegionStatus::Mismatch => c.mismatches += 1,
            }
        }
        c
    }
}

/// One step of the calibration (warm-up) phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationStep {
    /// Transfer size used, in bytes.
    pub block_size: usize,
    /// Result of testing the last block of the drive at that size.
    pub status: RegionStatus,
}

/// Overall conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// Every region was validated.
    Pass,
    /// Read-only mode found no problem. The capacity is not proven.
    ReadOnlyClean,
    /// Some regions do not store data: the drive is smaller than it claims.
    Counterfeit {
        /// Size of the contiguous good area from the start of the drive.
        validated_size: u64,
    },
    /// No tested region stores data.
    NoValidStorage,
    /// The drive returned read or write errors (it may be failing).
    Errors,
    /// The run was stopped before every region was tested.
    Incomplete,
}

impl Verdict {
    /// Whether the verdict reports a problem with the drive.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Counterfeit { .. } | Self::NoValidStorage | Self::Errors)
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => f.write_str("PASS: every tested region stored and returned data correctly"),
            Self::ReadOnlyClean => f.write_str(
                "NO PROBLEMS FOUND by the read-only checks (the capacity is not proven; use Quick or Thorough to validate it)",
            ),
            Self::Counterfeit { validated_size } => write!(
                f,
                "FAIL: the drive does not have the capacity it claims; only about {} is real",
                human_bytes(*validated_size)
            ),
            Self::NoValidStorage => f.write_str("FAIL: no valid storage was detected"),
            Self::Errors => f.write_str("FAIL: the drive returned read/write errors; it may be failing"),
            Self::Incomplete => f.write_str("INCOMPLETE: validation did not complete"),
        }
    }
}

/// Complete result of a validation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// Tool name and version.
    pub tool: String,
    /// Start time, seconds since the Unix epoch.
    pub started_at_unix: u64,
    /// Wall-clock duration of the run.
    pub duration_secs: f64,
    /// The tested drive.
    pub device: DeviceSummary,
    /// Test mode.
    pub mode: Mode,
    /// Transfer size per region.
    pub block_size: usize,
    /// Capacity the drive declares.
    pub declared_size: u64,
    /// Byte offset of each region.
    pub region_offsets: Vec<u64>,
    /// Result of each region.
    pub region_status: Vec<RegionStatus>,
    /// Status totals.
    pub counts: StatusCounts,
    /// End of the run of good regions that starts at region 0
    /// (validated in Quick/Thorough, readable in read-only mode).
    pub validated_size: u64,
    /// Highest good region (0-based index), if any.
    pub highest_valid_region: Option<usize>,
    /// End offset of the highest good region.
    pub highest_valid_end: u64,
    /// Whether address wrap-around (aliasing) was detected.
    pub wraparound_detected: bool,
    /// Whether every region was tested.
    pub completed: bool,
    /// Overall conclusion.
    pub verdict: Verdict,
    /// Calibration steps that ran.
    pub calibration: Vec<CalibrationStep>,
    /// Read latency statistics.
    pub read_stats: Option<Summary>,
    /// Write latency statistics.
    pub write_stats: Option<Summary>,
    /// Offsets whose original data could not be written back.
    pub unrestored_offsets: Vec<u64>,
    /// Journal kept for recovery (only when something was not restored).
    pub journal: Option<PathBuf>,
    /// Non-fatal problems encountered.
    pub warnings: Vec<String>,
}

impl Report {
    /// End offset of `region`. The last region extends to the declared end.
    pub fn region_end(&self, region: usize) -> u64 {
        region_end(&self.region_offsets, region, self.block_size, self.declared_size)
    }

    /// Text map: one symbol per region, `columns` per line.
    pub fn text_map(&self, columns: usize) -> String {
        let mut s = String::new();
        for row in self.region_status.chunks(columns.max(1)) {
            s.extend(row.iter().map(|r| r.symbol()));
            s.push('\n');
        }
        s
    }

    /// Label for `validated_size`, which means "readable" in read-only mode.
    pub fn size_label(&self) -> &'static str {
        if self.mode == Mode::ReadOnly { "readable size" } else { "validated size" }
    }
}

pub(crate) fn region_end(offsets: &[u64], region: usize, block: usize, declared: u64) -> u64 {
    if region + 1 == offsets.len() { declared } else { offsets[region] + block as u64 }
}

pub(crate) fn validated_size(statuses: &[RegionStatus], offsets: &[u64], block: usize, declared: u64) -> u64 {
    let run = statuses.iter().take_while(|s| s.is_good()).count();
    if run == 0 { 0 } else { region_end(offsets, run - 1, block, declared) }
}

pub(crate) fn verdict(statuses: &[RegionStatus], completed: bool, validated: u64, mode: Mode) -> Verdict {
    let c = StatusCounts::from_statuses(statuses);
    let good = c.ok + c.readable;
    if !completed || c.untested > 0 {
        Verdict::Incomplete
    } else if good == statuses.len() {
        if mode == Mode::ReadOnly { Verdict::ReadOnlyClean } else { Verdict::Pass }
    } else if good == 0 && c.mismatches > 0 {
        Verdict::NoValidStorage
    } else if c.mismatches > 0 {
        Verdict::Counterfeit { validated_size: validated }
    } else {
        Verdict::Errors
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
        writeln!(f, "{} report", self.tool)?;
        writeln!(f, "==========================================================")?;
        writeln!(f, "device               {}", self.device.path)?;
        writeln!(f, "vendor               {}", opt(&self.device.vendor))?;
        writeln!(f, "product              {}", opt(&self.device.model))?;
        writeln!(f, "serial number        {}", opt(&self.device.serial))?;
        writeln!(
            f,
            "mode                 {} ({} regions x {} bytes)",
            self.mode,
            self.region_status.len(),
            self.block_size
        )?;
        writeln!(f, "duration             {:.1} s", self.duration_secs)?;
        writeln!(f, "declared size        {}", bytes_detailed(self.declared_size))?;
        writeln!(f, "{:<21}{}", self.size_label(), bytes_detailed(self.validated_size))?;
        match self.highest_valid_region {
            Some(r) => writeln!(
                f,
                "highest good region  #{} of {} (ends at {})",
                r + 1,
                self.region_status.len(),
                bytes_detailed(self.highest_valid_end)
            )?,
            None => writeln!(f, "highest good region  none")?,
        }
        let c = &self.counts;
        let good =
            if self.mode == Mode::ReadOnly { format!("{} readable", c.readable) } else { format!("{} ok", c.ok) };
        writeln!(
            f,
            "regions              {good}, {} no storage, {} read errors, {} write errors, {} untested",
            c.mismatches, c.read_errors, c.write_errors, c.untested
        )?;
        if self.mode == Mode::ReadOnly {
            writeln!(f, "                     read-only mode: nothing was written to the drive")?;
        }
        if self.wraparound_detected {
            writeln!(f, "                     address wrap-around detected (high addresses alias low ones)")?;
        }
        writeln!(f)?;
        writeln!(f, "VERDICT: {}", self.verdict)?;
        writeln!(f)?;
        writeln!(
            f,
            "region map ('.' validated, 'o' readable, 'X' no storage, 'R' read error, 'W' write error, '?' untested)"
        )?;
        f.write_str(&self.text_map(32))?;
        writeln!(f)?;
        writeln!(f, "performance (microseconds)      read          write")?;
        let row = |name: &str, g: &dyn Fn(&Summary) -> f64| -> String {
            let v = |s: &Option<Summary>| {
                s.as_ref().map(|s| format!("{:>14.1}", g(s))).unwrap_or_else(|| format!("{:>14}", "-"))
            };
            format!("  {name:<20}{}{}", v(&self.read_stats), v(&self.write_stats))
        };
        let samples = |s: &Option<Summary>| s.as_ref().map_or("-".to_string(), |s| s.samples.to_string());
        writeln!(f, "  {:<20}{:>14}{:>14}", "samples", samples(&self.read_stats), samples(&self.write_stats))?;
        for (name, g) in [
            ("minimum", (|s: &Summary| s.min_us) as fn(&Summary) -> f64),
            ("maximum", |s| s.max_us),
            ("average", |s| s.mean_us),
            ("median", |s| s.median_us),
            ("std dev", |s| s.std_dev_us),
            ("total time", |s| s.total_us),
        ] {
            writeln!(f, "{}", row(name, &g))?;
        }
        let tput = |s: &Option<Summary>| {
            s.as_ref().map_or("-".to_string(), |s| format!("{}/s", human_bytes(s.throughput(self.block_size) as u64)))
        };
        writeln!(f, "  {:<20}{:>14}{:>14}", "throughput", tput(&self.read_stats), tput(&self.write_stats))?;
        if !self.calibration.is_empty() {
            writeln!(f)?;
            let steps: Vec<String> = self
                .calibration
                .iter()
                .map(|s| format!("{} {}", human_bytes_binary(s.block_size as u64), s.status.label()))
                .collect();
            writeln!(f, "calibration          {}", steps.join(", "))?;
        }
        if !self.unrestored_offsets.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "WARNING: original data could not be restored at {} offset(s).",
                self.unrestored_offsets.len()
            )?;
        }
        if let Some(j) = &self.journal {
            writeln!(f, "backup journal kept at {}", j.display())?;
            writeln!(f, "  restore with: flashvalidator restore \"{}\" {}", j.display(), self.device.path)?;
        }
        for w in &self.warnings {
            writeln!(f, "note: {w}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RegionStatus::*;

    #[test]
    fn validated_size_is_leading_ok_run() {
        let offs = [0, 100, 200, 300];
        assert_eq!(validated_size(&[Ok, Ok, Mismatch, Ok], &offs, 10, 400), 110);
        assert_eq!(validated_size(&[Ok, Ok, Ok, Ok], &offs, 10, 400), 400);
        assert_eq!(validated_size(&[Mismatch, Ok, Ok, Ok], &offs, 10, 400), 0);
    }

    #[test]
    fn verdicts() {
        let q = Mode::Quick;
        assert_eq!(verdict(&[Ok, Ok], true, 0, q), Verdict::Pass);
        assert_eq!(verdict(&[Ok, Mismatch], true, 5, q), Verdict::Counterfeit { validated_size: 5 });
        assert_eq!(verdict(&[Mismatch, Mismatch], true, 0, q), Verdict::NoValidStorage);
        assert_eq!(verdict(&[Ok, ReadError], true, 0, q), Verdict::Errors);
        assert_eq!(verdict(&[Ok, Pending], true, 0, q), Verdict::Incomplete);
        assert_eq!(verdict(&[Ok, Ok], false, 0, q), Verdict::Incomplete);
        assert_eq!(verdict(&[Readable, Readable], true, 0, Mode::ReadOnly), Verdict::ReadOnlyClean);
        assert_eq!(verdict(&[Readable, ReadError], true, 0, Mode::ReadOnly), Verdict::Errors);
    }

    #[test]
    fn report_round_trips_through_json() {
        let r = Report {
            tool: "t".into(),
            started_at_unix: 1,
            duration_secs: 2.0,
            device: DeviceSummary::default(),
            mode: Mode::ReadOnly,
            block_size: 4096,
            declared_size: 10,
            region_offsets: vec![0],
            region_status: vec![Readable],
            counts: StatusCounts::default(),
            validated_size: 10,
            highest_valid_region: Some(0),
            highest_valid_end: 10,
            wraparound_detected: false,
            completed: true,
            verdict: Verdict::Counterfeit { validated_size: 3 },
            calibration: vec![],
            read_stats: None,
            write_stats: None,
            unrestored_offsets: vec![],
            journal: None,
            warnings: vec!["w".into()],
        };
        let back: Report = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.verdict, r.verdict);
        assert_eq!(back.region_status, r.region_status);
        assert_eq!(back.mode, Mode::ReadOnly);
    }

    #[test]
    fn severity_order() {
        assert!(Ok < ReadError && ReadError < WriteError && WriteError < Mismatch);
    }
}
