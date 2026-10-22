//! TextCut – text-based video editing with automatic silence removal.

// No extra console window on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod export;
mod media;
mod preview;
mod project;
mod render;
mod settings;
mod subtitles;
mod transcribe;
mod ui;
mod util;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Optional: open a project or media file passed as an argument.
    let open = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TextCut")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("TextCut", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}
