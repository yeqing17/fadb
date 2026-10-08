use std::path::PathBuf;

use eframe::egui;
use fadb_domain::{
    BackendCommand, BackendEvent, BridgeError, DeviceTarget, ErrorCode, FileTransferDirection,
    OperationId, OverwritePolicy, RemoteFileEntry, RemoteFileKind, RemotePath,
};

use crate::i18n::{Language, error_text, text};

#[derive(Clone)]
struct TransferIntent {
    direction: FileTransferDirection,
    target: DeviceTarget,
    local_path: PathBuf,
    remote_path: RemotePath,
}

#[derive(Clone, Copy)]
enum MutationModalKind {
    CreateDirectory,
    Rename,
    Delete,
}

#[derive(Clone)]
struct MutationModal {
    kind: MutationModalKind,
    input: String,
    entry: Option<RemoteFileEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum SortKey {
    Name,
    #[default]
    Modified,
    Size,
}

/// Unit the files panel renders `size_bytes` in; user-selected in the
/// settings window and persisted across restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileSizeUnit {
    Bytes,
    Kilobytes,
    #[default]
    Megabytes,
    Gigabytes,
}

impl FileSizeUnit {
    pub const ALL: [Self; 4] = [
        Self::Bytes,
        Self::Kilobytes,
        Self::Megabytes,
        Self::Gigabytes,
    ];

    /// Storage / settings representation; also the ComboBox label (unit
    /// symbols read the same in both languages).
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Bytes => "B",
            Self::Kilobytes => "KB",
            Self::Megabytes => "MB",
            Self::Gigabytes => "GB",
        }
    }

    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|unit| unit.symbol() == symbol)
    }
}

/// Fixed-unit size rendering: the value is always divided down to the
/// chosen unit (1024-based) instead of auto-scaling, so a column stays
/// comparable row to row.
#[allow(clippy::cast_precision_loss)] // file sizes are far below f64's 2^53 exact range
fn format_size(size: u64, unit: FileSizeUnit) -> String {
    match unit {
        FileSizeUnit::Bytes => format!("{size} B"),
        FileSizeUnit::Kilobytes => format!("{:.1} KB", size as f64 / 1_024.0),
        FileSizeUnit::Megabytes => format!("{:.1} MB", size as f64 / 1_048_576.0),
        FileSizeUnit::Gigabytes => format!("{:.1} GB", size as f64 / 1_073_741_824.0),
    }
}

fn sort_header(
    ui: &mut egui::Ui,
    language: Language,
    state: &mut FilesPanelState,
    key: SortKey,
    label: &str,
) {
    // reverse=true flips the comparison to descending, so the arrow points
    // down for "large/new first" and up for the natural ascending order.
    let arrow = match (state.sort_key == key, state.sort_reverse) {
        (true, false) => " ▲",
        (true, true) => " ▼",
        (false, _) => "",
    };
    if ui
        .small_button(format!("{}{arrow}", text(language, label)))
        .clicked()
    {
        if state.sort_key == key {
            state.sort_reverse = !state.sort_reverse;
        } else {
            state.sort_key = key;
            state.sort_reverse = false;
        }
    }
}

pub struct FilesPanelState {
    target: Option<DeviceTarget>,
    directory: Option<RemotePath>,
    path_input: String,
    entries: Vec<RemoteFileEntry>,
    /// Selected entry, tracked by path: an index would silently point at a
    /// different row once sorting or the name filter changes.
    selected: Option<RemotePath>,
    history: Vec<RemotePath>,
    /// Case-insensitive name filter; cleared when navigating so a stale
    /// filter never makes a freshly loaded directory look empty.
    filter: String,
    sort_key: SortKey,
    sort_reverse: bool,
    listing_request: Option<OperationId>,
    /// Set once the automatic `/{sdcard,}` bootstrap listing failed and the
    /// panel fell back to `/`, so devices without `/sdcard` still show files.
    root_fallback_attempted: bool,
    loading: bool,
    transfer: Option<OperationId>,
    transfer_intent: Option<TransferIntent>,
    overwrite_prompt: Option<TransferIntent>,
    mutation: Option<OperationId>,
    mutation_modal: Option<MutationModal>,
    error: Option<String>,
}

impl Default for FilesPanelState {
    fn default() -> Self {
        Self {
            target: None,
            directory: None,
            path_input: String::new(),
            entries: Vec::new(),
            selected: None,
            history: Vec::new(),
            filter: String::new(),
            // Fresh listings read newest-changes-first: the folder you just
            // touched is the one you came to look for.
            sort_key: SortKey::Modified,
            sort_reverse: true,
            listing_request: None,
            root_fallback_attempted: false,
            loading: false,
            transfer: None,
            transfer_intent: None,
            overwrite_prompt: None,
            mutation: None,
            mutation_modal: None,
            error: None,
        }
    }
}

impl FilesPanelState {
    pub fn reconcile_target(&mut self, target: Option<DeviceTarget>) -> Option<BackendCommand> {
        if self.target == target {
            return None;
        }
        self.target.clone_from(&target);
        self.directory = None;
        self.path_input.clear();
        self.entries.clear();
        self.selected = None;
        self.history.clear();
        self.filter.clear();
        self.listing_request = None;
        self.root_fallback_attempted = false;
        self.loading = false;
        self.transfer = None;
        self.transfer_intent = None;
        self.overwrite_prompt = None;
        self.mutation = None;
        self.mutation_modal = None;
        self.error = None;
        target.map(|target| {
            self.list(
                target,
                RemotePath::new("/sdcard").expect("valid default path"),
            )
        })
    }

    #[allow(clippy::too_many_lines)]
    pub fn handle_event(
        &mut self,
        language: Language,
        event: &BackendEvent,
    ) -> Vec<BackendCommand> {
        let mut commands = Vec::new();
        match event {
            BackendEvent::DirectoryLoading {
                request_id,
                target,
                path,
            } if self.listing_request.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(target) =>
            {
                self.directory = Some(path.clone());
                self.path_input = path.to_string();
                self.loading = true;
                self.error = None;
            }
            BackendEvent::DirectoryLoaded {
                request_id,
                listing,
            } if self.listing_request.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(&listing.target) =>
            {
                self.directory = Some(listing.directory.clone());
                self.path_input = listing.directory.to_string();
                self.entries.clone_from(&listing.entries);
                self.selected = None;
                self.loading = false;
                self.error = None;
            }
            BackendEvent::DirectoryFailed {
                request_id,
                target,
                path,
                error,
            } if self.listing_request.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(target) =>
            {
                self.loading = false;
                self.error = Some(format_error(language, error));
                // Devices without /sdcard (some TVs and boxes) would show a
                // permanently blank panel; retry once at the root instead.
                if !self.root_fallback_attempted
                    && path.as_str() == "/sdcard"
                    && let Some(target) = self.target.clone()
                {
                    self.root_fallback_attempted = true;
                    commands.push(self.list(target, RemotePath::new("/").expect("valid path")));
                }
            }
            BackendEvent::FileTransferStarted {
                request_id, target, ..
            } if self.transfer.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(target) => {}
            BackendEvent::FileTransferCompleted {
                request_id,
                summary,
            } if self.transfer.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(&summary.target) =>
            {
                self.transfer = None;
                self.transfer_intent = None;
                if let Some(directory) = self.directory.clone() {
                    commands.push(self.list(summary.target.clone(), directory));
                }
            }
            BackendEvent::FileTransferFailed {
                request_id,
                target,
                error,
            } if self.transfer.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(target) =>
            {
                self.transfer = None;
                let intent = self.transfer_intent.take();
                if error.code == ErrorCode::AlreadyExists
                    && let Some(intent) = intent
                {
                    self.overwrite_prompt = Some(intent);
                } else {
                    self.error = Some(format_error(language, error));
                }
            }
            BackendEvent::FileTransferCancelled { request_id, target }
                if self.transfer.as_ref() == Some(request_id)
                    && self.target.as_ref() == Some(target) =>
            {
                self.transfer = None;
                self.transfer_intent = None;
            }
            BackendEvent::FileMutationStarted {
                request_id, target, ..
            } if self.mutation.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(target) => {}
            BackendEvent::FileMutationCompleted {
                request_id,
                summary,
            } if self.mutation.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(&summary.target) =>
            {
                self.mutation = None;
                self.mutation_modal = None;
                self.error = None;
                if let Some(directory) = self.directory.clone() {
                    commands.push(self.list(summary.target.clone(), directory));
                }
            }
            BackendEvent::FileMutationFailed {
                request_id,
                target,
                error,
            } if self.mutation.as_ref() == Some(request_id)
                && self.target.as_ref() == Some(target) =>
            {
                self.mutation = None;
                self.error = Some(format_error(language, error));
            }
            _ => {}
        }
        commands
    }

    /// Navigate to `path`, remembering the current directory so "Back" can return.
    fn navigate(&mut self, target: DeviceTarget, path: RemotePath) -> BackendCommand {
        if let Some(directory) = &self.directory
            && *directory != path
        {
            self.history.push(directory.clone());
        }
        self.list(target, path)
    }

    fn list(&mut self, target: DeviceTarget, path: RemotePath) -> BackendCommand {
        // Navigating to a different directory must not keep showing the
        // previous directory's rows: they would linger through the load and,
        // if the new listing fails, sit under the new path forever. Refreshes
        // of the same directory keep the old rows until the fresh ones arrive.
        if self.directory.as_ref() != Some(&path) {
            self.entries.clear();
            self.filter.clear();
        }
        self.selected = None;
        let request_id = OperationId::new();
        self.listing_request = Some(request_id);
        self.loading = true;
        BackendCommand::ListDirectory {
            request_id,
            target,
            path,
        }
    }

    fn start_transfer(
        &mut self,
        intent: TransferIntent,
        overwrite: OverwritePolicy,
    ) -> BackendCommand {
        let request_id = OperationId::new();
        let command = match intent.direction {
            FileTransferDirection::Upload => BackendCommand::UploadFile {
                request_id,
                target: intent.target.clone(),
                local_path: intent.local_path.clone(),
                remote_path: intent.remote_path.clone(),
                overwrite,
            },
            FileTransferDirection::Download => BackendCommand::DownloadFile {
                request_id,
                target: intent.target.clone(),
                remote_path: intent.remote_path.clone(),
                local_path: intent.local_path.clone(),
                overwrite,
            },
        };
        self.transfer = Some(request_id);
        self.transfer_intent = Some(intent);
        self.error = None;
        command
    }

    fn start_mutation(&mut self, command: BackendCommand) -> BackendCommand {
        self.mutation = Some(match &command {
            BackendCommand::CreateDirectory { request_id, .. }
            | BackendCommand::RenameRemoteEntry { request_id, .. }
            | BackendCommand::DeleteRemoteFile { request_id, .. } => *request_id,
            _ => unreachable!("file panel only creates mutation commands"),
        });
        self.error = None;
        command
    }

    fn selected_entry(&self) -> Option<&RemoteFileEntry> {
        let selected = self.selected.as_ref()?;
        self.entries.iter().find(|entry| &entry.path == selected)
    }
}

fn format_error(language: Language, error: &BridgeError) -> String {
    error_text(language, error)
}

/// Entries the user can browse into like a folder: plain directories and
/// symlinks that resolve to a directory (`/sdcard`, `/etc`, ...).
fn is_directory_like(entry: &RemoteFileEntry) -> bool {
    entry.kind == RemoteFileKind::Directory
        || (entry.kind == RemoteFileKind::Symlink
            && entry.target_kind == Some(RemoteFileKind::Directory))
}

fn sort_entries(entries: &mut [RemoteFileEntry], key: SortKey, reverse: bool) {
    entries.sort_by(|left, right| {
        // Folders always group first, like every desktop file manager; the
        // default view would otherwise interleave files among them. The
        // direction only flips the order inside the groups.
        let ordering = match key {
            SortKey::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            SortKey::Size => left.size_bytes.cmp(&right.size_bytes),
            SortKey::Modified => left.modified_unix_seconds.cmp(&right.modified_unix_seconds),
        };
        u8::from(!is_directory_like(left))
            .cmp(&u8::from(!is_directory_like(right)))
            .then_with(|| {
                if reverse {
                    ordering.reverse()
                } else {
                    ordering
                }
            })
    });
}

/// Break days since 1970-01-01 back into (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        u32::try_from(month).expect("month is 1..=12"),
        u32::try_from(day).expect("day is 1..=31"),
    )
}

fn format_modified_time(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        seconds_of_day / 3_600,
        seconds_of_day % 3_600 / 60,
        seconds_of_day % 60
    )
}

/// Row icon: a folder for browsable entries, a page for files, a diamond for
/// anything else, plus a small arrow marking symlinks. Painted with vectors
/// instead of emoji fonts so rows render identically on every machine.
fn kind_icon(ui: &mut egui::Ui, entry: &RemoteFileEntry) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    if is_directory_like(entry) {
        paint_folder(painter, rect);
    } else if matches!(entry.kind, RemoteFileKind::File | RemoteFileKind::Symlink) {
        paint_file(painter, rect);
    } else {
        paint_other(painter, rect);
    }
    if entry.kind == RemoteFileKind::Symlink {
        paint_link_arrow(painter, rect);
    }
}

fn paint_folder(painter: &egui::Painter, rect: egui::Rect) {
    let tab = egui::Color32::from_rgb(0xD9, 0xA1, 0x2C);
    let body = egui::Color32::from_rgb(0xFB, 0xC4, 0x4D);
    painter.rect_filled(
        egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 1.0), egui::vec2(6.5, 6.0)),
        1.5,
        tab,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            rect.left_bottom() + egui::vec2(0.0, -9.5),
            rect.right_bottom(),
        ),
        2.0,
        body,
    );
}

fn paint_file(painter: &egui::Painter, rect: egui::Rect) {
    let paper = egui::Color32::from_rgb(0xBB, 0xC7, 0xD6);
    let fold = egui::Color32::from_rgb(0x8B, 0x99, 0xAB);
    let page = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(2.0, 0.5),
        egui::vec2(10.0, 13.0),
    );
    painter.rect_filled(page, 1.5, paper);
    let tip = page.right_top();
    painter.add(egui::Shape::convex_polygon(
        vec![tip - egui::vec2(4.0, 0.0), tip, tip + egui::vec2(0.0, 4.0)],
        fold,
        egui::Stroke::NONE,
    ));
}

fn paint_other(painter: &egui::Painter, rect: egui::Rect) {
    let center = rect.center();
    let radius = 4.5;
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + egui::vec2(0.0, -radius),
            center + egui::vec2(radius, 0.0),
            center + egui::vec2(0.0, radius),
            center + egui::vec2(-radius, 0.0),
        ],
        egui::Color32::from_rgb(0x9A, 0xA5, 0xB4),
        egui::Stroke::NONE,
    ));
}

/// Small up-right arrow overlaid on symlink rows so links stay recognizable
/// even when the target is a folder and the base icon is a folder.
fn paint_link_arrow(painter: &egui::Painter, rect: egui::Rect) {
    let stroke = egui::Stroke::new(1.6, egui::Color32::from_rgb(0x5B, 0x9B, 0xE8));
    let start = egui::pos2(rect.left() + 6.0, rect.bottom() - 1.0);
    let tip = egui::pos2(rect.right() - 1.0, rect.bottom() - 6.0);
    painter.line_segment([start, tip], stroke);
    painter.line_segment([tip, tip + egui::vec2(-3.4, 0.0)], stroke);
    painter.line_segment([tip, tip + egui::vec2(0.0, 3.4)], stroke);
}

/// Folder rows never show a byte count, matching the reference layout.
fn size_label(entry: &RemoteFileEntry, size_unit: FileSizeUnit) -> String {
    if is_directory_like(entry) {
        "—".to_owned()
    } else {
        entry
            .size_bytes
            .map_or_else(|| "—".to_owned(), |size| format_size(size, size_unit))
    }
}

#[allow(clippy::too_many_lines)]
pub fn show(
    ui: &mut egui::Ui,
    language: Language,
    state: &mut FilesPanelState,
    size_unit: FileSizeUnit,
    target: Option<&DeviceTarget>,
) -> Vec<BackendCommand> {
    let mut commands = Vec::new();
    let Some(target) = target.cloned() else {
        ui.centered_and_justified(|ui| ui.label(text(language, "files_select_device")));
        return commands;
    };

    ui.horizontal(|ui| {
        if ui.button(text(language, "files_back")).clicked() {
            if let Some(previous) = state.history.pop() {
                commands.push(state.list(target.clone(), previous));
            } else if let Some(directory) = state.directory.clone() {
                commands.push(state.list(target.clone(), directory.parent()));
            }
        }
        if ui.button(text(language, "files_up")).clicked()
            && let Some(directory) = state.directory.clone()
        {
            commands.push(state.navigate(target.clone(), directory.parent()));
        }
        if ui.button(text(language, "refresh")).clicked()
            && let Some(directory) = state.directory.clone()
        {
            commands.push(state.list(target.clone(), directory));
        }
        let response =
            ui.add(egui::TextEdit::singleline(&mut state.path_input).desired_width(260.0));
        if (response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
            || ui.button(text(language, "files_go")).clicked()
        {
            match RemotePath::new(state.path_input.clone()) {
                Ok(path) => commands.push(state.navigate(target.clone(), path)),
                Err(error) => state.error = Some(format_error(language, &error)),
            }
        }
        if ui
            .add_enabled(
                state.mutation.is_none(),
                egui::Button::new(text(language, "files_new_folder")),
            )
            .clicked()
        {
            state.mutation_modal = Some(MutationModal {
                kind: MutationModalKind::CreateDirectory,
                input: String::new(),
                entry: None,
            });
        }
        if ui
            .add_enabled(
                state.transfer.is_none(),
                egui::Button::new(text(language, "files_upload")),
            )
            .clicked()
            && let Some(directory) = state.directory.clone()
            && let Some(local_path) = rfd::FileDialog::new()
                .set_title(text(language, "files_upload_dialog"))
                .pick_file()
        {
            match local_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| directory.join_component(name))
            {
                Some(Ok(remote_path)) => {
                    commands.push(state.start_transfer(
                        TransferIntent {
                            direction: FileTransferDirection::Upload,
                            target: target.clone(),
                            local_path,
                            remote_path,
                        },
                        OverwritePolicy::Deny,
                    ));
                }
                Some(Err(error)) => state.error = Some(format_error(language, &error)),
                None => {
                    state.error = Some(text(language, "files_upload_invalid_name").to_owned());
                }
            }
        }
        // adb pull follows symlinks, so links to files are downloadable too.
        let can_download = state.transfer.is_none()
            && state.selected_entry().is_some_and(|entry| {
                matches!(entry.kind, RemoteFileKind::File | RemoteFileKind::Symlink)
            });
        if ui
            .add_enabled(
                can_download,
                egui::Button::new(text(language, "files_download")),
            )
            .clicked()
            && let Some(entry) = state.selected_entry().cloned()
            && let Some(local_path) = rfd::FileDialog::new()
                .set_title(text(language, "files_download_dialog"))
                .set_file_name(&entry.name)
                .save_file()
        {
            commands.push(state.start_transfer(
                TransferIntent {
                    direction: FileTransferDirection::Download,
                    target: target.clone(),
                    local_path,
                    remote_path: entry.path,
                },
                OverwritePolicy::Deny,
            ));
        }
        if let Some(request_id) = state.transfer
            && ui.button(text(language, "files_cancel_transfer")).clicked()
        {
            commands.push(BackendCommand::CancelFileOperation(request_id));
        }
        // Name filter pinned to the far right of the toolbar.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.filter)
                    .hint_text(text(language, "files_filter"))
                    .desired_width(150.0),
            );
        });
    });
    ui.separator();
    if state.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(text(language, "files_loading"));
        });
    }
    let needle = state.filter.trim().to_lowercase();
    let mut entries: Vec<RemoteFileEntry> = state
        .entries
        .iter()
        .filter(|entry| needle.is_empty() || entry.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();
    sort_entries(&mut entries, state.sort_key, state.sort_reverse);
    // auto_shrink must be off: the default hugs the grid's content width,
    // which then truncates the last column (modification time) mid-glyph and
    // parks the scrollbar in the middle of the panel.
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new("files-grid").striped(true).show(ui, |ui| {
                sort_header(ui, language, state, SortKey::Name, "files_name");
                ui.strong(text(language, "files_permissions"));
                sort_header(ui, language, state, SortKey::Modified, "files_modified");
                ui.strong(text(language, "files_type"));
                sort_header(ui, language, state, SortKey::Size, "files_size");
                ui.end_row();
                for entry in &entries {
                    let selected = state.selected.as_ref() == Some(&entry.path);
                    let response = ui
                        .horizontal(|ui| {
                            kind_icon(ui, entry);
                            ui.selectable_label(selected, &entry.name)
                        })
                        .inner;
                    if response.clicked() {
                        state.selected = Some(entry.path.clone());
                    }
                    if response.double_clicked() && is_directory_like(entry) {
                        commands.push(state.navigate(target.clone(), entry.path.clone()));
                    }
                    response.context_menu(|ui| {
                        state.selected = Some(entry.path.clone());
                        entry_context_menu(ui, language, state, &mut commands, &target, entry);
                    });
                    ui.label(entry.permissions.as_deref().unwrap_or("—"));
                    ui.label(
                        entry
                            .modified_unix_seconds
                            .map_or_else(|| "—".to_owned(), format_modified_time),
                    );
                    ui.label(kind_label(language, entry.kind));
                    ui.label(size_label(entry, size_unit));
                    ui.end_row();
                }
            });
            // Distinguish "this folder is empty" from "the panel failed", so
            // a blank table is never mistaken for a broken listing.
            if !state.loading && state.error.is_none() && state.directory.is_some() {
                if state.entries.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(text(language, "files_empty"));
                    });
                } else if entries.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(text(language, "files_no_match"));
                    });
                }
            }
        });
    ui.horizontal(|ui| {
        let selected = state.selected_entry().cloned();
        let can_mutate = state.mutation.is_none();
        if ui
            .add_enabled(
                can_mutate && selected.is_some(),
                egui::Button::new(text(language, "files_rename")),
            )
            .clicked()
            && let Some(entry) = selected.clone()
        {
            state.mutation_modal = Some(MutationModal {
                kind: MutationModalKind::Rename,
                input: entry.name.clone(),
                entry: Some(entry),
            });
        }
        let can_delete = can_mutate && selected.is_some();
        if ui
            .add_enabled(
                can_delete,
                egui::Button::new(text(language, "files_delete")),
            )
            .clicked()
        {
            state.mutation_modal = Some(MutationModal {
                kind: MutationModalKind::Delete,
                input: String::new(),
                entry: selected.clone(),
            });
        }
    });
    if let Some(error) = &state.error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
    if state.transfer.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(text(language, "files_transferring"));
        });
    }

    if let Some(intent) = state.overwrite_prompt.clone() {
        let mut open = true;
        let (title, message) = match intent.direction {
            FileTransferDirection::Upload => (
                text(language, "files_overwrite_remote_title"),
                text(language, "files_overwrite_body")
                    .replace("{}", &intent.remote_path.to_string()),
            ),
            FileTransferDirection::Download => (
                text(language, "files_overwrite_local_title"),
                text(language, "files_overwrite_body")
                    .replace("{}", &intent.local_path.display().to_string()),
            ),
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(message);
                ui.horizontal(|ui| {
                    if ui.button(text(language, "files_replace")).clicked() {
                        state.overwrite_prompt = None;
                        commands.push(
                            state.start_transfer(intent.clone(), OverwritePolicy::ReplaceConfirmed),
                        );
                    }
                    if ui.button(text(language, "cancel")).clicked() {
                        state.overwrite_prompt = None;
                    }
                });
            });
        if !open {
            state.overwrite_prompt = None;
        }
    }

    if let Some(mut modal) = state.mutation_modal.take() {
        let kind = modal.kind;
        let title = match kind {
            MutationModalKind::CreateDirectory => text(language, "files_new_folder"),
            MutationModalKind::Rename => text(language, "files_rename"),
            MutationModalKind::Delete => text(language, "files_delete_title"),
        };
        let mut open = true;
        let mut submitted = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                if matches!(kind, MutationModalKind::Delete) {
                    ui.label(text(language, "files_delete_body"));
                } else {
                    ui.add(egui::TextEdit::singleline(&mut modal.input).desired_width(260.0));
                }
                ui.horizontal(|ui| {
                    let confirm = if matches!(kind, MutationModalKind::Delete) {
                        ui.button(text(language, "files_delete")).clicked()
                    } else {
                        ui.button(text(language, "confirm")).clicked()
                    };
                    if confirm {
                        let request_id = OperationId::new();
                        let command = match (kind, modal.entry.clone()) {
                            (MutationModalKind::CreateDirectory, _) => state
                                .directory
                                .clone()
                                .and_then(|directory| directory.join_component(&modal.input).ok())
                                .map(|path| BackendCommand::CreateDirectory {
                                    request_id,
                                    target: target.clone(),
                                    path,
                                }),
                            (MutationModalKind::Rename, Some(entry)) => state
                                .directory
                                .clone()
                                .and_then(|directory| directory.join_component(&modal.input).ok())
                                .map(|destination| BackendCommand::RenameRemoteEntry {
                                    request_id,
                                    target: target.clone(),
                                    source: entry.path,
                                    destination,
                                }),
                            (MutationModalKind::Delete, Some(entry)) => {
                                Some(BackendCommand::DeleteRemoteFile {
                                    request_id,
                                    target: target.clone(),
                                    path: entry.path,
                                    confirmed: true,
                                })
                            }
                            _ => None,
                        };
                        if let Some(command) = command {
                            commands.push(state.start_mutation(command));
                            submitted = true;
                        } else {
                            state.error = Some(text(language, "files_invalid_name").to_owned());
                        }
                    }
                });
                if ui.button(text(language, "cancel")).clicked() {
                    submitted = true;
                }
            });
        if open && !submitted {
            state.mutation_modal = Some(modal);
        }
    }
    commands
}

/// Row context menu, mirroring the toolbar actions so files can be managed
/// in place like a desktop file manager.
fn entry_context_menu(
    ui: &mut egui::Ui,
    language: Language,
    state: &mut FilesPanelState,
    commands: &mut Vec<BackendCommand>,
    target: &DeviceTarget,
    entry: &RemoteFileEntry,
) {
    if is_directory_like(entry) && ui.button(text(language, "files_enter")).clicked() {
        commands.push(state.navigate(target.clone(), entry.path.clone()));
        ui.close();
    }
    if matches!(entry.kind, RemoteFileKind::File | RemoteFileKind::Symlink)
        && ui
            .add_enabled(
                state.transfer.is_none(),
                egui::Button::new(text(language, "files_download")),
            )
            .clicked()
        && let Some(local_path) = rfd::FileDialog::new()
            .set_title(text(language, "files_download_dialog"))
            .set_file_name(&entry.name)
            .save_file()
    {
        commands.push(state.start_transfer(
            TransferIntent {
                direction: FileTransferDirection::Download,
                target: target.clone(),
                local_path,
                remote_path: entry.path.clone(),
            },
            OverwritePolicy::Deny,
        ));
        ui.close();
    }
    if ui.button(text(language, "files_copy_path")).clicked() {
        ui.ctx().copy_text(entry.path.to_string());
        ui.close();
    }
    if state.mutation.is_none() {
        if ui.button(text(language, "files_rename")).clicked() {
            state.mutation_modal = Some(MutationModal {
                kind: MutationModalKind::Rename,
                input: entry.name.clone(),
                entry: Some(entry.clone()),
            });
            ui.close();
        }
        if ui.button(text(language, "files_delete")).clicked() {
            state.mutation_modal = Some(MutationModal {
                kind: MutationModalKind::Delete,
                input: String::new(),
                entry: Some(entry.clone()),
            });
            ui.close();
        }
    }
}

fn kind_label(language: Language, kind: RemoteFileKind) -> &'static str {
    match kind {
        RemoteFileKind::Directory => text(language, "files_kind_directory"),
        RemoteFileKind::File => text(language, "files_kind_file"),
        RemoteFileKind::Symlink => text(language, "files_kind_symlink"),
        RemoteFileKind::Other => text(language, "files_kind_other"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_change_clears_listing() {
        let mut state = FilesPanelState::default();
        let serial = fadb_domain::DeviceSerial::new("a").expect("valid");
        let target = DeviceTarget::new(serial, 1);
        let _ = state.reconcile_target(Some(target));
        assert!(state.loading);
    }

    #[test]
    fn default_sort_is_modified_newest_first() {
        let state = FilesPanelState::default();
        assert_eq!(state.sort_key, SortKey::Modified);
        assert!(state.sort_reverse);
    }

    #[test]
    fn navigation_records_history_for_back() {
        let mut state = FilesPanelState::default();
        let serial = fadb_domain::DeviceSerial::new("a").expect("valid");
        let target = DeviceTarget::new(serial, 1);
        let _ = state.reconcile_target(Some(target.clone()));
        state.directory = Some(RemotePath::new("/sdcard").expect("valid"));
        let _ = state.navigate(
            target.clone(),
            RemotePath::new("/sdcard/DCIM").expect("valid"),
        );
        assert_eq!(state.history.len(), 1);
        // Refreshing the same directory must not pollute history.
        let _ = state.list(target, RemotePath::new("/sdcard/DCIM").expect("valid"));
        assert_eq!(state.history.len(), 1);
        let back = state.history.pop().expect("history entry");
        assert_eq!(back.to_string(), "/sdcard");
    }

    fn entry(
        name: &str,
        kind: RemoteFileKind,
        size: Option<u64>,
        modified: Option<i64>,
    ) -> RemoteFileEntry {
        RemoteFileEntry {
            path: RemotePath::new(format!("/sdcard/{name}")).expect("valid"),
            name: name.to_owned(),
            kind,
            target_kind: None,
            size_bytes: size,
            modified_unix_seconds: modified,
            permissions: None,
        }
    }

    fn symlink_entry(name: &str, target_kind: Option<RemoteFileKind>) -> RemoteFileEntry {
        RemoteFileEntry {
            path: RemotePath::new(format!("/sdcard/{name}")).expect("valid"),
            name: name.to_owned(),
            kind: RemoteFileKind::Symlink,
            target_kind,
            size_bytes: None,
            modified_unix_seconds: None,
            permissions: None,
        }
    }

    #[test]
    fn sorts_entries_mixed_files_and_directories() {
        let mut entries = vec![
            entry("b.txt", RemoteFileKind::File, Some(2), Some(200)),
            symlink_entry("zlink", Some(RemoteFileKind::Directory)),
            entry("dir", RemoteFileKind::Directory, None, None),
            entry("a.txt", RemoteFileKind::File, Some(1), Some(100)),
        ];
        sort_entries(&mut entries, SortKey::Name, false);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // Folders (including symlinks to folders) group before files.
        assert_eq!(names, vec!["dir", "zlink", "a.txt", "b.txt"]);
        sort_entries(&mut entries, SortKey::Modified, false);
        // None (unknown time) sorts before Some(..), still inside the
        // folder group.
        assert!(is_directory_like(&entries[0]));
        assert!(is_directory_like(&entries[1]));
        assert_eq!(entries[2].name, "a.txt");
        assert_eq!(entries[3].name, "b.txt");
        sort_entries(&mut entries, SortKey::Modified, true);
        // Reversing keeps the folder group on top.
        assert!(is_directory_like(&entries[0]));
        assert!(is_directory_like(&entries[1]));
        assert_eq!(entries[2].name, "b.txt");
        assert_eq!(entries[3].name, "a.txt");
    }

    #[test]
    fn navigating_clears_previous_directory_rows() {
        let mut state = FilesPanelState::default();
        let serial = fadb_domain::DeviceSerial::new("a").expect("valid");
        let target = DeviceTarget::new(serial, 1);
        let _ = state.reconcile_target(Some(target.clone()));
        state.directory = Some(RemotePath::new("/sdcard").expect("valid"));
        state
            .entries
            .push(entry("stale.txt", RemoteFileKind::File, Some(1), Some(1)));
        let _ = state.navigate(target, RemotePath::new("/data").expect("valid"));
        assert!(state.entries.is_empty());
        assert_eq!(state.selected, None);
    }

    #[test]
    fn sdcard_failure_falls_back_to_root_once() {
        let mut state = FilesPanelState::default();
        let serial = fadb_domain::DeviceSerial::new("a").expect("valid");
        let target = DeviceTarget::new(serial, 1);
        let command = state
            .reconcile_target(Some(target.clone()))
            .expect("initial listing");
        let BackendCommand::ListDirectory {
            request_id, path, ..
        } = command
        else {
            panic!("expected ListDirectory");
        };
        assert_eq!(path.as_str(), "/sdcard");
        let fail = |request_id, path| BackendEvent::DirectoryFailed {
            request_id,
            target: target.clone(),
            path,
            error: fadb_domain::BridgeError::new(
                fadb_domain::ErrorCode::PathNotFound,
                "file.path_not_found",
                "missing",
            ),
        };
        // The bootstrap /sdcard listing failing retries at the root.
        let commands = state.handle_event(
            Language::English,
            &fail(request_id, RemotePath::new("/sdcard").expect("valid")),
        );
        let Some(BackendCommand::ListDirectory {
            request_id, path, ..
        }) = commands.first()
        else {
            panic!("expected root fallback, got {commands:?}");
        };
        assert_eq!(path.as_str(), "/");
        // A failing root listing must not loop the fallback.
        let commands = state.handle_event(Language::English, &fail(*request_id, path.clone()));
        assert!(commands.is_empty());
    }

    #[test]
    fn formats_known_timestamp() {
        assert_eq!(format_modified_time(1_700_000_000), "2023-11-14 22:13:20");
    }

    #[test]
    fn formats_size_in_fixed_units() {
        let size = 5 * 1_048_576 + 300 * 1_024;
        assert_eq!(format_size(512, FileSizeUnit::Bytes), "512 B");
        assert_eq!(format_size(512, FileSizeUnit::Kilobytes), "0.5 KB");
        assert_eq!(format_size(size, FileSizeUnit::Megabytes), "5.3 MB");
        assert_eq!(format_size(0, FileSizeUnit::Gigabytes), "0.0 GB");
    }

    #[test]
    fn unit_symbols_round_trip() {
        assert_eq!(FileSizeUnit::default(), FileSizeUnit::Megabytes);
        for unit in FileSizeUnit::ALL {
            assert_eq!(FileSizeUnit::from_symbol(unit.symbol()), Some(unit));
        }
        assert_eq!(FileSizeUnit::from_symbol("TiB"), None);
    }
}
