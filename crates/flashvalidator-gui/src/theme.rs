//! Pure-black dark theme with a small, related accent palette and subtle corners.

use eframe::egui::{self, Color32, CornerRadius, Stroke, epaint::Shadow};
use flashvalidator_core::report::RegionStatus;

/// Colours used throughout the interface.
pub mod palette {
    use super::Color32;

    /// Window background.
    pub const BG: Color32 = Color32::from_rgb(0, 0, 0);
    /// Raised surfaces (dialogs, popups).
    pub const SURFACE: Color32 = Color32::from_rgb(10, 10, 12);
    /// Input and track backgrounds.
    pub const SUNKEN: Color32 = Color32::from_rgb(14, 14, 17);
    /// Idle widget fill.
    pub const WIDGET: Color32 = Color32::from_rgb(20, 20, 24);
    /// Hovered widget fill.
    pub const WIDGET_HOVER: Color32 = Color32::from_rgb(28, 28, 33);
    /// Pressed widget fill.
    pub const WIDGET_ACTIVE: Color32 = Color32::from_rgb(36, 36, 42);
    /// Hairline borders.
    pub const BORDER: Color32 = Color32::from_rgb(38, 38, 44);

    /// Primary text.
    pub const TEXT: Color32 = Color32::from_rgb(230, 230, 232);
    /// Secondary text.
    pub const TEXT_WEAK: Color32 = Color32::from_rgb(132, 132, 142);

    /// Accent (teal): selection, progress, focus.
    pub const ACCENT: Color32 = Color32::from_rgb(20, 184, 166);
    /// Darker accent for filled surfaces.
    pub const ACCENT_DIM: Color32 = Color32::from_rgb(15, 118, 110);
    /// Light accent for strokes on accent fills.
    pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(94, 234, 212);

    /// Validated / pass.
    pub const OK: Color32 = Color32::from_rgb(34, 197, 94);
    /// Readable (read-only mode, unverified).
    pub const READABLE: Color32 = Color32::from_rgb(56, 189, 248);
    /// No storage / fail.
    pub const BAD: Color32 = Color32::from_rgb(239, 68, 68);
    /// Read error / warnings.
    pub const WARN: Color32 = Color32::from_rgb(245, 158, 11);
    /// Write error.
    pub const WRITE_ERR: Color32 = Color32::from_rgb(168, 85, 247);
    /// Untested cell.
    pub const PENDING: Color32 = Color32::from_rgb(28, 28, 33);
}

/// Cell / swatch colour for a region status.
pub fn status_color(status: RegionStatus) -> Color32 {
    match status {
        RegionStatus::Pending => palette::PENDING,
        RegionStatus::Ok => palette::OK,
        RegionStatus::Readable => palette::READABLE,
        RegionStatus::ReadError => palette::WARN,
        RegionStatus::WriteError => palette::WRITE_ERR,
        RegionStatus::Mismatch => palette::BAD,
    }
}

/// Installs the theme on `ctx` (dark only).
pub fn apply(ctx: &egui::Context) {
    use palette::*;

    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    let small = CornerRadius::same(2);

    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(4);
    v.menu_corner_radius = CornerRadius::same(3);
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.extreme_bg_color = SUNKEN;
    v.faint_bg_color = Color32::from_rgb(9, 9, 11);
    v.code_bg_color = SUNKEN;
    v.hyperlink_color = ACCENT_LIGHT;
    v.warn_fg_color = WARN;
    v.error_fg_color = BAD;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT_LIGHT);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG;
    w.noninteractive.weak_bg_fill = BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    for (wv, fill, stroke) in [
        (&mut w.inactive, WIDGET, BORDER),
        (&mut w.hovered, WIDGET_HOVER, ACCENT_DIM),
        (&mut w.active, WIDGET_ACTIVE, ACCENT),
        (&mut w.open, WIDGET_ACTIVE, ACCENT_DIM),
    ] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.bg_stroke = Stroke::new(1.0, stroke);
        wv.expansion = 0.0;
    }
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        wv.corner_radius = small;
    }

    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
    });
}
