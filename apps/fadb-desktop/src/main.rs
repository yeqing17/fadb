#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod i18n;
mod panels;
mod platform;
mod quick_commands;
mod runtime;
mod theme;
mod wireless;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use app::FadbApp;
use eframe::egui;
use tracing_subscriber::{EnvFilter, prelude::*};

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

    init_logging();

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

/// Path of the rolling log file: `%APPDATA%\Fadb\logs\fadb.log`, or the temp
/// directory when the profile has no roaming AppData (portable/CI runs).
fn log_file_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("Fadb")
        .join("logs")
        .join("fadb.log")
}

/// Installs tracing: stderr for terminal runs plus the log file. Release builds
/// are `windows_subsystem = "windows"` and own no console, so without the file
/// sink every warning about a device we do not have on the desk is lost.
fn init_logging() {
    let filter =
        EnvFilter::from_default_env().add_directive("fadb=info".parse().expect("valid directive"));
    let console = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);
    let file = tracing_subscriber::fmt::layer()
        .with_writer(LogFile::open(log_file_path()))
        .with_target(false)
        .with_ansi(false);
    tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file)
        .init();
}

/// Largest log file kept between restarts; a normal session appends a few KiB,
/// so this is weeks of history.
const LOG_FILE_LIMIT: u64 = 4 * 1024 * 1024;

/// Append-only log sink shared by all writers. A failed open or write degrades
/// to dropping output: logging must never take the app down.
#[derive(Clone)]
struct LogFile {
    handle: Arc<Mutex<Option<std::fs::File>>>,
}

impl LogFile {
    fn open(path: PathBuf) -> Self {
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > LOG_FILE_LIMIT) {
            // Keep exactly one previous run's worth of history.
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        Self {
            handle: Arc::new(Mutex::new(file)),
        }
    }
}

impl std::io::Write for LogFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Ok(mut handle) = self.handle.lock() else {
            return Ok(buffer.len());
        };
        match handle.as_mut() {
            Some(file) => file.write(buffer),
            None => Ok(buffer.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let Ok(mut handle) = self.handle.lock() else {
            return Ok(());
        };
        if let Some(file) = handle.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogFile {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}
