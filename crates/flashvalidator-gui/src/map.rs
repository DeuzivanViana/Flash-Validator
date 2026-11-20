//! The 576-cell region map widget.

use crate::theme::{palette, status_color};
use eframe::egui::{self, Rect, Sense, Stroke, StrokeKind, Vec2};
use flashvalidator_core::format::{bytes_detailed, human_bytes_binary};
use flashvalidator_core::report::RegionStatus;

/// Number of columns in the map (576 = 32 x 18).
pub const COLUMNS: usize = 32;

/// Draws the map; hovering a cell shows its offset and status.
pub fn region_map(ui: &mut egui::Ui, statuses: &[RegionStatus], offsets: &[u64], block_size: usize) {
    let n = statuses.len().max(1);
    let rows = n.div_ceil(COLUMNS);
    let gap = 3.0;
    let width = ui.available_width();
    let cell = ((width - gap * (COLUMNS as f32 - 1.0)) / COLUMNS as f32).clamp(8.0, 16.0);
    let size =
        Vec2::new(cell * COLUMNS as f32 + gap * (COLUMNS as f32 - 1.0), cell * rows as f32 + gap * (rows as f32 - 1.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect.expand(2.0));

    let cell_rect = |i: usize| {
        let (r, c) = (i / COLUMNS, i % COLUMNS);
        Rect::from_min_size(rect.min + Vec2::new(c as f32 * (cell + gap), r as f32 * (cell + gap)), Vec2::splat(cell))
    };
    for (i, &s) in statuses.iter().enumerate() {
        painter.rect_filled(cell_rect(i), 1.0, status_color(s));
    }

    if let Some(pos) = response.hover_pos() {
        let rel = pos - rect.min;
        let (c, r) = ((rel.x / (cell + gap)) as usize, (rel.y / (cell + gap)) as usize);
        let i = r * COLUMNS + c;
        if c < COLUMNS && i < statuses.len() && cell_rect(i).contains(pos) {
            painter.rect_stroke(cell_rect(i).expand(1.0), 1.0, Stroke::new(1.5, palette::TEXT), StrokeKind::Outside);
            response.on_hover_ui_at_pointer(|ui| {
                ui.strong(format!("Region {} of {}", i + 1, statuses.len()));
                if let Some(&off) = offsets.get(i) {
                    ui.label(format!("Offset: {}", bytes_detailed(off)));
                    ui.label(format!("Size: {}", human_bytes_binary(block_size as u64)));
                }
                ui.label(format!("Status: {}", statuses[i].label()));
            });
        }
    }
}

/// Colour swatches with labels and counts. Statuses that cannot occur in
/// the current mode are hidden unless present.
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
            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
            ui.painter().rect_filled(r, 1.0, status_color(s));
            ui.label(egui::RichText::new(format!("{} {count}", s.label())).color(palette::TEXT_WEAK));
            ui.add_space(10.0);
        }
    });
}
