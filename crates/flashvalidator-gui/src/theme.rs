//! Pure-black monochrome theme: layered greys, one teal accent, status colours
//! reserved for results, and soft rounded corners.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, epaint::Shadow};
use flashvalidator_core::report::RegionStatus;

/// Colours used throughout the interface.
pub mod palette {
    use super::Color32;

    /// Window background.
    pub const BG: Color32 = Color32::from_rgb(0, 0, 0);
    /// Side bar and raised surfaces (cards, dialogs, popups).
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
    /// Borders of hovered or selected surfaces.
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(64, 64, 72);

    /// Primary text.
    pub const TEXT: Color32 = Color32::from_rgb(230, 230, 232);
    /// Secondary text.
    pub const TEXT_WEAK: Color32 = Color32::from_rgb(132, 132, 142);
    /// Tertiary text: section captions, units.
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(88, 88, 98);

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

/// Corner radius of cards and dialogs.
pub const CARD_RADIUS: u8 = 8;
/// Corner radius of buttons and inputs.
pub const WIDGET_RADIUS: u8 = 6;

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

/// Opaque mix of `base` towards `color` by `t` (0 = base, 1 = color).
/// Used for tinted backgrounds and borders on the black window.
pub fn mix(base: Color32, color: Color32, t: f32) -> Color32 {
    let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(f(base.r(), color.r()), f(base.g(), color.g()), f(base.b(), color.b()))
}

/// A raised card: surface fill, hairline border, rounded corners.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(palette::SURFACE)
        .stroke(Stroke::new(1.0, palette::BORDER))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(egui::Margin::same(16))
}

/// A card tinted with a status colour (verdict banners, warnings).
pub fn tinted_card(color: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(mix(palette::BG, color, 0.07))
        .stroke(Stroke::new(1.0, mix(palette::BG, color, 0.35)))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(egui::Margin::symmetric(14, 12))
}

/// Small upper-case section caption.
pub fn caption(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase()).size(11.0).color(palette::TEXT_FAINT).strong().extra_letter_spacing(0.8)
}

/// Installs the theme on `ctx` (dark only).
pub fn apply(ctx: &egui::Context) {
    use palette::*;

    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    let widget_radius = CornerRadius::same(WIDGET_RADIUS);

    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS + 2);
    v.menu_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.window_shadow = Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(200) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(180) };
    v.extreme_bg_color = SUNKEN;
    v.faint_bg_color = Color32::from_rgb(13, 13, 16);
    v.code_bg_color = SUNKEN;
    v.hyperlink_color = ACCENT_LIGHT;
    v.warn_fg_color = WARN;
    v.error_fg_color = BAD;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT_LIGHT);
    v.striped = false;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG;
    w.noninteractive.weak_bg_fill = BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    for (wv, fill, stroke) in [
        (&mut w.inactive, WIDGET, BORDER),
        (&mut w.hovered, WIDGET_HOVER, BORDER_STRONG),
        (&mut w.active, WIDGET_ACTIVE, ACCENT_DIM),
        (&mut w.open, WIDGET_ACTIVE, BORDER_STRONG),
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
        wv.corner_radius = widget_radius;
    }

    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.combo_height = 300.0;
        s.spacing.icon_width = 16.0;
        s.spacing.icon_width_inner = 8.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(20);
        s.spacing.scroll.floating = true;
        s.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(11.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));
    });
}
