//! `flashvalidator`: command-line front end.
//!
//! Exit codes: `0` drive passed (or command succeeded), `1` the test could
//! not run, `2` the drive failed, `3` the test was incomplete.
//!
//! `test` and `restore` ask for confirmation first, then request root
//! (`sudo`) or administrator rights (UAC) automatically if needed.

use clap::{Parser, Subcommand, ValueEnum};
use flashvalidator_core::blockdev::Access;
use flashvalidator_core::device::{self, DeviceInfo};
use flashvalidator_core::engine::{Activity, Config, Event, JournalSetting, Phase, Validator};
use flashvalidator_core::format::{bytes_detailed, human_bytes, human_bytes_binary};
use flashvalidator_core::privilege::{self, Prompt};
use flashvalidator_core::report::{Mode, RegionStatus, Report, Verdict};
use flashvalidator_core::{Error, journal};
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "flashvalidator",
    version,
    about = "Quick, data-preserving spot-check for counterfeit and failing USB drives"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Internal: keep the elevated console window open at the end (Windows).
    #[arg(long, global = true, hide = true)]
    pause_on_exit: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List attached drives.
    List {
        /// Include non-USB drives (internal disks).
        #[arg(long)]
        all: bool,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Validate a drive (its contents are preserved).
    Test(TestArgs),
    /// Write back original data from a journal left by an interrupted run.
    Restore {
        /// Journal file (`*.fvj`).
        journal: PathBuf,
        /// Drive to restore onto.
        device: String,
        /// Restore even if the drive's identity (serial/path) differs.
        #[arg(long)]
        ignore_identity: bool,
        /// Do not ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    /// Never writes to the drive (zero risk); cannot prove the capacity.
    ReadOnly,
    /// Fast per-region write/verify/restore test.
    Quick,
    /// Write-all-then-verify with alias probes; also detects address wrap-around.
    Thorough,
}

impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::ReadOnly => Mode::ReadOnly,
            ModeArg::Quick => Mode::Quick,
            ModeArg::Thorough => Mode::Thorough,
        }
    }
}

#[derive(Debug, clap::Args)]
struct TestArgs {
    /// Drive path: /dev/sdX, or \\.\PhysicalDriveN (or just N) on Windows.
    device: String,
    /// Test mode.
    #[arg(long, value_enum, default_value_t = ModeArg::Quick)]
    mode: ModeArg,
    /// Skip the calibration (warm-up) phase.
    #[arg(long)]
    no_calibrate: bool,
    /// Do not ask for confirmation.
    #[arg(short, long)]
    yes: bool,
    /// Allow testing a drive that is not attached through USB.
    #[arg(long)]
    allow_non_usb: bool,
    /// Print the report as JSON on stdout.
    #[arg(long)]
    json: bool,
    /// Also save the JSON report to this file.
    #[arg(long, value_name = "FILE")]
    save_json: Option<PathBuf>,
    /// Directory for the crash-recovery journal.
    #[arg(long, value_name = "DIR")]
    journal_dir: Option<PathBuf>,
    /// Do not keep a crash-recovery journal (not recommended).
    #[arg(long, conflicts_with = "journal_dir")]
    no_journal: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::List { all, json } => list(all, json),
        Command::Test(args) => test(args),
        Command::Restore { journal, device, ignore_identity, yes } => restore(&journal, &device, ignore_identity, yes),
    };
    let code = match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    };
    if cli.pause_on_exit {
        eprint!("\nPress Enter to close this window...");
        let _ = io::stdin().lock().read_line(&mut String::new());
    }
    code
}

fn list(all: bool, json: bool) -> Result<ExitCode, Error> {
    let devices: Vec<DeviceInfo> = device::list_devices()?.into_iter().filter(|d| all || d.is_usb()).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&devices).expect("serialisable"));
        return Ok(ExitCode::SUCCESS);
    }
    if devices.is_empty() {
        println!("No {}drives found.", if all { "" } else { "USB " });
        if !all {
            println!("Use --all to include internal drives.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    println!("{:<22} {:>10}  {:<6} {:<36} MOUNTED", "DEVICE", "SIZE", "BUS", "PRODUCT");
    for d in devices {
        let bus = match d.bus {
            device::Bus::Usb => "usb",
            device::Bus::Other => "other",
        };
        println!(
            "{:<22} {:>10}  {:<6} {:<36} {}",
            d.path,
            human_bytes(d.size_bytes),
            bus,
            truncate(&d.product_name(), 36),
            d.mount_points.join(", ")
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Re-runs this command with elevated rights, adding `--yes` (the user has
/// already confirmed). Returns the elevated process's exit code.
fn relaunch_elevated() -> Result<ExitCode, Error> {
    let exe = std::env::current_exe()?;
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if !args.iter().any(|a| a == "--yes" || a == "-y") {
        args.push("--yes".into());
    }
    if cfg!(windows) {
        args.push("--pause-on-exit".into());
        eprintln!("Administrator rights are needed; continuing in an elevated window...");
    } else {
        eprintln!("Root rights are needed to access the drive.");
    }
    let mut child = privilege::spawn_elevated(&exe, &args, Prompt::Terminal).map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            Error::InvalidConfig("permission was not granted".into())
        } else {
            Error::Io(e)
        }
    })?;
    let code = child.wait()?;
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

fn test(args: TestArgs) -> Result<ExitCode, Error> {
    let info = device::find_device(&args.device)?;
    if !info.is_usb() && !args.allow_non_usb {
        return Err(Error::InvalidConfig(format!(
            "{} is not a USB drive; pass --allow-non-usb if you are sure",
            info.path
        )));
    }
    let mode = Mode::from(args.mode);

    if !args.yes {
        eprintln!("Drive:          {}", info.label());
        if let Some(s) = &info.serial {
            eprintln!("Serial:         {s}");
        }
        eprintln!("Declared size:  {}", bytes_detailed(info.size_bytes));
        if !info.mount_points.is_empty() {
            eprintln!("Mounted at:     {}", info.mount_points.join(", "));
            if mode.writes() {
                #[cfg(target_os = "linux")]
                eprintln!("                (unmount these first, e.g. `udisksctl unmount -b <partition>`)");
                #[cfg(windows)]
                eprintln!("                (these volumes will be dismounted during the test)");
            }
        }
        eprintln!("Mode:           {mode}");
        eprintln!();
        if mode.writes() {
            eprintln!("The test writes to 576 places on the drive and restores the original data.");
            eprintln!("A failing drive could still lose data: back up anything important first,");
            eprintln!("or use --mode read-only, which never writes.");
        } else {
            eprintln!("Read-only mode never writes to the drive. It cannot prove the capacity is real.");
        }
        if !confirm("Type YES to start the test: ")? {
            eprintln!("Cancelled.");
            return Ok(ExitCode::from(1));
        }
    }

    if !privilege::can_open_raw(&info.path, mode.writes()) {
        return relaunch_elevated();
    }

    let journal = if args.no_journal {
        JournalSetting::Disabled
    } else {
        JournalSetting::Directory(args.journal_dir.clone().unwrap_or_else(journal::default_dir))
    };
    let cfg = Config { mode, calibrate: !args.no_calibrate, journal, ..Config::default() };

    let dev = info.open(if mode.writes() { Access::ReadWrite } else { Access::ReadOnly })?;
    let validator = Validator::new(dev, cfg).with_device_summary(info.summary());
    let token = validator.cancel_token();
    let _ = ctrlc::set_handler(move || {
        eprintln!("\nStopping after the current region (original data will be restored)...");
        token.cancel();
    });

    let mut progress = Progress::default();
    let report = validator.run(&mut |e| progress.on_event(e))?;
    progress.finish();

    if let Some(path) = &args.save_json {
        std::fs::write(path, serde_json::to_string_pretty(&report).expect("serialisable"))?;
        eprintln!("JSON report saved to {}", path.display());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report).expect("serialisable"));
    } else {
        println!("{report}");
    }
    Ok(exit_code(&report))
}

fn exit_code(report: &Report) -> ExitCode {
    match report.verdict {
        Verdict::Pass | Verdict::ReadOnlyClean => ExitCode::SUCCESS,
        Verdict::Incomplete => ExitCode::from(3),
        Verdict::Counterfeit { .. } | Verdict::NoValidStorage | Verdict::Errors => ExitCode::from(2),
    }
}

fn restore(
    journal_path: &std::path::Path,
    device_path: &str,
    ignore_identity: bool,
    yes: bool,
) -> Result<ExitCode, Error> {
    let contents = journal::read_journal(journal_path)?;
    let info = device::find_device(device_path)?;
    eprintln!("Journal:  {} ({} region(s) to restore)", journal_path.display(), contents.pending.len());
    eprintln!("Recorded: {} ({})", contents.identity, human_bytes(contents.device_size));
    eprintln!("Target:   {}", info.label());
    if contents.truncated_tail {
        eprintln!("note: the journal ends with an incomplete record (ignored; that region was never written)");
    }
    if contents.pending.is_empty() {
        eprintln!("Nothing to restore.");
        return Ok(ExitCode::SUCCESS);
    }
    if !yes && !confirm("Type YES to write the original data back: ")? {
        eprintln!("Cancelled.");
        return Ok(ExitCode::from(1));
    }
    if !privilege::can_open_raw(&info.path, true) {
        return relaunch_elevated();
    }
    let mut dev = info.open(Access::ReadWrite)?;
    let n = journal::replay(journal_path, dev.as_mut(), ignore_identity)?;
    eprintln!("Restored {n} region(s). You may now delete {}", journal_path.display());
    Ok(ExitCode::SUCCESS)
}

fn confirm(prompt: &str) -> Result<bool, Error> {
    if !io::stdin().is_terminal() {
        return Err(Error::InvalidConfig("standard input is not a terminal; pass --yes to confirm".into()));
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim() == "YES")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_owned() } else { format!("{}...", s.chars().take(max - 3).collect::<String>()) }
}

/// Renders a single updating status line on stderr.
#[derive(Default)]
struct Progress {
    phase: Option<Phase>,
    done: usize,
    total: usize,
    counts: [usize; 4],
    statuses: Vec<RegionStatus>,
    activity: Option<Activity>,
    last_draw: Option<std::time::Instant>,
}

impl Progress {
    fn on_event(&mut self, e: Event) {
        match e {
            Event::Phase(p) => {
                self.phase = Some(p);
                self.draw(true);
            }
            Event::Progress { done, total } => {
                self.done = done;
                self.total = total;
                self.draw(false);
            }
            Event::Region { index, status } => {
                // A region can change status more than once (thorough mode), so
                // keep the latest value and adjust the counters.
                let slot = |s: RegionStatus| match s {
                    RegionStatus::Ok | RegionStatus::Readable => Some(0),
                    RegionStatus::Mismatch => Some(1),
                    RegionStatus::ReadError => Some(2),
                    RegionStatus::WriteError => Some(3),
                    RegionStatus::Pending => None,
                };
                if let Some(old) = self.statuses.get(index).copied().and_then(slot) {
                    self.counts[old] -= 1;
                }
                if let Some(new) = slot(status) {
                    self.counts[new] += 1;
                }
                if let Some(s) = self.statuses.get_mut(index) {
                    *s = status;
                }
            }
            Event::Calibration { step, total, result } => {
                self.done = step;
                self.total = total;
                if result.status != RegionStatus::Ok {
                    eprintln!(
                        "\ncalibration step {step} ({}): {}",
                        human_bytes_binary(result.block_size as u64),
                        result.status.label()
                    );
                }
                self.draw(false);
            }
            Event::Activity(a) => self.activity = Some(a),
            Event::Warning(w) => eprintln!("\nwarning: {w}"),
            Event::Started { offsets, .. } => self.statuses = vec![RegionStatus::Pending; offsets.len()],
        }
    }

    fn draw(&mut self, force: bool) {
        let now = std::time::Instant::now();
        if !force && self.last_draw.is_some_and(|t| now.duration_since(t).as_millis() < 100) && self.done != self.total
        {
            return;
        }
        self.last_draw = Some(now);
        let label = self.phase.map_or("Starting", Phase::label);
        let pct = (self.done * 100).checked_div(self.total).unwrap_or(0);
        let rw = match self.activity {
            Some(Activity::Reading) => 'R',
            Some(Activity::Writing) => 'W',
            _ => ' ',
        };
        let [ok, ns, re, we] = self.counts;
        eprint!(
            "\r\x1b[2K[{rw}] {label:<30} {:>4}/{:<4} {pct:>3}%   ok {ok}  no-storage {ns}  read-err {re}  write-err {we}",
            self.done, self.total
        );
        let _ = io::stderr().flush();
    }

    fn finish(&mut self) {
        self.draw(true);
        eprintln!();
    }
}
