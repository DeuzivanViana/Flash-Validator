//! FlashValidator graphical interface (eframe/egui).
//!
//! The window runs with normal user rights. Raw drive access is requested
//! when a test starts (see [`helper`]).

// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod helper;
#[path = "../../../assets/icon_art.rs"]
mod icon_art;
mod map;
mod theme;
mod worker;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(helper::HELPER_FLAG) {
        std::process::exit(helper::run(&args[2..]));
    }

    const ICON_SIZE: u32 = 64;
    let icon = eframe::egui::IconData { rgba: icon_art::icon_rgba(ICON_SIZE), width: ICON_SIZE, height: ICON_SIZE };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("FlashValidator")
            .with_app_id("flashvalidator")
            .with_icon(icon)
            .with_inner_size([960.0, 760.0])
            .with_min_inner_size([720.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("FlashValidator", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
