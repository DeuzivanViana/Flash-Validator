//! Application state and layout.

use crate::map;
use crate::theme::{self, palette};
use crate::worker::{Message, Worker};
use eframe::egui::{self, Color32, RichText};
use flashvalidator_core::device::{self, DeviceInfo};
use flashvalidator_core::engine::{Activity, Config, Event, Phase};
use flashvalidator_core::format::{bytes_detailed, human_bytes};
use flashvalidator_core::layout::DEFAULT_REGIONS;
use flashvalidator_core::privilege;
use flashvalidator_core::report::{Mode, RegionStatus, Report, Verdict};
use flashvalidator_core::stats::Summary;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How often the device list refreshes while idle.
const REFRESH_EVERY: Duration = Duration::from_secs(3);

/// Live view of the current (or last) run.
#[derive(Debug, Default)]
struct RunView {
    device_label: String,
    mode: Mode,
    offsets: Vec<u64>,
    block_size: usize,
    statuses: Vec<RegionStatus>,
    phase: Option<Phase>,
    status: String,
    activity: Option<Activity>,
    progress: (usize, usize),
    calibration: (usize, usize),
    report: Option<Report>,
    error: Option<String>,
}

impl RunView {
    fn new(device_label: String, mode: Mode) -> Self {
        Self { device_label, mode, statuses: vec![RegionStatus::Pending; DEFAULT_REGIONS], ..Self::default() }
    }

    fn apply(&mut self, e: Event) {
        match e {
            Event::Started { offsets, block_size, .. } => {
                self.statuses = vec![RegionStatus::Pending; offsets.len()];
                self.offsets = offsets;
                self.block_size = block_size;
            }
            Event::Phase(p) => self.phase = Some(p),
            Event::Activity(a) => self.activity = Some(a),
            Event::Calibration { step, total, .. } => self.calibration = (step, total),
            Event::Region { index, status } => {
                if let Some(s) = self.statuses.get_mut(index) {
                    *s = status;
                }
            }
            Event::Progress { done, total } => self.progress = (done, total),
            Event::Warning(_) => {}
        }
    }
}

/// The FlashValidator window.
#[derive(Debug)]
pub struct App {
    devices: Vec<DeviceInfo>,
    selected: Option<String>,
    show_all: bool,
    mode: Mode,
    calibrate: bool,
    confirm: bool,
    worker: Option<Worker>,
    view: Option<RunView>,
    notice: Option<(String, bool)>,
    last_refresh: Option<Instant>,
    list_error: Option<String>,
}

fn accent_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).strong().color(Color32::WHITE))
        .fill(palette::ACCENT_DIM)
        .stroke(egui::Stroke::new(1.0, palette::ACCENT))
}

impl App {
    /// Creates the application.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut app = Self {
            devices: Vec::new(),
            selected: None,
            show_all: false,
            mode: Mode::Quick,
            calibrate: true,
            confirm: false,
            worker: None,
            view: None,
            notice: None,
            last_refresh: None,
            list_error: None,
        };
        app.refresh_devices();
        app
    }

    fn running(&self) -> bool {
        self.worker.is_some()
    }

    fn refresh_devices(&mut self) {
        self.last_refresh = Some(Instant::now());
        let mut list = match device::list_devices() {
            Ok(l) => {
                self.list_error = None;
                l
            }
            Err(e) => {
                self.list_error = Some(e.to_string());
                Vec::new()
            }
        };
        if !self.show_all {
            list.retain(DeviceInfo::is_usb);
        }
        self.devices = list;
        if self.selected.as_ref().is_none_or(|s| !self.devices.iter().any(|d| &d.path == s)) {
            self.selected = self.devices.first().map(|d| d.path.clone());
        }
    }

    fn selected_device(&self) -> Option<&DeviceInfo> {
        let path = self.selected.as_ref()?;
        self.devices.iter().find(|d| &d.path == path)
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(dev) = self.selected_device().cloned() else { return };
        let cfg = Config { mode: self.mode, calibrate: self.calibrate, ..Config::default() };
        self.view = Some(RunView::new(dev.label(), self.mode));
        self.notice = None;
        self.worker = Some(Worker::spawn(dev, cfg, ctx.clone()));
    }

    fn poll_worker(&mut self) {
        let Some(worker) = &self.worker else { return };
        let mut finished = false;
        for msg in worker.drain() {
            let Some(view) = self.view.as_mut() else { continue };
            match msg {
                Message::Status(s) => view.status = s,
                Message::Event(e) => view.apply(e),
                Message::Done(Ok(report)) => {
                    view.statuses = report.region_status.clone();
                    view.offsets = report.region_offsets.clone();
                    view.block_size = report.block_size;
                    view.report = Some(*report);
                    finished = true;
                }
                Message::Done(Err(e)) => {
                    view.error = Some(e);
                    finished = true;
                }
            }
        }
        if finished {
            self.worker = None;
            self.refresh_devices();
        }
    }

    fn save_report(&mut self) {
        let Some(report) = self.view.as_ref().and_then(|v| v.report.as_ref()) else { return };
        let dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = dir.join(format!("flashvalidator-report-{}", report.started_at_unix));
        let txt = stem.with_extension("txt");
        let json = stem.with_extension("json");
        let result = std::fs::write(&txt, report.to_string())
            .and_then(|()| std::fs::write(&json, serde_json::to_string_pretty(report).unwrap_or_default()));
        self.notice = Some(match result {
            Ok(()) => (format!("Report saved to {} and {}", txt.display(), json.display()), false),
            Err(e) => (format!("Could not save the report: {e}"), true),
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("FlashValidator").size(20.0).strong().color(palette::TEXT));
            ui.label(RichText::new("USB drive validator").color(palette::TEXT_WEAK));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("v{}", flashvalidator_core::VERSION)).color(palette::TEXT_WEAK));
            });
        });
        ui.add_space(2.0);
        ui.separator();

        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                ui.label("Drive");
                let text = self.selected_device().map_or_else(|| "No USB drive found".to_owned(), DeviceInfo::label);
                egui::ComboBox::from_id_salt("device")
                    .width((ui.available_width() - 190.0).max(200.0))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for d in &self.devices {
                            ui.selectable_value(&mut self.selected, Some(d.path.clone()), d.label());
                        }
                    });
                if ui.button("Refresh").clicked() {
                    self.refresh_devices();
                }
                if ui
                    .checkbox(&mut self.show_all, "Non-USB")
                    .on_hover_text("Also list internal drives. Be careful: never test a system disk.")
                    .changed()
                {
                    self.refresh_devices();
                }
            });

            if let Some(d) = self.selected_device() {
                ui.horizontal_wrapped(|ui| {
                    let weak = |s: String| RichText::new(s).color(palette::TEXT_WEAK);
                    ui.label(weak(format!("Declared size {}", bytes_detailed(d.size_bytes))));
                    if let Some(s) = &d.serial {
                        ui.label(weak(format!("·  Serial {s}")));
                    }
                    if !d.mount_points.is_empty() {
                        ui.label(
                            RichText::new(format!("·  Mounted {}", d.mount_points.join(", "))).color(palette::WARN),
                        );
                    }
                    if !d.is_usb() {
                        ui.label(RichText::new("·  NOT A USB DRIVE").color(palette::BAD).strong());
                    }
                });
            }

            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("Mode");
                ui.radio_value(&mut self.mode, Mode::ReadOnly, "Read-only").on_hover_text(
                    "Never writes to the drive (zero risk). Finds read errors, unstable reads and wrap-around \
                     in existing data, but cannot prove the capacity is real.",
                );
                ui.radio_value(&mut self.mode, Mode::Quick, "Quick")
                    .on_hover_text("Fast: each region is written, verified and restored in turn.");
                ui.radio_value(&mut self.mode, Mode::Thorough, "Thorough").on_hover_text(
                    "Writes every region first, then verifies. Also detects drives that wrap addresses.",
                );
                ui.add_enabled(self.mode.writes(), egui::Checkbox::new(&mut self.calibrate, "Calibration warm-up"));
            });
        });
        if let Some(e) = &self.list_error {
            ui.colored_label(palette::BAD, format!("Could not list drives: {e}"));
        }

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if running {
                let cancelling = self.worker.as_ref().is_some_and(Worker::is_cancelling);
                let label = if cancelling { "Stopping... (restoring data)" } else { "Stop" };
                if ui.add_enabled(!cancelling, egui::Button::new(label)).clicked() {
                    if let Some(w) = &self.worker {
                        w.cancel();
                    }
                }
            } else {
                let can_start = self.selected_device().is_some();
                if ui.add_enabled(can_start, accent_button("Validate this drive")).clicked() {
                    self.confirm = true;
                }
            }
            if let Some((msg, is_err)) = &self.notice {
                ui.colored_label(if *is_err { palette::BAD } else { palette::TEXT_WEAK }, msg);
            }
        });
        ui.add_space(6.0);
    }

    fn confirm_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.confirm {
            return;
        }
        let Some(dev) = self.selected_device().cloned() else {
            self.confirm = false;
            return;
        };
        let ctx = ui.ctx().clone();
        let mode = self.mode;
        let needs_auth = !privilege::can_open_raw(&dev.path, mode.writes());
        let mut start = false;
        let modal = egui::Modal::new(egui::Id::unique("flashvalidator-confirm")).show(&ctx, |ui| {
            ui.set_max_width(480.0);
            ui.label(RichText::new("Validate this drive?").size(17.0).strong());
            ui.add_space(6.0);
            ui.label(RichText::new(dev.label()).strong());
            ui.add_space(6.0);
            if mode.writes() {
                ui.label(
                    "FlashValidator will write test data to 576 places on the drive and put the original data back. \
                     It is designed to preserve your files, but a failing drive could still lose data. \
                     Back up anything important first, or choose Read-only mode.",
                );
                if !dev.mount_points.is_empty() {
                    ui.add_space(6.0);
                    #[cfg(windows)]
                    ui.colored_label(
                        palette::WARN,
                        "Its volumes will be dismounted and the disk taken offline during the test.",
                    );
                    #[cfg(not(windows))]
                    ui.colored_label(palette::WARN, "Unmount all of its partitions first, or the test cannot start.");
                }
            } else {
                ui.label(
                    "Read-only mode never writes to the drive, so your data cannot be changed. \
                     It finds read errors, unstable reads and address wrap-around, but it cannot prove \
                     that the declared capacity is real.",
                );
            }
            if !dev.is_usb() {
                ui.add_space(6.0);
                ui.colored_label(palette::BAD, "This is NOT a USB drive. Make sure it is not a system disk.");
            }
            if needs_auth {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(if cfg!(windows) {
                        "Windows will ask for administrator permission to access the drive."
                    } else {
                        "You will be asked for your password to access the drive."
                    })
                    .color(palette::TEXT_WEAK),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(accent_button("Start")).clicked() {
                    start = true;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm = false;
        }
        if start {
            self.confirm = false;
            self.start(&ctx);
        }
    }

    fn status_line(ui: &mut egui::Ui, view: &RunView, running: bool) {
        ui.horizontal(|ui| {
            let (dot, tip) = match (running, view.activity) {
                (true, Some(Activity::Reading)) => (palette::OK, "reading"),
                (true, Some(Activity::Writing)) => (palette::BAD, "writing"),
                _ => (palette::TEXT_WEAK, "idle"),
            };
            let (r, resp) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, dot);
            resp.on_hover_text(tip);
            let text = match view.phase {
                Some(Phase::Calibrating) if view.calibration.1 > 0 => {
                    format!("Calibrating ({}/{})", view.calibration.0, view.calibration.1)
                }
                Some(p) => p.label().to_owned(),
                None if !view.status.is_empty() => view.status.clone(),
                None => "Opening drive".to_owned(),
            };
            ui.label(text);
            if view.mode == Mode::ReadOnly {
                ui.label(RichText::new("·  read-only, nothing is written").color(palette::READABLE));
            }
        });
        let (done, total) = view.progress;
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        ui.add(
            egui::ProgressBar::new(frac)
                .show_percentage()
                .animate(running)
                .corner_radius(1.0)
                .desired_height(14.0)
                .fill(palette::ACCENT_DIM),
        );
    }

    fn report_panel(&mut self, ui: &mut egui::Ui) {
        let Some(view) = &self.view else { return };
        if let Some(err) = &view.error {
            ui.colored_label(palette::BAD, RichText::new(format!("The test could not run: {err}")).strong());
            return;
        }
        let Some(r) = &view.report else { return };
        let color = match r.verdict {
            Verdict::Pass => palette::OK,
            Verdict::ReadOnlyClean => palette::READABLE,
            Verdict::Incomplete => palette::WARN,
            _ => palette::BAD,
        };
        ui.label(RichText::new(r.verdict.to_string()).color(color).strong().size(15.0));
        ui.add_space(4.0);

        egui::Grid::new("summary").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(RichText::new(k).color(palette::TEXT_WEAK));
                ui.label(v);
                ui.end_row();
            };
            row("Declared size", bytes_detailed(r.declared_size));
            let size_label = if r.mode == Mode::ReadOnly { "Readable size" } else { "Validated size" };
            row(size_label, bytes_detailed(r.validated_size));
            row(
                "Highest good region",
                r.highest_valid_region
                    .map_or("none".into(), |i| format!("#{} (ends at {})", i + 1, human_bytes(r.highest_valid_end))),
            );
            row(
                "Vendor / product",
                format!("{} {}", r.device.vendor.as_deref().unwrap_or("-"), r.device.model.as_deref().unwrap_or("")),
            );
            row("Serial number", r.device.serial.clone().unwrap_or_else(|| "-".into()));
            row("Mode / duration", format!("{} / {:.1} s", r.mode, r.duration_secs));
            if r.wraparound_detected {
                row("Wrap-around", "detected (high addresses alias low ones)".into());
            }
        });

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Performance").default_open(true).show(ui, |ui| perf_table(ui, r));
        if !r.warnings.is_empty() || r.journal.is_some() || !r.unrestored_offsets.is_empty() {
            egui::CollapsingHeader::new("Notes").default_open(true).show(ui, |ui| {
                if !r.unrestored_offsets.is_empty() {
                    ui.colored_label(
                        palette::BAD,
                        format!("Original data could not be restored at {} place(s).", r.unrestored_offsets.len()),
                    );
                }
                if let Some(j) = &r.journal {
                    ui.label(format!("Backup journal kept at {}", j.display()));
                    ui.label(
                        RichText::new(format!(
                            "Restore with: flashvalidator restore \"{}\" {}",
                            j.display(),
                            r.device.path
                        ))
                        .monospace(),
                    );
                }
                for w in &r.warnings {
                    ui.label(RichText::new(format!("• {w}")).color(palette::TEXT_WEAK));
                }
            });
        }
        ui.add_space(6.0);
        let mut save = false;
        ui.horizontal(|ui| {
            if ui.button("Copy report").clicked() {
                ui.ctx().copy_text(r.to_string());
            }
            if ui.button("Save report").clicked() {
                save = true;
            }
        });
        if save {
            self.save_report();
        }
    }
}

fn perf_table(ui: &mut egui::Ui, r: &Report) {
    let fmt = |s: &Option<Summary>, f: fn(&Summary) -> f64| s.as_ref().map_or("-".into(), |s| format!("{:.0}", f(s)));
    egui::Grid::new("perf").num_columns(3).striped(true).spacing([28.0, 2.0]).show(ui, |ui| {
        ui.label(RichText::new("microseconds").color(palette::TEXT_WEAK));
        ui.strong("read");
        ui.strong("write");
        ui.end_row();
        let samples = |s: &Option<Summary>| s.as_ref().map_or("-".into(), |s| s.samples.to_string());
        ui.label("samples");
        ui.label(samples(&r.read_stats));
        ui.label(samples(&r.write_stats));
        ui.end_row();
        type Getter = fn(&Summary) -> f64;
        let rows: [(&str, Getter); 6] = [
            ("minimum", |s| s.min_us),
            ("maximum", |s| s.max_us),
            ("average", |s| s.mean_us),
            ("median", |s| s.median_us),
            ("std dev", |s| s.std_dev_us),
            ("total", |s| s.total_us),
        ];
        for (name, f) in rows {
            ui.label(name);
            ui.label(fmt(&r.read_stats, f));
            ui.label(fmt(&r.write_stats, f));
            ui.end_row();
        }
        let tput = |s: &Option<Summary>| {
            s.as_ref().map_or("-".into(), |s| format!("{}/s", human_bytes(s.throughput(r.block_size) as u64)))
        };
        ui.label("throughput");
        ui.label(tput(&r.read_stats));
        ui.label(tput(&r.write_stats));
        ui.end_row();
    });
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        if !self.running() && !self.confirm && self.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY) {
            self.refresh_devices();
        }
        if !self.running() {
            ctx.request_repaint_after(REFRESH_EVERY);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let top = egui::Frame::new().fill(palette::BG).inner_margin(egui::Margin::symmetric(14, 4));
        egui::Panel::top("top").frame(top).show(ui, |ui| self.top_bar(ui));
        let running = self.running();
        let central = egui::Frame::new().fill(palette::BG).inner_margin(egui::Margin::symmetric(14, 10));
        egui::CentralPanel::default().frame(central).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match &self.view {
                Some(view) => {
                    ui.label(RichText::new(&view.device_label).strong());
                    ui.add_space(4.0);
                    Self::status_line(ui, view, running);
                    ui.add_space(10.0);
                    map::region_map(ui, &view.statuses, &view.offsets, view.block_size);
                    ui.add_space(6.0);
                    map::legend(ui, &view.statuses, view.mode == Mode::ReadOnly);
                    ui.separator();
                    self.report_panel(ui);
                }
                None => {
                    ui.add_space(28.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Select a USB drive and press \"Validate this drive\".").size(16.0));
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "FlashValidator checks 576 regions spread across the drive's declared capacity to find \
                                 counterfeit (fake-capacity) and failing drives, without erasing your files.",
                            )
                            .color(palette::TEXT_WEAK),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Read-only mode never writes to the drive at all.").color(palette::READABLE),
                        );
                    });
                }
            });
        });
        self.confirm_dialog(ui);
    }
}
