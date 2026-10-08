#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod i18n;
mod panels;
mod platform;
mod quick_commands;
mod runtime;
mod theme;
mod wireless;

use app::FadbApp;
use eframe::egui;
use tracing_subscriber::EnvFilter;

/// Size the window opens at, also the recovery target when persisted state
/// carries a degenerate window rectangle (see `heal_window_size`).
pub const DEFAULT_WINDOW_SIZE: egui::Vec2 = egui::vec2(1180.0, 760.0);
/// Smallest window size the seam-drag resize allows.
pub const MIN_WINDOW_SIZE: egui::Vec2 = egui::vec2(900.0, 600.0);

fn main() -> eframe::Result<()> {
    // Leftover from a previous in-app update swap: the previous binary is
    // kept as `.old` only so the running exe can be renamed over; once we are
    // the fresh process it is dead weight.
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("fadb=info".parse().expect("valid directive")),
        )
        .with_target(false)
        .init();

    let native_options = eframe::NativeOptions {
        // Undecorated: the app draws its own title bar (drag to move, window
        // controls in the top bar) and edge seams for resizing.
        viewport: egui::ViewportBuilder::default()
            .with_title("Fadb")
            .with_icon(app_icon())
            .with_decorations(false)
            .with_inner_size([DEFAULT_WINDOW_SIZE.x, DEFAULT_WINDOW_SIZE.y])
            .with_min_inner_size([MIN_WINDOW_SIZE.x, MIN_WINDOW_SIZE.y]),
        ..Default::default()
    };

    eframe::run_native(
        "Fadb",
        native_options,
        Box::new(|creation_context| Ok(Box::new(FadbApp::new(creation_context)))),
    )
}

/// Window/taskbar icon, embedded at compile time from the logo set in
/// `assets/` (taskbar, Alt+Tab; the exe file icon comes from `build.rs`).
fn app_icon() -> egui::IconData {
    let png = include_bytes!("../assets/icon-256.png");
    let img = image::load_from_memory(png)
        .expect("embedded icon is a valid PNG")
        .to_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        width,
        height,
        rgba: img.into_raw(),
    }
}
