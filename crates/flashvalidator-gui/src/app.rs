//! Application state and layout.
//!
//! The window is split into a side bar (drive, mode, start/stop) and a main
//! area that shows a welcome screen, or the live run and its report.

use crate::icon_art;
use crate::map;
use crate::theme::{self, caption, palette};
use crate::worker::{Message, Worker};
use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke};
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

/// Width of the side bar.
const SIDEBAR_WIDTH: f32 = 330.0;

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
    logo: Logo,
}

/// The app icon as a texture (for the header and welcome screen).
struct Logo(egui::TextureHandle);

impl std::fmt::Debug for Logo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Logo")
    }
}

impl Logo {
    fn image(&self, size: f32) -> egui::Image<'static> {
        egui::Image::from_texture(egui::load::SizedTexture::from_handle(&self.0))
            .fit_to_exact_size(egui::vec2(size, size))
    }
}

/// The main call-to-action: a light, high-contrast button on the black UI.
fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).strong().size(15.0).color(palette::BG))
        .fill(palette::TEXT)
        .stroke(Stroke::new(1.0, Color32::WHITE))
        .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS))
}

/// A secondary, outlined button.
fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(palette::TEXT))
        .fill(palette::WIDGET)
        .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS))
}

/// A small rounded label, e.g. "USB" or "Mounted".
fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(theme::mix(palette::BG, color, 0.12))
        .stroke(Stroke::new(1.0, theme::mix(palette::BG, color, 0.40)))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(color).strong());
        });
}

/// A square icon button with a painted "refresh" arrow.
fn refresh_button(ui: &mut egui::Ui) -> egui::Response {
    let resp = ui.add(
        egui::Button::new("").min_size(egui::vec2(30.0, 28.0)).corner_radius(CornerRadius::same(theme::WIDGET_RADIUS)),
    );
    let color = ui.style().interact(&resp).fg_stroke.color;
    let c = resp.rect.center();
    let r = 6.0;
    let (start, end) = (-0.9_f32, 4.4_f32);
    let points: Vec<egui::Pos2> = (0..=24)
        .map(|i| {
            let a = start + (end - start) * i as f32 / 24.0;
            c + r * egui::vec2(a.cos(), a.sin())
        })
        .collect();
    let tip = *points.last().unwrap_or(&c);
    let painter = ui.painter();
    painter.add(egui::Shape::line(points, Stroke::new(1.6, color)));
    // Arrow head pointing along the arc's direction at its end.
    let dir = egui::vec2(-end.sin(), end.cos());
    let side = egui::vec2(-dir.y, dir.x);
    painter.add(egui::Shape::convex_polygon(
        vec![tip + dir * 3.5, tip - dir * 1.5 + side * 3.0, tip - dir * 1.5 - side * 3.0],
        color,
        Stroke::NONE,
    ));
    resp
}

/// A small filled dot (activity / status indicator).
fn dot(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 4.0, color);
    resp
}

/// A thin, rounded progress track.
fn progress_track(ui: &mut egui::Ui, frac: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
    let radius = CornerRadius::same(3);
    ui.painter().rect_filled(rect, radius, palette::WIDGET);
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * frac).max(6.0));
        ui.painter().rect_filled(fill, radius, color);
    }
}

/// Short title, accent colour and explanation for a verdict.
fn verdict_style(v: Verdict) -> (&'static str, Color32, String) {
    let (title, color) = match v {
        Verdict::Pass => ("Drive passed", palette::OK),
        Verdict::ReadOnlyClean => ("No problems found", palette::READABLE),
        Verdict::Counterfeit { .. } => ("Counterfeit capacity", palette::BAD),
        Verdict::NoValidStorage => ("No valid storage", palette::BAD),
        Verdict::Errors => ("Read / write errors", palette::BAD),
        Verdict::Incomplete => ("Test incomplete", palette::WARN),
    };
    // The full sentence is "LABEL: explanation"; keep the explanation.
    let full = v.to_string();
    let detail = full.split_once(": ").map_or(full.as_str(), |(_, d)| d);
    let mut chars = detail.chars();
    let detail = chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str());
    (title, color, detail)
}

/// A statistic: large value, small caption underneath.
fn stat_tile(ui: &mut egui::Ui, label: &str, value: String, hint: Option<String>, color: Color32) {
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| stat_tile_inner(ui, label, value, hint, color));
}

fn stat_tile_inner(ui: &mut egui::Ui, label: &str, value: String, hint: Option<String>, color: Color32) {
    let resp = egui::Frame::new()
        .fill(palette::SURFACE)
        .stroke(Stroke::new(1.0, palette::BORDER))
        .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(caption(label)).truncate());
            ui.add(egui::Label::new(RichText::new(value).size(20.0).strong().color(color)).truncate());
        })
        .response;
    if let Some(h) = hint {
        resp.on_hover_text(h);
    }
}

impl App {
    /// Creates the application.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        const LOGO_SIZE: u32 = 128;
        let logo = Logo(cc.egui_ctx.load_texture(
            "flashvalidator-logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [LOGO_SIZE as usize, LOGO_SIZE as usize],
                &icon_art::icon_rgba(LOGO_SIZE),
            ),
            egui::TextureOptions::LINEAR,
        ));
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
            logo,
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

    // ----------------------------------------------------------------- header

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(self.logo.image(28.0));
            ui.add_space(2.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("FlashValidator").size(16.0).strong().color(palette::TEXT));
                ui.label(RichText::new("USB drive validator").size(11.5).color(palette::TEXT_WEAK));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                chip(ui, &format!("v{}", flashvalidator_core::VERSION), palette::TEXT_WEAK);
                if self.running() {
                    chip(ui, "Test running", palette::ACCENT);
                }
            });
        });
    }

    // ---------------------------------------------------------------- sidebar

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let actions = egui::Frame::new().fill(palette::SURFACE).inner_margin(egui::Margin {
            left: 18,
            right: 18,
            top: 12,
            bottom: 18,
        });
        egui::Panel::bottom("sidebar-actions").frame(actions).show_separator_line(true).show(ui, |ui| self.actions(ui));

        let body = egui::Frame::new().fill(palette::SURFACE).inner_margin(egui::Margin::symmetric(18, 16));
        egui::CentralPanel::default().frame(body).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let running = self.running();
                ui.add_enabled_ui(!running, |ui| {
                    self.drive_section(ui);
                    ui.add_space(10.0);
                    self.mode_section(ui);
                });
            });
        });
    }

    fn drive_section(&mut self, ui: &mut egui::Ui) {
        ui.label(caption("Drive"));
        ui.horizontal(|ui| {
            let text = self.selected_device().map_or_else(
                || "No USB drive found".to_owned(),
                |d| format!("{}  ·  {}", d.product_name(), human_bytes(d.size_bytes)),
            );
            egui::ComboBox::from_id_salt("device")
                .width((ui.available_width() - 38.0).max(120.0))
                .height(320.0)
                .truncate()
                .selected_text(text)
                .show_ui(ui, |ui| {
                    for d in &self.devices {
                        ui.selectable_value(&mut self.selected, Some(d.path.clone()), d.label());
                    }
                });
            if refresh_button(ui).on_hover_text("Refresh the drive list").clicked() {
                self.refresh_devices();
            }
        });

        ui.add_space(4.0);
        let info = egui::Frame::new()
            .fill(palette::SUNKEN)
            .stroke(Stroke::new(1.0, palette::BORDER))
            .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
            .inner_margin(egui::Margin::symmetric(14, 12));
        info.show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(d) = self.selected_device() {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&d.path).monospace().color(palette::TEXT_WEAK));
                    ui.add(egui::Label::new(RichText::new(d.product_name()).strong().color(palette::TEXT)).truncate());
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(human_bytes(d.size_bytes)).size(24.0).strong().color(palette::TEXT));
                    ui.label(RichText::new("declared").color(palette::TEXT_FAINT));
                })
                .response
                .on_hover_text(bytes_detailed(d.size_bytes));
                if let Some(s) = &d.serial {
                    ui.add(
                        egui::Label::new(RichText::new(format!("Serial {s}")).size(12.0).color(palette::TEXT_WEAK))
                            .truncate(),
                    );
                }
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    if d.is_usb() {
                        chip(ui, "USB", palette::TEXT_WEAK);
                    } else {
                        chip(ui, "NOT A USB DRIVE", palette::BAD);
                    }
                    if d.removable {
                        chip(ui, "Removable", palette::TEXT_WEAK);
                    }
                    if !d.mount_points.is_empty() {
                        egui::Frame::new()
                            .show(ui, |ui| chip(ui, "Mounted", palette::WARN))
                            .response
                            .on_hover_text(format!("Mounted at {}", d.mount_points.join(", ")));
                    }
                });
            } else {
                ui.label(RichText::new("No drive selected").strong().color(palette::TEXT_WEAK));
                ui.label(
                    RichText::new("Plug in a USB drive. The list refreshes automatically every few seconds.")
                        .size(12.5)
                        .color(palette::TEXT_FAINT),
                );
            }
        });

        ui.add_space(2.0);
        if ui
            .checkbox(&mut self.show_all, RichText::new("Show non-USB drives").color(palette::TEXT_WEAK))
            .on_hover_text("Also list internal drives. Be careful: never test a system disk.")
            .changed()
        {
            self.refresh_devices();
        }
        if let Some(e) = &self.list_error {
            ui.colored_label(palette::BAD, format!("Could not list drives: {e}"));
        }
    }

    fn mode_section(&mut self, ui: &mut egui::Ui) {
        ui.label(caption("Test mode"));
        let modes = [
            (
                Mode::ReadOnly,
                "Read-only",
                "Never writes to the drive. Finds read errors and unstable reads, but cannot prove the capacity.",
                Some(("No writes", palette::READABLE)),
            ),
            (
                Mode::Quick,
                "Quick",
                "Each region is written, verified and restored in turn.",
                Some(("Default", palette::TEXT_WEAK)),
            ),
            (
                Mode::Thorough,
                "Thorough",
                "Writes every region first, then verifies. Also detects drives that wrap addresses.",
                None,
            ),
        ];
        for (mode, title, desc, tag) in modes {
            if self.mode_card(ui, mode, title, desc, tag).clicked() {
                self.mode = mode;
            }
        }
        ui.add_enabled(self.mode.writes(), egui::Checkbox::new(&mut self.calibrate, "Calibration warm-up"))
            .on_hover_text("Warms the drive up with growing transfer sizes before the timed run.");
    }

    fn mode_card(
        &self,
        ui: &mut egui::Ui,
        mode: Mode,
        title: &str,
        desc: &str,
        tag: Option<(&str, Color32)>,
    ) -> egui::Response {
        let selected = self.mode == mode;
        let mut prepared = egui::Frame::new()
            .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .begin(ui);
        {
            let ui = &mut prepared.content_ui;
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let p = ui.painter();
                if selected {
                    p.circle_filled(r.center(), 7.0, palette::ACCENT);
                    p.circle_filled(r.center(), 3.0, palette::BG);
                } else {
                    p.circle_stroke(r.center(), 6.5, Stroke::new(1.5, palette::BORDER_STRONG));
                }
                ui.label(RichText::new(title).strong().color(if selected {
                    palette::TEXT
                } else {
                    palette::TEXT_WEAK
                }));
                if let Some((t, c)) = tag {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| chip(ui, t, c));
                }
            });
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(RichText::new(desc).size(12.0).color(palette::TEXT_WEAK)).wrap());
        }
        let rect = prepared.frame.widget_rect(prepared.content_ui.min_rect());
        let resp = ui.interact(rect, ui.scope_id().with(("mode-card", mode.as_str())), egui::Sense::click());
        let hovered = resp.hovered() && ui.is_enabled();
        let (fill, stroke) = if selected {
            (theme::mix(palette::SURFACE, palette::ACCENT, 0.07), theme::mix(palette::BG, palette::ACCENT, 0.55))
        } else if hovered {
            (palette::WIDGET, palette::BORDER_STRONG)
        } else {
            (palette::SUNKEN, palette::BORDER)
        };
        prepared.frame.fill = fill;
        prepared.frame.stroke = Stroke::new(1.0, stroke);
        prepared.end(ui);
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        let size = egui::vec2(ui.available_width(), 40.0);
        if self.running() {
            let cancelling = self.worker.as_ref().is_some_and(Worker::is_cancelling);
            let label = if cancelling { "Stopping… restoring data" } else { "Stop test" };
            let stop = egui::Button::new(RichText::new(label).strong().color(palette::BAD))
                .fill(theme::mix(palette::BG, palette::BAD, 0.10))
                .stroke(Stroke::new(1.0, theme::mix(palette::BG, palette::BAD, 0.45)))
                .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS))
                .min_size(size);
            if ui.add_enabled(!cancelling, stop).clicked() {
                if let Some(w) = &self.worker {
                    w.cancel();
                }
            }
        } else {
            let can_start = self.selected_device().is_some();
            if ui.add_enabled(can_start, primary_button("Validate this drive").min_size(size)).clicked() {
                self.confirm = true;
            }
        }
        if let Some((msg, is_err)) = &self.notice {
            ui.add_space(2.0);
            ui.label(RichText::new(msg).size(12.0).color(if *is_err { palette::BAD } else { palette::TEXT_WEAK }));
        } else if !self.running() {
            let hint = if self.mode.writes() {
                "Your data is backed up and restored after the test."
            } else {
                "Read-only: nothing is written to the drive."
            };
            ui.vertical_centered(|ui| ui.label(RichText::new(hint).size(12.0).color(palette::TEXT_FAINT)));
        }
    }

    // ---------------------------------------------------------------- dialogs

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
        let frame = egui::Frame::popup(&ctx.global_style())
            .inner_margin(egui::Margin::same(22))
            .corner_radius(CornerRadius::same(theme::CARD_RADIUS + 4));
        let modal = egui::Modal::new(egui::Id::unique("flashvalidator-confirm")).frame(frame).show(&ctx, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new("Validate this drive?").size(19.0).strong());
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(palette::SUNKEN)
                .stroke(Stroke::new(1.0, palette::BORDER))
                .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(dev.label()).strong()).truncate());
                    });
                    ui.label(
                        RichText::new(format!("{} mode", capitalize(mode.as_str())))
                            .size(12.5)
                            .color(palette::TEXT_WEAK),
                    );
                });
            ui.add_space(4.0);
            if mode.writes() {
                ui.label(
                    RichText::new(
                        "FlashValidator will write test data to 576 places on the drive and put the original data \
                         back. It is designed to preserve your files, but a failing drive could still lose data. \
                         Back up anything important first, or choose Read-only mode.",
                    )
                    .color(palette::TEXT_WEAK),
                );
                if !dev.mount_points.is_empty() {
                    #[cfg(windows)]
                    let msg = "Its volumes will be dismounted and the disk taken offline during the test.";
                    #[cfg(not(windows))]
                    let msg = "Unmount all of its partitions first, or the test cannot start.";
                    callout(ui, palette::WARN, msg);
                }
            } else {
                ui.label(
                    RichText::new(
                        "Read-only mode never writes to the drive, so your data cannot be changed. \
                         It finds read errors, unstable reads and address wrap-around, but it cannot prove \
                         that the declared capacity is real.",
                    )
                    .color(palette::TEXT_WEAK),
                );
            }
            if !dev.is_usb() {
                callout(ui, palette::BAD, "This is NOT a USB drive. Make sure it is not a system disk.");
            }
            if needs_auth {
                ui.label(
                    RichText::new(if cfg!(windows) {
                        "Windows will ask for administrator permission to access the drive."
                    } else {
                        "You will be asked for your password to access the drive."
                    })
                    .size(12.5)
                    .color(palette::TEXT_FAINT),
                );
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary_button("Start").min_size(egui::vec2(96.0, 34.0))).clicked() {
                    start = true;
                }
                if ui.add(secondary_button("Cancel").min_size(egui::vec2(96.0, 34.0))).clicked() {
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

    // ------------------------------------------------------------- main area

    fn welcome(&self, ui: &mut egui::Ui) {
        let top = ((ui.available_height() - 420.0) / 2.0).clamp(24.0, 140.0);
        ui.add_space(top);
        ui.vertical_centered(|ui| {
            ui.set_max_width(ui.available_width().min(620.0));
            ui.add(self.logo.image(72.0));
            ui.add_space(10.0);
            ui.label(RichText::new("Is your USB drive real?").size(26.0).strong().color(palette::TEXT));
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "FlashValidator checks 576 regions spread across the drive's declared capacity to find \
                     counterfeit (fake-capacity) and failing drives, without erasing your files.",
                )
                .size(14.5)
                .color(palette::TEXT_WEAK),
            );
            ui.add_space(22.0);
        });
        let features = [
            ("576 regions", "Spot-checks the whole declared capacity in minutes, not hours."),
            ("Data preserved", "Every block is backed up and restored, with a crash-recovery journal."),
            ("Read-only option", "Read-only mode never writes to the drive at all."),
        ];
        let width = ui.available_width().min(760.0);
        let indent = (ui.available_width() - width) / 2.0;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.allocate_ui_with_layout(egui::vec2(width, 120.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.columns(features.len(), |cols| {
                    for (col, (title, text)) in cols.iter_mut().zip(features) {
                        col.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.set_min_height(84.0);
                                ui.label(RichText::new(title).strong().color(palette::TEXT));
                                ui.label(RichText::new(text).size(12.5).color(palette::TEXT_WEAK));
                            });
                        });
                    }
                });
            });
        });
        ui.add_space(22.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Choose a drive and a test mode on the left, then press \"Validate this drive\".")
                    .color(palette::TEXT_FAINT),
            );
        });
    }

    fn run_card(ui: &mut egui::Ui, view: &RunView, running: bool) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (label, color) = if running {
                    ("Running", palette::ACCENT)
                } else if view.error.is_some() {
                    ("Could not run", palette::BAD)
                } else if let Some(r) = &view.report {
                    let (t, c, _) = verdict_style(r.verdict);
                    (t, c)
                } else {
                    ("Idle", palette::TEXT_WEAK)
                };
                ui.add(egui::Label::new(RichText::new(&view.device_label).strong().size(15.0)).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| chip(ui, label, color));
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (dot_color, tip) = match (running, view.activity) {
                    (true, Some(Activity::Reading)) => (palette::OK, "reading"),
                    (true, Some(Activity::Writing)) => (palette::BAD, "writing"),
                    _ => (palette::TEXT_FAINT, "idle"),
                };
                dot(ui, dot_color).on_hover_text(tip);
                let text = match view.phase {
                    Some(Phase::Calibrating) if view.calibration.1 > 0 => {
                        format!("Calibrating ({}/{})", view.calibration.0, view.calibration.1)
                    }
                    Some(p) => p.label().to_owned(),
                    None if !view.status.is_empty() => view.status.clone(),
                    None => "Opening drive".to_owned(),
                };
                ui.label(RichText::new(text).color(palette::TEXT_WEAK));
                if view.mode == Mode::ReadOnly {
                    ui.label(RichText::new("·  read-only, nothing is written").color(palette::READABLE));
                }
                let (done, total) = view.progress;
                if total > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let pct = done as f32 / total as f32 * 100.0;
                        ui.label(RichText::new(format!("{pct:.0}%")).strong().color(palette::TEXT));
                    });
                }
            });
            ui.add_space(2.0);
            let (done, total) = view.progress;
            let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            progress_track(ui, frac, palette::ACCENT);
        });
    }

    fn map_card(ui: &mut egui::Ui, view: &RunView) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(caption("Region map"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} regions · hover a cell for details", view.statuses.len()))
                            .size(12.0)
                            .color(palette::TEXT_FAINT),
                    );
                });
            });
            ui.add_space(4.0);
            map::region_map(ui, &view.statuses, &view.offsets, view.block_size);
            ui.add_space(8.0);
            map::legend(ui, &view.statuses, view.mode == Mode::ReadOnly);
        });
    }

    fn report_panel(&mut self, ui: &mut egui::Ui) {
        let Some(view) = &self.view else { return };
        if let Some(err) = &view.error {
            theme::tinted_card(palette::BAD).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("The test could not run").size(17.0).strong().color(palette::BAD));
                ui.label(RichText::new(err).color(palette::TEXT));
            });
            return;
        }
        let Some(r) = &view.report else { return };

        let (title, color, detail) = verdict_style(r.verdict);
        theme::tinted_card(color).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(4.0, 44.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(2), color);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).size(20.0).strong().color(color));
                    ui.add(egui::Label::new(RichText::new(detail).color(palette::TEXT)).wrap());
                });
            });
        });

        ui.add_space(4.0);
        let size_label = if r.mode == Mode::ReadOnly { "Readable size" } else { "Validated size" };
        let size_color = if r.validated_size >= r.declared_size {
            palette::TEXT
        } else if r.verdict.is_failure() {
            palette::BAD
        } else {
            palette::WARN
        };
        ui.columns(4, |cols| {
            stat_tile(
                &mut cols[0],
                "Declared size",
                human_bytes(r.declared_size),
                Some(bytes_detailed(r.declared_size)),
                palette::TEXT,
            );
            stat_tile(
                &mut cols[1],
                size_label,
                human_bytes(r.validated_size),
                Some(bytes_detailed(r.validated_size)),
                size_color,
            );
            stat_tile(
                &mut cols[2],
                "Highest good",
                r.highest_valid_region.map_or("none".into(), |i| format!("#{}", i + 1)),
                r.highest_valid_region.map(|_| format!("Ends at {}", human_bytes(r.highest_valid_end))),
                palette::TEXT,
            );
            stat_tile(&mut cols[3], "Duration", format!("{:.1} s", r.duration_secs), None, palette::TEXT);
        });

        ui.add_space(4.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(caption("Details"));
            egui::Grid::new("summary").num_columns(2).spacing([28.0, 6.0]).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).color(palette::TEXT_WEAK));
                    ui.label(v);
                    ui.end_row();
                };
                row(
                    "Vendor / product",
                    format!(
                        "{} {}",
                        r.device.vendor.as_deref().unwrap_or("-"),
                        r.device.model.as_deref().unwrap_or("")
                    ),
                );
                row("Serial number", r.device.serial.clone().unwrap_or_else(|| "-".into()));
                row("Mode", capitalize(r.mode.as_str()));
                row(
                    "Highest good region",
                    r.highest_valid_region.map_or("none".into(), |i| {
                        format!("#{} (ends at {})", i + 1, human_bytes(r.highest_valid_end))
                    }),
                );
                if r.wraparound_detected {
                    row("Wrap-around", "detected (high addresses alias low ones)".into());
                }
            });
        });

        ui.add_space(4.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(caption("Performance")).default_open(true).show(ui, |ui| perf_table(ui, r));
        });

        if !r.warnings.is_empty() || r.journal.is_some() || !r.unrestored_offsets.is_empty() {
            ui.add_space(4.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(caption("Notes"));
                if !r.unrestored_offsets.is_empty() {
                    callout(
                        ui,
                        palette::BAD,
                        &format!("Original data could not be restored at {} place(s).", r.unrestored_offsets.len()),
                    );
                }
                if let Some(j) = &r.journal {
                    ui.label(format!("Backup journal kept at {}", j.display()));
                    egui::Frame::new()
                        .fill(palette::SUNKEN)
                        .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS))
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("flashvalidator restore \"{}\" {}", j.display(), r.device.path))
                                    .monospace(),
                            );
                        });
                }
                for w in &r.warnings {
                    ui.label(RichText::new(format!("•  {w}")).color(palette::TEXT_WEAK));
                }
            });
        }

        ui.add_space(8.0);
        let mut save = false;
        ui.horizontal(|ui| {
            if ui.add(secondary_button("Copy report")).clicked() {
                ui.ctx().copy_text(r.to_string());
            }
            if ui.add(secondary_button("Save report")).clicked() {
                save = true;
            }
        });
        if save {
            self.save_report();
        }
    }
}

/// A tinted one-line warning box.
fn callout(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(theme::mix(palette::BG, color, 0.08))
        .stroke(Stroke::new(1.0, theme::mix(palette::BG, color, 0.35)))
        .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(color));
        });
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

fn perf_table(ui: &mut egui::Ui, r: &Report) {
    let fmt = |s: &Option<Summary>, f: fn(&Summary) -> f64| s.as_ref().map_or("-".into(), |s| format!("{:.0}", f(s)));
    let head = |t: &str| RichText::new(t).strong().color(palette::TEXT);
    let key = |t: &str| RichText::new(t).color(palette::TEXT_WEAK);
    egui::Grid::new("perf").num_columns(3).striped(true).spacing([40.0, 6.0]).min_col_width(80.0).show(ui, |ui| {
        ui.label(RichText::new("microseconds").size(12.0).color(palette::TEXT_FAINT));
        ui.label(head("Read"));
        ui.label(head("Write"));
        ui.end_row();
        let samples = |s: &Option<Summary>| s.as_ref().map_or("-".into(), |s| s.samples.to_string());
        ui.label(key("samples"));
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
            ui.label(key(name));
            ui.label(fmt(&r.read_stats, f));
            ui.label(fmt(&r.write_stats, f));
            ui.end_row();
        }
        let tput = |s: &Option<Summary>| {
            s.as_ref().map_or("-".into(), |s| format!("{}/s", human_bytes(s.throughput(r.block_size) as u64)))
        };
        ui.label(key("throughput"));
        ui.label(RichText::new(tput(&r.read_stats)).strong());
        ui.label(RichText::new(tput(&r.write_stats)).strong());
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
        let top = egui::Frame::new().fill(palette::BG).inner_margin(egui::Margin::symmetric(18, 10));
        egui::Panel::top("top").frame(top).show(ui, |ui| self.header(ui));

        let side = egui::Frame::new().fill(palette::SURFACE);
        egui::Panel::left("sidebar")
            .frame(side)
            .resizable(false)
            .exact_size(SIDEBAR_WIDTH)
            .show(ui, |ui| self.sidebar(ui));

        let running = self.running();
        let central = egui::Frame::new().fill(palette::BG).inner_margin(egui::Margin::symmetric(22, 18));
        egui::CentralPanel::default().frame(central).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if let Some(view) = &self.view {
                    Self::run_card(ui, view, running);
                    ui.add_space(4.0);
                    Self::map_card(ui, view);
                    ui.add_space(4.0);
                    self.report_panel(ui);
                } else {
                    self.welcome(ui);
                }
            });
        });
        self.confirm_dialog(ui);
    }
}
