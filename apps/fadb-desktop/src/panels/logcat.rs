use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use fadb_domain::{BackendCommand, BackendEvent, DeviceTarget, LogcatSessionId};

use crate::i18n::{Language, text};

const MAX_LINES: usize = 10_000;
const AUTO_START_RETRY: Duration = Duration::from_secs(3);
const ROW_HEIGHT: f32 = 17.0;

/// Severity letters as emitted by `logcat -v threadtime`, in ascending order.
const LEVELS: [char; 6] = ['V', 'D', 'I', 'W', 'E', 'F'];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogLine {
    pub time: String,
    pub pid: String,
    pub tid: String,
    pub level: char,
    pub tag: String,
    pub message: String,
}

impl LogLine {
    /// Parses one `threadtime` line: `MM-DD HH:MM:SS.mmm PID TID L TAG: msg`.
    /// Runs of whitespace are collapsed, and everything past the severity
    /// letter is kept verbatim. Unrecognized lines survive as gray level-'?'
    /// rows so device noise is never silently dropped.
    fn parse(raw: &str) -> Self {
        if let Some((fields, rest)) = split_prefix_fields(raw, 5)
            && fields[4].len() == 1
        {
            let level = fields[4].chars().next().unwrap_or('?');
            if LEVELS.contains(&level) {
                let (tag, message) = match rest.split_once(": ") {
                    Some((tag, message)) => (tag, message),
                    None => (rest, ""),
                };
                return Self {
                    time: format!("{} {}", fields[0], fields[1]),
                    pid: fields[2].to_owned(),
                    tid: fields[3].to_owned(),
                    level,
                    tag: tag.to_owned(),
                    message: message.to_owned(),
                };
            }
        }
        Self {
            message: raw.to_owned(),
            level: '?',
            ..Self::default()
        }
    }

    fn severity_index(&self) -> Option<usize> {
        LEVELS.iter().position(|candidate| candidate == &self.level)
    }

    fn format(&self) -> String {
        if self.level == '?' {
            self.message.clone()
        } else {
            format!(
                "{} {}/{} {}: {}",
                self.time, self.level, self.tag, self.pid, self.message
            )
        }
    }
}

/// `filter` is the dropdown index: 0 = all, otherwise the minimum severity.
/// The dropdown carries an extra "all" entry ahead of `LEVELS`, hence the
/// shift against `severity_index`. Unrecognized ('?') rows only survive the
/// unfiltered view.
fn level_passes(filter: usize, severity_index: Option<usize>) -> bool {
    match (filter, severity_index) {
        (0, _) => true,
        (_, None) => false,
        (_, Some(index)) => index + 1 >= filter,
    }
}

/// Splits the first `count` whitespace-separated fields off `raw`,
/// collapsing runs of spaces; returns the fields and the untrimmed remainder.
fn split_prefix_fields(raw: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let mut fields = Vec::with_capacity(count);
    let mut rest = raw;
    for _ in 0..count {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    Some((fields, rest.trim_start()))
}

fn level_color(level: char) -> Color32 {
    match level {
        'V' => Color32::from_gray(140),
        'D' => Color32::from_rgb(96, 165, 250),
        'I' => Color32::from_rgb(134, 239, 172),
        'W' => Color32::from_rgb(250, 204, 21),
        'E' => Color32::from_rgb(248, 113, 113),
        'F' => Color32::from_rgb(244, 63, 94),
        _ => Color32::from_gray(110),
    }
}

// The plain bools are the small session state machine (starting/running/
// user-stopped/paused) plus the auto-scroll preference.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
pub struct LogcatPanelState {
    pub target: Option<DeviceTarget>,
    session: Option<LogcatSessionId>,
    /// A `StartLogcat` is in flight for this session id.
    starting: bool,
    /// The stream is live (started and not yet closed/failed).
    running: bool,
    /// The user pressed stop; suppress auto-start until the target changes.
    user_stopped: bool,
    paused: bool,
    auto_scroll: bool,
    /// Last time stick-to-bottom fired; auto-scroll is throttled to a few
    /// updates per second so high-volume streams stay readable.
    last_stick: Option<Instant>,
    /// Minimum severity to display (index into `LEVELS`).
    level_filter: usize,
    query: String,
    lines: Vec<LogLine>,
    pending_bytes: Vec<u8>,
    last_start_attempt: Option<Instant>,
    /// Row indices passing the current filters, rebuilt each frame.
    visible: Vec<usize>,
    /// Selected buffer-line range (anchor, head) — indices into `lines`.
    selection: Option<(usize, usize)>,
    /// View index and row top where the active drag began; drag events are
    /// only delivered to that row, which maps pointer deltas back onto rows.
    drag_origin: Option<(usize, f32)>,
}

impl LogcatPanelState {
    pub fn reset_for(&mut self, target: Option<DeviceTarget>) -> Vec<BackendCommand> {
        let mut commands = Vec::new();
        if self.target != target {
            self.target = target;
            commands.extend(self.take_down());
            self.user_stopped = false;
        }
        commands
    }

    /// Stops any live stream and clears the buffer.
    fn take_down(&mut self) -> Vec<BackendCommand> {
        let mut commands = Vec::new();
        if let Some(session) = self.session.take() {
            commands.push(BackendCommand::StopLogcat(session));
        }
        self.starting = false;
        self.running = false;
        self.lines.clear();
        self.pending_bytes.clear();
        self.selection = None;
        self.drag_origin = None;
        commands
    }

    pub fn handle_event(&mut self, event: &BackendEvent) {
        match event {
            BackendEvent::LogcatStarted { target, session_id }
                if self.target.as_ref() == Some(target)
                    && self.session.as_ref() == Some(session_id) =>
            {
                self.starting = false;
                self.running = true;
                self.user_stopped = false;
            }
            BackendEvent::LogcatOutput { session_id, bytes }
                if self.session.as_ref() == Some(session_id) && !bytes.is_empty() =>
            {
                self.ingest(bytes);
            }
            BackendEvent::LogcatClosed { session_id }
            | BackendEvent::LogcatFailed { session_id, .. }
                if self.session.as_ref() == Some(session_id) =>
            {
                self.session = None;
                self.starting = false;
                self.running = false;
            }
            _ => {}
        }
    }

    /// Splits streamed bytes into lines and appends them while not paused.
    /// A paused stream keeps consuming (dropping) data, matching the usual
    /// "resume from now" expectation.
    fn ingest(&mut self, bytes: &[u8]) {
        let mut start = 0;
        for (index, byte) in bytes.iter().copied().enumerate() {
            if byte == b'\n' {
                self.pending_bytes.extend_from_slice(&bytes[start..index]);
                start = index + 1;
                let line = String::from_utf8_lossy(&self.pending_bytes);
                if !self.paused {
                    self.append(LogLine::parse(line.trim_end_matches('\r')));
                }
                self.pending_bytes.clear();
            }
        }
        self.pending_bytes.extend_from_slice(&bytes[start..]);
        if self.lines.len() > MAX_LINES {
            let remove = self.lines.len() - MAX_LINES;
            self.lines.drain(0..remove);
            self.shift_selection_after_trim(remove);
        }
    }

    fn append(&mut self, line: LogLine) {
        self.lines.push(line);
        if self.lines.len() > MAX_LINES {
            self.lines.remove(0);
            self.shift_selection_after_trim(1);
        }
    }

    /// Selection anchors are buffer indices; when rows fall off the front of
    /// the ring, a range that touched them is dropped rather than re-anchored.
    fn shift_selection_after_trim(&mut self, removed: usize) {
        let Some((anchor, head)) = self.selection.as_mut() else {
            return;
        };
        if *anchor < removed || *head < removed {
            self.selection = None;
            return;
        }
        *anchor -= removed;
        *head -= removed;
    }
}

/// Renders the logcat console; returns any backend commands the user issued.
#[allow(clippy::too_many_lines)]
pub fn show(
    ui: &mut egui::Ui,
    language: Language,
    state: &mut LogcatPanelState,
) -> Vec<BackendCommand> {
    let mut commands = Vec::new();
    ui.horizontal(|ui| {
        ui.heading(text(language, "logcat"));
        ui.add_space(6.0);
        if state.running {
            ui.label(RichText::new("●").color(Color32::from_rgb(74, 222, 128)));
            ui.label(text(language, "logcat_streaming"));
        } else if state.starting {
            ui.spinner();
            ui.label(text(language, "logcat_starting"));
        } else {
            ui.label(text(language, "logcat_idle"));
        }
        if state.paused {
            ui.label(RichText::new(text(language, "logcat_paused")).weak());
        }
    });
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        let online = state.target.is_some();
        if !state.running
            && !state.starting
            && ui
                .add_enabled(online, egui::Button::new(text(language, "logcat_start")))
                .clicked()
            && let Some(target) = state.target.clone()
        {
            let session_id = LogcatSessionId::new();
            state.session = Some(session_id);
            state.starting = true;
            state.paused = false;
            state.user_stopped = false;
            state.last_start_attempt = Some(Instant::now());
            commands.push(BackendCommand::StartLogcat { target, session_id });
        }
        if state.running
            && ui
                .add_enabled(online, egui::Button::new(text(language, "logcat_stop")))
                .clicked()
        {
            commands.extend(state.take_down());
            state.user_stopped = true;
        }
        if ui
            .add_enabled(
                state.running,
                egui::Button::new(if state.paused {
                    text(language, "logcat_resume")
                } else {
                    text(language, "logcat_pause")
                }),
            )
            .clicked()
        {
            state.paused = !state.paused;
        }
        if ui.button(text(language, "logcat_clear")).clicked() {
            state.lines.clear();
            state.selection = None;
            state.drag_origin = None;
        }
        if ui
            .add_enabled(
                !state.lines.is_empty(),
                egui::Button::new(text(language, "logcat_copy_visible")),
            )
            .clicked()
        {
            let query = state.query.trim().to_lowercase();
            let mut body = String::new();
            for line in &state.lines {
                if line_passes_filters(line, state.level_filter, &query) {
                    body.push_str(&line.format());
                    body.push('\n');
                }
            }
            ui.ctx().copy_text(body);
        }
        if ui.button(text(language, "logcat_save")).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("fadb-logcat.txt")
                .add_filter("Text", &["txt", "log"])
                .save_file()
        {
            let mut body = String::new();
            for line in &state.lines {
                body.push_str(&line.format());
                body.push('\n');
            }
            if let Err(error) = std::fs::write(&path, body) {
                tracing::warn!(%error, "logcat export failed");
            }
        }
        ui.separator();
        ui.checkbox(&mut state.auto_scroll, text(language, "logcat_autoscroll"));
        ui.separator();
        ui.label(text(language, "logcat_level"));
        let level_names = [
            text(language, "logcat_level_all"),
            "V",
            "D",
            "I",
            "W",
            "E",
            "F",
        ];
        egui::ComboBox::from_id_salt("logcat-level-filter")
            .selected_text(level_names[state.level_filter])
            .width(64.0)
            .show_ui(ui, |ui| {
                for (index, name) in level_names.iter().enumerate() {
                    ui.selectable_value(&mut state.level_filter, index, *name);
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut state.query)
                .desired_width(180.0)
                .hint_text(text(language, "logcat_search_hint")),
        );
    });
    ui.add_space(6.0);

    if !online_and_selected(state) {
        ui.label(text(language, "files_select_device"));
        return commands;
    }

    // Rebuild the visible-row set for this frame's filters.
    let query = state.query.trim().to_lowercase();
    state.visible.clear();
    for (index, line) in state.lines.iter().enumerate() {
        if line_passes_filters(line, state.level_filter, &query) {
            state.visible.push(index);
        }
    }

    let total = state.lines.len();
    ui.label(
        RichText::new(text_with_count(
            language,
            "logcat_line_count",
            state.visible.len(),
            total,
        ))
        .weak(),
    );

    let visible = std::mem::take(&mut state.visible);
    // Stick to the bottom a few times per second at most: on every frame the
    // view would jump dozens of rows per second, which is unreadable.
    let stick_now = state.auto_scroll
        && !state.paused
        && state
            .last_stick
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(250));
    if stick_now {
        state.last_stick = Some(Instant::now());
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .stick_to_bottom(stick_now)
        .show_rows(ui, ROW_HEIGHT, visible.len(), |ui, range| {
            // `Range` is not `Copy`; iterate a clone so the original window
            // can still be handed to the pointer handler below.
            for row in range.clone() {
                let line_index = visible[row];
                let color = level_color(state.lines[line_index].level);
                // One interactive surface per row: the selection tint is
                // painted under a manually laid out label because `Label`
                // cannot carry the drag sense row selection needs.
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ROW_HEIGHT),
                    egui::Sense::click_and_drag(),
                );
                let painter = ui.painter_at(rect);
                let selected = selection_span(state.selection)
                    .is_some_and(|(start, end)| start <= line_index && line_index <= end);
                if selected {
                    painter.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                }
                painter.text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    state.lines[line_index].format(),
                    egui::FontId::monospace(11.5),
                    color,
                );
                handle_row_drag(ui, state, &response, rect, row, &visible, range.clone());
                handle_row_menu(language, state, &response, line_index, &visible);
            }
        });
    state.visible = visible;
    commands
}

/// Drag-select with release-to-copy over the visible window, mirroring the
/// shell terminal. A plain click collapses the selection again.
// The row math is safe: indices come from the on-screen window (a few dozen
// rows at most), and the pointer delta is floored and clamped to that window.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn handle_row_drag(
    ui: &egui::Ui,
    state: &mut LogcatPanelState,
    response: &egui::Response,
    rect: egui::Rect,
    row: usize,
    visible: &[usize],
    range: std::ops::Range<usize>,
) {
    let line_index = visible[row];
    if response.drag_started() {
        state.selection = Some((line_index, line_index));
        state.drag_origin = Some((row, rect.top()));
    }
    // Drag events are delivered to the row where the drag began; map the
    // pointer's vertical travel back onto row indices, clamped to the window
    // on screen (the terminal has the same viewport-bound selection).
    if response.dragged()
        && let Some((origin_row, origin_top)) = state.drag_origin
        && let Some(pos) = response.interact_pointer_pos()
        && !range.is_empty()
    {
        let delta = ((pos.y - origin_top) / ROW_HEIGHT).round() as isize;
        let head_row = (origin_row as isize + delta)
            .clamp(range.start as isize, range.end.saturating_sub(1) as isize)
            as usize;
        let head = visible[head_row];
        match state.selection.as_mut() {
            Some((_, slot)) => *slot = head,
            None => state.selection = Some((head, head)),
        }
    }
    if response.drag_stopped() {
        state.drag_origin = None;
        // Release-to-copy, but only for a real drag across rows.
        if let Some((start, end)) = selection_span(state.selection)
            && start != end
            && state.lines[start..=end]
                .iter()
                .any(|line| !line.message.trim().is_empty())
            && let Some(text) = selection_text(state)
        {
            ui.ctx().copy_text(text);
        }
    }
    if response.clicked() {
        state.selection = None;
        state.drag_origin = None;
    }
}

/// The right-click clipboard menu: copy the selection (right-clicking a row
/// outside the current selection re-anchors it there, matching list
/// conventions) or everything passing the current filters.
fn handle_row_menu(
    language: Language,
    state: &mut LogcatPanelState,
    response: &egui::Response,
    line_index: usize,
    visible: &[usize],
) {
    response.context_menu(|ui| {
        let covers_row = selection_span(state.selection)
            .is_some_and(|(start, end)| start <= line_index && line_index <= end);
        if !covers_row {
            state.selection = Some((line_index, line_index));
        }
        if ui
            .add_enabled(
                state.selection.is_some(),
                egui::Button::new(text(language, "logcat_copy_selection")),
            )
            .clicked()
        {
            if let Some(text) = selection_text(state) {
                ui.ctx().copy_text(text);
            }
            ui.close();
        }
        if ui.button(text(language, "logcat_copy_visible")).clicked() {
            ui.ctx().copy_text(visible_text(state, visible));
            ui.close();
        }
    });
}

/// Normalizes the (anchor, head) pair into an ordered inclusive span.
fn selection_span(selection: Option<(usize, usize)>) -> Option<(usize, usize)> {
    selection.map(|(anchor, head)| {
        if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        }
    })
}

/// The formatted lines covered by the selection, one per row.
fn selection_text(state: &LogcatPanelState) -> Option<String> {
    let (start, end) = selection_span(state.selection)?;
    let mut body = String::new();
    for line in &state.lines[start..=end] {
        body.push_str(&line.format());
        body.push('\n');
    }
    Some(body)
}

/// The formatted lines passing the current filters, in display order.
fn visible_text(state: &LogcatPanelState, visible: &[usize]) -> String {
    let mut body = String::new();
    for &index in visible {
        body.push_str(&state.lines[index].format());
        body.push('\n');
    }
    body
}

/// The shared per-line filter: minimum severity plus the search query.
fn line_passes_filters(line: &LogLine, level_filter: usize, query: &str) -> bool {
    if !level_passes(level_filter, line.severity_index()) {
        return false;
    }
    query.is_empty()
        || line.tag.to_lowercase().contains(query)
        || line.message.to_lowercase().contains(query)
}

fn online_and_selected(state: &LogcatPanelState) -> bool {
    state.target.is_some()
}

/// "显示 123 / 4560 行" without pulling a formatting crate in.
fn text_with_count(language: Language, key: &str, visible: usize, total: usize) -> String {
    let template = text(language, key);
    template
        .replace("{visible}", &visible.to_string())
        .replace("{total}", &total.to_string())
}

/// Called by the app shell each frame: auto-start the stream when the panel
/// is on screen, a device is online, and no session exists yet.
pub fn auto_start(state: &mut LogcatPanelState, target_online: bool) -> Vec<BackendCommand> {
    let mut commands = Vec::new();
    if !target_online
        || state.running
        || state.starting
        || state.user_stopped
        || state.session.is_some()
    {
        return commands;
    }
    let recently_attempted = state
        .last_start_attempt
        .is_some_and(|last| last.elapsed() < AUTO_START_RETRY);
    if recently_attempted {
        return commands;
    }
    let Some(target) = state.target.clone() else {
        return commands;
    };
    let session_id = LogcatSessionId::new();
    state.session = Some(session_id);
    state.starting = true;
    state.last_start_attempt = Some(Instant::now());
    commands.push(BackendCommand::StartLogcat { target, session_id });
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_threadtime_lines() {
        let line = LogLine::parse(
            "08-29 10:00:01.100  1000  1001 I ActivityTaskManager: Displayed com.example/.Main",
        );
        assert_eq!(line.time, "08-29 10:00:01.100");
        assert_eq!(line.pid, "1000");
        assert_eq!(line.tid, "1001");
        assert_eq!(line.level, 'I');
        assert_eq!(line.tag, "ActivityTaskManager");
        assert_eq!(line.message, "Displayed com.example/.Main");
    }

    #[test]
    fn keeps_unrecognized_lines_as_raw_rows() {
        let line = LogLine::parse("--------- beginning of main");
        assert_eq!(line.level, '?');
        assert_eq!(line.message, "--------- beginning of main");
    }

    #[test]
    fn severity_orders_for_filtering() {
        assert_eq!(
            LogLine::parse("08-29 10:00:00.000 1 1 V T: m").severity_index(),
            Some(0)
        );
        assert_eq!(
            LogLine::parse("08-29 10:00:00.000 1 1 D T: m").severity_index(),
            Some(1)
        );
        assert_eq!(
            LogLine::parse("08-29 10:00:00.000 1 1 W T: m").severity_index(),
            Some(3)
        );
        assert_eq!(
            LogLine::parse("08-29 10:00:00.000 1 1 F T: m").severity_index(),
            Some(5)
        );
        assert_eq!(LogLine::parse("junk").severity_index(), None);
    }

    #[test]
    fn ingest_splits_lines_and_respects_pause() {
        let mut state = LogcatPanelState::default();
        state.ingest(b"08-29 10:00:01.100 1 1 I Tag: hello\n08-29 10:00:01.200 1 1 W");
        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.pending_bytes, b"08-29 10:00:01.200 1 1 W");
        state.ingest(b" Tag: partial\n");
        assert_eq!(state.lines.len(), 2);
        assert_eq!(state.lines[1].tag, "Tag");

        state.paused = true;
        state.ingest(b"08-29 10:00:01.300 1 1 E Tag: dropped\n");
        assert_eq!(state.lines.len(), 2);
    }

    #[test]
    fn level_filter_minimum_severity_is_inclusive() {
        // Selecting W (dropdown index 4) keeps W, E and F, drops V/D/I.
        assert!(level_passes(4, Some(3)));
        assert!(level_passes(4, Some(4)));
        assert!(level_passes(4, Some(5)));
        assert!(!level_passes(4, Some(2)));
        // Selecting E keeps E and F.
        assert!(level_passes(5, Some(4)));
        assert!(!level_passes(5, Some(3)));
        // "All" keeps everything, including unparsed rows.
        assert!(level_passes(0, Some(0)));
        assert!(level_passes(0, None));
        // Unparsed rows only survive the unfiltered view.
        assert!(!level_passes(1, None));
    }

    #[test]
    fn selection_span_orders_anchor_and_head() {
        assert_eq!(selection_span(None), None);
        assert_eq!(selection_span(Some((4, 2))), Some((2, 4)));
        assert_eq!(selection_span(Some((3, 3))), Some((3, 3)));
    }

    #[test]
    fn selection_text_joins_formatted_rows() {
        let mut state = LogcatPanelState::default();
        state
            .lines
            .push(LogLine::parse("08-29 10:00:00.000 1 1 I A: one"));
        state.lines.push(LogLine::parse("junk"));
        state.selection = Some((1, 0));
        assert_eq!(
            selection_text(&state).as_deref(),
            Some("08-29 10:00:00.000 I/A 1: one\njunk\n")
        );
        state.selection = None;
        assert!(selection_text(&state).is_none());
    }

    #[test]
    fn trimming_the_ring_drops_selections_touching_the_front() {
        let mut state = LogcatPanelState {
            selection: Some((5, 7)),
            ..Default::default()
        };
        state.shift_selection_after_trim(2);
        assert_eq!(state.selection, Some((3, 5)));
        state.shift_selection_after_trim(5);
        assert_eq!(state.selection, None);
    }

    #[test]
    fn line_filter_combines_level_and_query() {
        let line = LogLine::parse("08-29 10:00:00.000 1 1 W Tag: boom");
        assert!(line_passes_filters(&line, 4, ""));
        assert!(!line_passes_filters(&line, 5, ""));
        assert!(line_passes_filters(&line, 4, "boo"));
        assert!(!line_passes_filters(&line, 4, "nope"));
        assert!(line_passes_filters(&LogLine::parse("junk"), 0, ""));
        assert!(!line_passes_filters(&LogLine::parse("junk"), 1, ""));
    }

    #[test]
    fn auto_start_requires_online_target_and_backs_off() {
        let mut state = LogcatPanelState::default();
        assert!(auto_start(&mut state, false).is_empty());

        let target = DeviceTarget::new(
            fadb_domain::DeviceSerial::new("emulator-5554").expect("serial"),
            1,
        );
        state.target = Some(target);
        let commands = auto_start(&mut state, true);
        assert_eq!(commands.len(), 1);
        // A session is already pending: no double start.
        assert!(auto_start(&mut state, true).is_empty());
        // The user pressed stop: auto-start stays suppressed.
        let mut stopped = LogcatPanelState {
            target: state.target.clone(),
            user_stopped: true,
            ..LogcatPanelState::default()
        };
        assert!(auto_start(&mut stopped, true).is_empty());
    }
}
