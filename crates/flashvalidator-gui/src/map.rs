//! The 576-cell region map widget.

use crate::theme::{palette, status_color};
use eframe::egui::{self, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use flashvalidator_core::format::{bytes_detailed, human_bytes_binary};
use flashvalidator_core::report::RegionStatus;

/// Number of columns in the map (576 = 32 x 18).
pub const COLUMNS: usize = 32;

/// Draws the map, centred in the available width; hovering a cell shows its
/// offset and status.
pub fn region_map(ui: &mut egui::Ui, statuses: &[RegionStatus], offsets: &[u64], block_size: usize) {
    let n = statuses.len().max(1);
    let rows = n.div_ceil(COLUMNS);
    let gap = 4.0;
    let width = ui.available_width();
    let cell = ((width - gap * (COLUMNS as f32 - 1.0)) / COLUMNS as f32).clamp(8.0, 22.0).floor();
    let size =
        Vec2::new(cell * COLUMNS as f32 + gap * (COLUMNS as f32 - 1.0), cell * rows as f32 + gap * (rows as f32 - 1.0));
    let indent = ((width - size.x) / 2.0).max(0.0);
    let (outer, response) = ui.allocate_exact_size(Vec2::new(width, size.y), Sense::hover());
    let rect = Rect::from_min_size(outer.min + Vec2::new(indent, 0.0), size);
    let painter = ui.painter_at(rect.expand(3.0));
    let radius = CornerRadius::same((cell / 5.0).round().clamp(1.0, 4.0) as u8);

    let cell_rect = |i: usize| {
        let (r, c) = (i / COLUMNS, i % COLUMNS);
        Rect::from_min_size(rect.min + Vec2::new(c as f32 * (cell + gap), r as f32 * (cell + gap)), Vec2::splat(cell))
    };
    for (i, &s) in statuses.iter().enumerate() {
        painter.rect_filled(cell_rect(i), radius, status_color(s));
    }

    if let Some(pos) = response.hover_pos() {
        let rel = pos - rect.min;
        if rel.x >= 0.0 && rel.y >= 0.0 {
            let (c, r) = ((rel.x / (cell + gap)) as usize, (rel.y / (cell + gap)) as usize);
            let i = r * COLUMNS + c;
            if c < COLUMNS && i < statuses.len() && cell_rect(i).contains(pos) {
                painter.rect_stroke(
                    cell_rect(i).expand(1.0),
                    radius,
                    Stroke::new(1.5, palette::TEXT),
                    StrokeKind::Outside,
                );
                response.on_hover_ui_at_pointer(|ui| {
                    ui.horizontal(|ui| {
                        swatch(ui, statuses[i], 10.0);
                        ui.strong(format!("Region {} of {}", i + 1, statuses.len()));
                    });
                    if let Some(&off) = offsets.get(i) {
                        ui.label(RichText::new(format!("Offset  {}", bytes_detailed(off))).color(palette::TEXT_WEAK));
                        ui.label(
                            RichText::new(format!("Size  {}", human_bytes_binary(block_size as u64)))
                                .color(palette::TEXT_WEAK),
                        );
                    }
                    ui.label(format!("Status: {}", statuses[i].label()));
                });
            }
        }
    }
}

fn swatch(ui: &mut egui::Ui, status: RegionStatus, size: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(2), status_color(status));
}

/// Pill-shaped chips with a colour swatch, label and count. Statuses that
/// cannot occur in the current mode are hidden unless present.
pub fn legend(ui: &mut egui::Ui, statuses: &[RegionStatus], read_only: bool) {
    let all = [
        RegionStatus::Ok,
        RegionStatus::Readable,
        RegionStatus::Mismatch,
        RegionStatus::ReadError,
        RegionStatus::WriteError,
        RegionStatus::Pending,
    ];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for s in all {
            let count = statuses.iter().filter(|&&x| x == s).count();
            let relevant = match s {
                RegionStatus::Ok | RegionStatus::WriteError => !read_only,
                RegionStatus::Readable => read_only,
                _ => true,
            };
            if !relevant && count == 0 {
                continue;
            }
            legend_chip(ui, s, count);
        }
    });
}

/// One legend chip, painted directly so `horizontal_wrapped` can wrap it.
fn legend_chip(ui: &mut egui::Ui, status: RegionStatus, count: usize) {
    let active = count > 0;
    let font = egui::FontId::proportional(12.5);
    let label_color = if active { palette::TEXT_WEAK } else { palette::TEXT_FAINT };
    let count_color = if active { palette::TEXT } else { palette::TEXT_FAINT };
    let painter = ui.painter();
    let label = painter.layout_no_wrap(status.label().to_owned(), font.clone(), label_color);
    let number = painter.layout_no_wrap(count.to_string(), font, count_color);
    let (pad, swatch, gap) = (10.0, 9.0, 6.0);
    let height = 24.0;
    let width = pad + swatch + gap + label.size().x + gap + number.size().x + pad;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(255),
        palette::SUNKEN,
        Stroke::new(1.0, if active { palette::BORDER } else { palette::SURFACE }),
        StrokeKind::Inside,
    );
    let cy = rect.center().y;
    let sw = Rect::from_center_size(egui::pos2(rect.left() + pad + swatch / 2.0, cy), Vec2::splat(swatch));
    painter.rect_filled(sw, CornerRadius::same(2), status_color(status));
    let x = sw.right() + gap;
    painter.galley(egui::pos2(x, cy - label.size().y / 2.0), label.clone(), label_color);
    let x = x + label.size().x + gap;
    painter.galley(egui::pos2(x, cy - number.size().y / 2.0), number, count_color);
}
