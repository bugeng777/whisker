use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_core::{
    ApprovalDecision, ApprovalRequest, CancelHandle, Event, GenerationSettings, TodoItem,
    TodoStatus,
};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::{mpsc, oneshot};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::brand::{self, Pose};
use crate::{describe_tool, truncate, Command, Interaction};

const ACCENT: Color = Color::Indexed(208);
const OK: Color = Color::Indexed(114);
const PLAN: Color = Color::Indexed(179);
const DIM: Color = Color::DarkGray;
const FOCUS: Color = Color::Indexed(153);
const MUTED: Color = Color::Indexed(246);
const RED: Color = Color::Rgb(255, 100, 100);
const YELLOW: Color = Color::Rgb(230, 190, 80);
const SETTINGS_COL_WIDTH: usize = 47;
const SETTINGS_TABS: &[&str] = &["Settings", "Status", "Usage"];
const DEEPSEEK_SETTINGS_TABS: &[&str] = &["Settings", "DeepSeek", "Status", "Usage"];
const DEEPSEEK_EFFORTS: &[&str] = &["low", "high", "max"];
const MAX_TOKEN_PRESETS: &[Option<u32>] =
    &[None, Some(8_192), Some(32_768), Some(65_536), Some(131_072)];
const TEMPERATURE_PRESETS: &[f32] = &[0.0, 0.2, 0.5, 1.0, 1.5, 2.0];

fn settings_tabs(deepseek: bool) -> &'static [&'static str] {
    if deepseek {
        DEEPSEEK_SETTINGS_TABS
    } else {
        SETTINGS_TABS
    }
}

fn deepseek_settings_path() -> PathBuf {
    std::env::var_os("WHISKER_DEEPSEEK_SETTINGS")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".whisker/deepseek-settings.json"))
        })
        .unwrap_or_else(|| PathBuf::from(".whisker/deepseek-settings.json"))
}

fn max_tokens_label(value: Option<u32>) -> String {
    match value {
        None => "auto".to_string(),
        Some(value) if value >= 1024 => format!("{}K", value / 1024),
        Some(value) => value.to_string(),
    }
}

fn cycle_max_tokens(value: Option<u32>) -> Option<u32> {
    let index = MAX_TOKEN_PRESETS
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0);
    MAX_TOKEN_PRESETS[(index + 1) % MAX_TOKEN_PRESETS.len()]
}

fn temperature_label(value: Option<f32>) -> String {
    value
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "auto".to_string())
}

fn cycle_temperature(value: Option<f32>) -> Option<f32> {
    let index = value
        .and_then(|value| {
            TEMPERATURE_PRESETS
                .iter()
                .position(|candidate| (*candidate - value).abs() < 0.001)
        })
        .unwrap_or(0);
    Some(TEMPERATURE_PRESETS[(index + 1) % TEMPERATURE_PRESETS.len()])
}

fn cycle_effort(value: &str) -> String {
    let index = DEEPSEEK_EFFORTS
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(value))
        .unwrap_or(1);
    DEEPSEEK_EFFORTS[(index + 1) % DEEPSEEK_EFFORTS.len()].to_string()
}

fn context_label(context: usize) -> String {
    if context >= 1_048_576 {
        format!("{}M", context / 1_048_576)
    } else {
        format!("{}K", context / 1000)
    }
}

struct Chunk {
    text: String,
    style: Style,
}

enum Pending {
    Approval {
        request: ApprovalRequest,
        reply: oneshot::Sender<ApprovalDecision>,
    },
    Ask {
        question: String,
        options: Vec<String>,
        reply: oneshot::Sender<Option<String>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsFocus {
    Search,
    Tabs,
    List,
}

struct SettingsPage {
    tab: usize,
    focus: SettingsFocus,
    query: String,
    selected: Option<usize>,
    command: String,
}

#[derive(Clone, Copy)]
enum SettingAction {
    CycleMode,
    ToggleTips,
    ToggleMotion,
    OpenModelPicker,
    ToggleThinking,
    CycleEffort,
    CycleMaxTokens,
    CycleTemperature,
}

struct SettingRow {
    label: String,
    value: String,
    action: Option<SettingAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerKind {
    Model,
    Mode,
}

struct PickerOption {
    value: String,
    label: String,
    description: String,
    current: bool,
}

struct PickerState {
    kind: PickerKind,
    title: String,
    subtitle: String,
    options: Vec<PickerOption>,
    selected: usize,
}

struct TuiState {
    chunks: Vec<Chunk>,
    input: String,
    ask_input: String,
    busy: bool,
    spinner: usize,
    mode: String,
    model: String,
    models: Vec<String>,
    generation: GenerationSettings,
    context_window: usize,
    deepseek_mode: bool,
    deepseek_settings_path: PathBuf,
    picker: Option<PickerState>,
    todos: Vec<TodoItem>,
    total_in: u32,
    total_out: u32,
    follow: bool,
    scroll: u16,
    pending: Option<Pending>,
    mascot_frame: usize,
    scanning: bool,
    success_until: Instant,
    pose_override: Option<(Pose, Instant)>,
    turn_text: bool,
    answer_open: bool,
    plan_pending: bool,
    plan_selection: usize,
    approval_selection: usize,
    toast: Option<(String, Instant)>,
    settings: Option<SettingsPage>,
    show_tips: bool,
    reduce_motion: bool,
    turns: u32,
}

impl TuiState {
    fn new(mode: String) -> Self {
        let state = Self {
            chunks: Vec::new(),
            input: String::new(),
            ask_input: String::new(),
            busy: false,
            spinner: 0,
            mode,
            model: String::new(),
            models: Vec::new(),
            generation: GenerationSettings::default(),
            context_window: 0,
            deepseek_mode: false,
            deepseek_settings_path: deepseek_settings_path(),
            picker: None,
            todos: Vec::new(),
            total_in: 0,
            total_out: 0,
            follow: true,
            scroll: 0,
            pending: None,
            mascot_frame: 0,
            scanning: false,
            success_until: Instant::now(),
            pose_override: None,
            turn_text: false,
            answer_open: false,
            plan_pending: false,
            plan_selection: 0,
            approval_selection: 0,
            toast: None,
            settings: None,
            show_tips: true,
            reduce_motion: false,
            turns: 0,
        };
        state
    }

    fn current_pose(&self) -> Pose {
        if let Some((pose, until)) = self.pose_override {
            if Instant::now() < until {
                return pose;
            }
        }
        if self.busy {
            if self.scanning {
                Pose::Scanning
            } else {
                Pose::Working
            }
        } else if Instant::now() < self.success_until {
            Pose::Success
        } else {
            Pose::Idle
        }
    }

    fn push(&mut self, text: &str, style: Style) {
        self.chunks.push(Chunk {
            text: text.to_string(),
            style,
        });
    }

    fn push_line(&mut self, text: &str, style: Style) {
        self.push(text, style);
        self.push("\n", Style::default());
    }

    fn ensure_line_start(&mut self) {
        let ends_with_newline = self
            .chunks
            .last()
            .map(|chunk| chunk.text.ends_with('\n'))
            .unwrap_or(true);
        if !ends_with_newline {
            self.push("\n", Style::default());
        }
    }

    fn ensure_blank_line(&mut self) {
        self.ensure_line_start();
        let mut newlines = 0;
        for chunk in self.chunks.iter().rev() {
            for ch in chunk.text.chars().rev() {
                match ch {
                    '\n' => newlines += 1,
                    _ => {
                        if newlines < 2 {
                            self.push("\n", Style::default());
                        }
                        return;
                    }
                }
            }
        }
        if newlines < 2 {
            self.push("\n", Style::default());
        }
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.answer_open = false;
        self.scroll = 0;
        self.follow = true;
    }
}

fn dim() -> Style {
    Style::default().fg(DIM)
}

const INPUT_MAX_VIEW_ROWS: usize = 8;

fn input_display(state: &TuiState) -> &str {
    if matches!(state.pending, Some(Pending::Ask { .. })) {
        &state.ask_input
    } else {
        &state.input
    }
}

fn input_text_width(total_width: u16) -> usize {
    (total_width as usize).saturating_sub(3).max(1)
}

fn wrap_input(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for logical in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        let mut last_space: Option<usize> = None;
        for ch in logical.chars() {
            let char_width = ch.width().unwrap_or(0);
            while used + char_width > width && !line.is_empty() {
                if let Some(pos) = last_space {
                    let tail = line[pos + 1..].to_string();
                    line.truncate(pos);
                    if !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                    } else {
                        line.clear();
                    }
                    line = tail;
                    used = UnicodeWidthStr::width(line.as_str());
                    last_space = line.rfind(' ');
                    if last_space.is_none() && used + char_width > width && !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                } else {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
            }
            if ch == ' ' {
                last_space = Some(line.len());
            }
            line.push(ch);
            used += char_width;
        }
        lines.push(line);
    }
    lines
}

fn input_view_rows(state: &TuiState, total_width: u16, total_height: u16) -> usize {
    let cap = INPUT_MAX_VIEW_ROWS.min((total_height as usize).saturating_sub(14).max(1));
    wrap_input(input_display(state), input_text_width(total_width))
        .len()
        .clamp(1, cap)
}

fn styled_rows(chunks: &[Chunk]) -> Vec<Vec<(char, Style)>> {
    let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    for chunk in chunks {
        let mut first = true;
        for part in chunk.text.split('\n') {
            if !first {
                rows.push(Vec::new());
            }
            if let Some(row) = rows.last_mut() {
                for ch in part.chars() {
                    row.push((ch, chunk.style));
                }
            }
            first = false;
        }
    }
    rows
}

fn push_wrapped(
    out: &mut Vec<Line<'static>>,
    body: &[(char, Style)],
    marker: Option<char>,
    marker_style: Style,
    width: usize,
) {
    let width = width.max(1);
    let mut line_start = 0;
    let mut first = true;
    loop {
        if !first {
            while line_start < body.len() && body[line_start].0 == ' ' {
                line_start += 1;
            }
        }
        let mut end = line_start;
        let mut used = 0;
        let mut last_space = None;
        while end < body.len() {
            let (ch, _) = body[end];
            let char_width = ch.width().unwrap_or(0);
            if used + char_width > width && end > line_start {
                break;
            }
            if ch == ' ' {
                last_space = Some(end);
            }
            used += char_width;
            end += 1;
        }
        let line_end = match last_space {
            Some(space) if end < body.len() && space > line_start => space,
            _ => end,
        };
        let next = if line_end < end && line_end > line_start {
            line_end + 1
        } else if end < body.len() {
            end
        } else {
            body.len()
        };

        let mut spans: Vec<Span<'static>> = Vec::new();
        if first {
            if let Some(marker) = marker {
                spans.push(Span::styled(format!("{marker} "), marker_style));
            } else {
                spans.push(Span::raw("  "));
            }
        } else {
            spans.push(Span::raw("  "));
        }
        let mut run = String::new();
        let mut run_style: Option<Style> = None;
        for (ch, style) in &body[line_start..line_end] {
            if run_style == Some(*style) {
                run.push(*ch);
            } else {
                if !run.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        run_style.unwrap_or_default(),
                    ));
                }
                run.push(*ch);
                run_style = Some(*style);
            }
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style.unwrap_or_default()));
        }
        out.push(Line::from(spans));

        first = false;
        line_start = next;
        if line_start >= body.len() {
            break;
        }
    }
}

fn bool_value(value: bool) -> String {
    if value {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

fn setting_rows(state: &TuiState, tab: usize, model: &str, session: &str) -> Vec<SettingRow> {
    let row = |label: &str, value: String, action: Option<SettingAction>| SettingRow {
        label: label.to_string(),
        value,
        action,
    };
    let effective_model = if state.model.is_empty() {
        model.to_string()
    } else {
        state.model.clone()
    };
    let deepseek_tab = state.deepseek_mode;
    let status_tab = if deepseek_tab { 2 } else { 1 };
    let usage_tab = if deepseek_tab { 3 } else { 2 };
    let thinking = match state.generation.thinking {
        Some(true) => "on",
        Some(false) => "off",
        None => "auto",
    };
    let effort = state
        .generation
        .reasoning_effort
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    match tab {
        0 => vec![
            row(
                "Permission mode",
                state.mode.clone(),
                Some(SettingAction::CycleMode),
            ),
            row(
                "Model",
                effective_model,
                Some(SettingAction::OpenModelPicker),
            ),
            row(
                "Show tips",
                bool_value(state.show_tips),
                Some(SettingAction::ToggleTips),
            ),
            row(
                "Reduce motion",
                bool_value(state.reduce_motion),
                Some(SettingAction::ToggleMotion),
            ),
            row("Session", session.to_string(), None),
        ],
        1 if deepseek_tab => vec![
            row(
                "Model",
                effective_model,
                Some(SettingAction::OpenModelPicker),
            ),
            row(
                "Thinking",
                thinking.to_string(),
                Some(SettingAction::ToggleThinking),
            ),
            row(
                "Reasoning effort",
                effort.clone(),
                Some(SettingAction::CycleEffort),
            ),
            row(
                "Max output tokens",
                max_tokens_label(state.generation.max_tokens),
                Some(SettingAction::CycleMaxTokens),
            ),
            row(
                "Temperature",
                temperature_label(state.generation.temperature),
                Some(SettingAction::CycleTemperature),
            ),
            row("Context window", context_label(state.context_window), None),
        ],
        t if t == status_tab => {
            let status = if state.busy {
                if state.scanning {
                    "scanning"
                } else {
                    "working"
                }
            } else if Instant::now() < state.success_until {
                "purring"
            } else {
                "ready"
            };
            vec![
                row("Session", session.to_string(), None),
                row("Model", effective_model, None),
                row("Mode", state.mode.clone(), None),
                row("State", status.to_string(), None),
                row("Thinking", thinking.to_string(), None),
                row("Reasoning effort", effort, None),
                row(
                    "Tokens",
                    format!("{} / {}", state.total_in, state.total_out),
                    None,
                ),
                row("Turns", state.turns.to_string(), None),
            ]
        }
        t if t == usage_tab => vec![
            row("Input tokens", state.total_in.to_string(), None),
            row("Output tokens", state.total_out.to_string(), None),
            row(
                "Total tokens",
                state.total_in.saturating_add(state.total_out).to_string(),
                None,
            ),
            row("Turns", state.turns.to_string(), None),
        ],
        _ => Vec::new(),
    }
}

fn filtered_setting_rows(
    page: &SettingsPage,
    state: &TuiState,
    model: &str,
    session: &str,
) -> Vec<SettingRow> {
    let query = page.query.trim().to_lowercase();
    setting_rows(state, page.tab, model, session)
        .into_iter()
        .filter(|row| {
            query.is_empty()
                || row.label.to_lowercase().contains(&query)
                || row.value.to_lowercase().contains(&query)
        })
        .collect()
}

fn open_settings(state: &mut TuiState, command: &str) {
    state.settings = Some(SettingsPage {
        tab: 0,
        focus: SettingsFocus::Search,
        query: String::new(),
        selected: None,
        command: command.to_string(),
    });
    if !state.chunks.is_empty() {
        state.ensure_blank_line();
    }
    state.push_line(
        &format!("❯ {command}"),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    );
    state.push_line("", Style::default());
}

fn apply_setting(
    state: &mut TuiState,
    action: SettingAction,
    cmd_tx: &mpsc::UnboundedSender<Command>,
) {
    match action {
        SettingAction::CycleMode => {
            let _ = cmd_tx.send(Command::CycleMode);
        }
        SettingAction::ToggleTips => state.show_tips = !state.show_tips,
        SettingAction::ToggleMotion => state.reduce_motion = !state.reduce_motion,
        SettingAction::OpenModelPicker => {
            state.settings = None;
            open_model_picker(state);
        }
        SettingAction::ToggleThinking => {
            update_generation(state, cmd_tx, |generation| {
                generation.thinking = Some(match generation.thinking {
                    Some(true) => false,
                    Some(false) => true,
                    None => false,
                });
            });
        }
        SettingAction::CycleEffort => {
            update_generation(state, cmd_tx, |generation| {
                let current = generation
                    .reasoning_effort
                    .clone()
                    .unwrap_or_else(|| "high".to_string());
                generation.reasoning_effort = Some(cycle_effort(&current));
            });
        }
        SettingAction::CycleMaxTokens => {
            update_generation(state, cmd_tx, |generation| {
                generation.max_tokens = cycle_max_tokens(generation.max_tokens);
            });
        }
        SettingAction::CycleTemperature => {
            update_generation(state, cmd_tx, |generation| {
                generation.temperature = cycle_temperature(generation.temperature);
            });
        }
    }
}

fn update_generation(
    state: &mut TuiState,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    update: impl FnOnce(&mut GenerationSettings),
) {
    let mut generation = state.generation.clone();
    update(&mut generation);
    state.generation = generation.clone();
    let _ = generation.save(&state.deepseek_settings_path);
    let _ = cmd_tx.send(Command::SetGeneration(generation));
}

fn handle_settings_key(
    state: &mut TuiState,
    key: KeyEvent,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    model: &str,
    session: &str,
) {
    let Some(mut page) = state.settings.take() else {
        return;
    };
    let tab_count = settings_tabs(state.deepseek_mode).len();
    let rows = filtered_setting_rows(&page, state, model, session);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let select = |page: &mut SettingsPage| {
        if !rows.is_empty() {
            page.focus = SettingsFocus::List;
            page.selected = Some(0);
        }
    };
    let mut keep = true;
    match key.code {
        KeyCode::Esc => {
            if page.query.is_empty() {
                keep = false;
            } else {
                page.query.clear();
                page.selected = None;
                page.focus = SettingsFocus::Search;
            }
        }
        KeyCode::Up => match page.focus {
            SettingsFocus::List => {
                if page.selected.unwrap_or(0) == 0 {
                    page.focus = SettingsFocus::Tabs;
                    page.selected = None;
                } else {
                    page.selected = Some(page.selected.unwrap_or(0) - 1);
                }
            }
            SettingsFocus::Search => {
                page.focus = SettingsFocus::Tabs;
                page.selected = None;
            }
            SettingsFocus::Tabs => {}
        },
        KeyCode::Down => match page.focus {
            SettingsFocus::Tabs | SettingsFocus::Search => select(&mut page),
            SettingsFocus::List => {
                let last = rows.len().saturating_sub(1);
                page.selected = Some((page.selected.unwrap_or(0) + 1).min(last));
            }
        },
        KeyCode::Left | KeyCode::Right if page.focus == SettingsFocus::Tabs => {
            let step = if key.code == KeyCode::Right {
                1
            } else {
                tab_count - 1
            };
            page.tab = (page.tab + step) % tab_count;
            page.query.clear();
            page.selected = None;
        }
        KeyCode::Tab => {
            page.tab = (page.tab + 1) % tab_count;
            page.focus = SettingsFocus::Tabs;
            page.query.clear();
            page.selected = None;
        }
        KeyCode::BackTab => {
            page.tab = (page.tab + tab_count - 1) % tab_count;
            page.focus = SettingsFocus::Tabs;
            page.query.clear();
            page.selected = None;
        }
        KeyCode::Enter => {
            if page.focus == SettingsFocus::List {
                if let Some(action) = page
                    .selected
                    .and_then(|index| rows.get(index))
                    .and_then(|row| row.action)
                {
                    apply_setting(state, action, cmd_tx);
                }
            } else {
                select(&mut page);
            }
        }
        KeyCode::Char(' ') if page.focus != SettingsFocus::Search => {
            if page.focus == SettingsFocus::List {
                if let Some(action) = page
                    .selected
                    .and_then(|index| rows.get(index))
                    .and_then(|row| row.action)
                {
                    apply_setting(state, action, cmd_tx);
                }
            } else {
                select(&mut page);
            }
        }
        KeyCode::Backspace => {
            if page.focus == SettingsFocus::Search {
                page.query.pop();
            } else {
                page.focus = SettingsFocus::Search;
            }
            page.selected = None;
        }
        KeyCode::Char(ch) if !ctrl && !alt => {
            if ch == '/' && page.focus != SettingsFocus::Search {
                page.focus = SettingsFocus::Search;
                page.query.clear();
            } else {
                page.focus = SettingsFocus::Search;
                page.query.push(ch);
            }
            page.selected = None;
        }
        _ => {}
    }
    if keep && state.picker.is_none() {
        state.settings = Some(page);
    }
}

fn display_lines(chunks: &[Chunk], width: u16) -> Vec<Line<'static>> {
    let inner = (width as usize).saturating_sub(2).max(1);
    let mut out = Vec::new();
    for row in styled_rows(chunks) {
        let text: String = row.iter().map(|(ch, _)| *ch).collect();
        let (marker, body_start) = if text.starts_with("❯ ") {
            (Some('❯'), 2)
        } else if text.starts_with("● ") {
            (Some('●'), 2)
        } else if text.starts_with("⏺ ") {
            (Some('⏺'), 2)
        } else if text.starts_with("  ") {
            (None, 2)
        } else {
            (None, 0)
        };
        let marker_style = row.first().map(|(_, style)| *style).unwrap_or_default();
        let body = &row[body_start.min(row.len())..];
        push_wrapped(&mut out, body, marker, marker_style, inner);
    }
    out
}

struct TuiGuard;

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

enum Incoming {
    Key(KeyEvent),
    Agent(Event),
    Interaction(Interaction),
    Tick,
}

pub async fn run_tui(
    cmd_tx: mpsc::UnboundedSender<Command>,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
    mut interaction_rx: mpsc::UnboundedReceiver<Interaction>,
    cancel: CancelHandle,
    queue: Arc<Mutex<VecDeque<String>>>,
    model: String,
    models: Vec<String>,
    generation: GenerationSettings,
    context_window: usize,
    deepseek_mode: bool,
    session_id: String,
    initial_mode: &str,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _guard = TuiGuard;

    let mut key_rx = crate::spawn_key_reader();
    let mut state = TuiState::new(initial_mode.to_string());
    state.model = model.clone();
    state.models = models;
    state.generation = generation;
    state.context_window = context_window;
    state.deepseek_mode = deepseek_mode;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut quit = false;
    while !quit {
        terminal.draw(|frame| render(frame, &mut state, &model, &session_id))?;

        let incoming = tokio::select! {
            biased;
            Some(key) = key_rx.recv() => Incoming::Key(key),
            Some(event) = event_rx.recv() => Incoming::Agent(event),
            Some(interaction) = interaction_rx.recv() => Incoming::Interaction(interaction),
            _ = tick.tick() => Incoming::Tick,
            else => break,
        };

        match incoming {
            Incoming::Tick => {
                if !state.reduce_motion {
                    state.mascot_frame = state.mascot_frame.wrapping_add(1);
                }
                if state.busy {
                    state.spinner = state.spinner.wrapping_add(1);
                }
            }
            Incoming::Key(key) => {
                quit = handle_key(
                    &mut state,
                    key,
                    &cmd_tx,
                    &cancel,
                    &queue,
                    &model,
                    &session_id,
                );
            }
            Incoming::Agent(event) => handle_event(&mut state, event),
            Incoming::Interaction(interaction) => handle_interaction(&mut state, interaction),
        }
    }

    drop(terminal);
    Ok(())
}

fn handle_key(
    state: &mut TuiState,
    key: KeyEvent,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    cancel: &CancelHandle,
    queue: &Arc<Mutex<VecDeque<String>>>,
    model: &str,
    session: &str,
) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && matches!(key.code, KeyCode::Char('c')) {
        return true;
    }

    if state.settings.is_some() {
        handle_settings_key(state, key, cmd_tx, model, session);
        return false;
    }

    if state.picker.is_some() {
        handle_picker_key(state, key, cmd_tx);
        return false;
    }

    if state.pending.is_some() {
        handle_pending(state, key);
        return false;
    }

    if state.plan_pending {
        match key.code {
            KeyCode::Up => state.plan_selection = 0,
            KeyCode::Down => state.plan_selection = 1,
            KeyCode::Enter => {
                let execute = state.plan_selection == 0;
                state.plan_pending = false;
                if execute {
                    let _ = cmd_tx.send(Command::SetMode("normal".to_string()));
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                state.plan_pending = false;
                let _ = cmd_tx.send(Command::SetMode("normal".to_string()));
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                state.plan_pending = false;
            }
            _ => {}
        }
        return false;
    }

    if ctrl && matches!(key.code, KeyCode::Char('l')) {
        state.clear();
        return false;
    }

    if matches!(key.code, KeyCode::BackTab) {
        let _ = cmd_tx.send(Command::CycleMode);
        return false;
    }

    match key.code {
        KeyCode::Esc => {
            if state.busy {
                cancel.cancel();
                state.push_line("· interrupted", Style::default().fg(YELLOW));
            } else {
                state.input.clear();
            }
        }
        KeyCode::Enter => submit(state, cmd_tx, queue),
        KeyCode::Up => {
            state.follow = false;
            state.scroll = state.scroll.saturating_sub(1);
        }
        KeyCode::Down => {
            state.scroll = state.scroll.saturating_add(1);
        }
        KeyCode::PageUp => {
            state.follow = false;
            state.scroll = state.scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.scroll = state.scroll.saturating_add(10);
        }
        KeyCode::Home => {
            state.follow = false;
            state.scroll = 0;
        }
        KeyCode::End => {
            state.follow = true;
        }
        KeyCode::Backspace => {
            state.input.pop();
        }
        KeyCode::Char(ch) => {
            if !ctrl && !alt {
                state.input.push(ch);
            }
        }
        _ => {}
    }
    false
}

fn handle_picker_key(state: &mut TuiState, key: KeyEvent, cmd_tx: &mpsc::UnboundedSender<Command>) {
    let choice = match key.code {
        KeyCode::Esc => {
            state.picker = None;
            return;
        }
        KeyCode::Up => {
            if let Some(picker) = state.picker.as_mut() {
                let len = picker.options.len();
                if len > 0 {
                    picker.selected = if picker.selected == 0 {
                        len - 1
                    } else {
                        picker.selected - 1
                    };
                }
            }
            return;
        }
        KeyCode::Down => {
            if let Some(picker) = state.picker.as_mut() {
                let len = picker.options.len();
                if len > 0 {
                    picker.selected = (picker.selected + 1) % len;
                }
            }
            return;
        }
        KeyCode::Enter => {
            let Some(picker) = state.picker.as_ref() else {
                return;
            };
            let Some(option) = picker.options.get(picker.selected) else {
                return;
            };
            (picker.kind, option.value.clone())
        }
        _ => return,
    };

    state.picker = None;
    match choice.0 {
        PickerKind::Mode => {
            if agent_core::Mode::parse(&choice.1).is_some() {
                state.mode = choice.1.clone();
                let _ = cmd_tx.send(Command::SetMode(choice.1));
            }
        }
        PickerKind::Model => apply_model_choice(state, &choice.1, cmd_tx),
    }
}

fn apply_model_choice(state: &mut TuiState, value: &str, cmd_tx: &mpsc::UnboundedSender<Command>) {
    state.model = value.to_string();
    let _ = cmd_tx.send(Command::SetModel(value.to_string()));
}

fn model_label(model: &str) -> String {
    match model.to_ascii_lowercase().as_str() {
        "deepseek-flash" => "DeepSeek Flash".to_string(),
        "deepseek-chat" => "DeepSeek Chat".to_string(),
        "deepseek-v4-pro" => "DeepSeek V4 Pro".to_string(),
        _ => model.to_string(),
    }
}

fn model_description(model: &str) -> &'static str {
    match model.to_ascii_lowercase().as_str() {
        "deepseek-flash" => "fast · vision",
        "deepseek-chat" => "balanced · vision",
        "deepseek-v4-pro" => "reasoning · text only",
        _ => "",
    }
}

fn open_model_picker(state: &mut TuiState) {
    let mut names = state.models.clone();
    if names.is_empty() {
        names.push(state.model.clone());
    } else if !names.iter().any(|name| name == &state.model) {
        names.insert(0, state.model.clone());
    }
    let options: Vec<PickerOption> = names
        .iter()
        .map(|name| PickerOption {
            value: name.clone(),
            label: model_label(name),
            description: model_description(name).to_string(),
            current: name == &state.model,
        })
        .collect();
    let selected = options
        .iter()
        .position(|option| option.current)
        .unwrap_or(0);
    state.picker = Some(PickerState {
        kind: PickerKind::Model,
        title: "Select model".to_string(),
        subtitle: "Applies to this session · switch at any time".to_string(),
        options,
        selected,
    });
}

fn open_mode_picker(state: &mut TuiState) {
    let options: Vec<PickerOption> = [
        ("normal", "Normal", "Ask before edits and commands"),
        ("plan", "Plan", "Read-only research, then propose a plan"),
        ("auto", "Auto", "Auto-accept edits and commands"),
    ]
    .iter()
    .map(|(value, label, description)| PickerOption {
        value: value.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        current: state.mode == *value,
    })
    .collect();
    let selected = options
        .iter()
        .position(|option| option.current)
        .unwrap_or(0);
    state.picker = Some(PickerState {
        kind: PickerKind::Mode,
        title: "Select mode".to_string(),
        subtitle: "Controls how edits and commands are approved".to_string(),
        options,
        selected,
    });
}

fn submit(
    state: &mut TuiState,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    queue: &Arc<Mutex<VecDeque<String>>>,
) {
    let text = state.input.trim().to_string();
    if text.is_empty() {
        return;
    }
    state.input.clear();
    if text.starts_with('/') {
        handle_slash(state, &text, cmd_tx);
        return;
    }
    if state.busy {
        if let Ok(mut pending) = queue.lock() {
            pending.push_back(text);
        }
        state.push_line("[queued]", dim());
        return;
    }
    if cmd_tx.send(Command::UserInput(text)).is_ok() {
        state.busy = true;
        state.turn_text = false;
        state.scanning = false;
        state.spinner = 0;
        state.follow = true;
    }
}

fn handle_slash(state: &mut TuiState, text: &str, cmd_tx: &mpsc::UnboundedSender<Command>) {
    let mut parts = text.split_whitespace();
    match parts.next().unwrap_or_default() {
        "/help" => {
            state.push_line("/help        show commands", dim());
            state.push_line("/config      settings and status", dim());
            state.push_line("/model       choose a model (↑/↓)", dim());
            state.push_line("/mode [m]    choose a mode (↑/↓)", dim());
            state.push_line("/plan        show the recorded plan", dim());
            state.push_line("/skills      list available skills", dim());
            state.push_line("/cat <pose>  idle | scanning | working | success", dim());
            state.push_line("/compact     summarize history now", dim());
            state.push_line("/clear       clear conversation history", dim());
            state.push_line("/cost        show token totals", dim());
        }
        "/config" | "/settings" => {
            if state.busy {
                state.push_line("settings unavailable while working", dim());
            } else {
                open_settings(state, text);
            }
        }
        "/model" => match parts.next() {
            Some(value) => apply_model_choice(state, value, cmd_tx),
            None => open_model_picker(state),
        },
        "/mode" => match parts.next() {
            Some(value) if agent_core::Mode::parse(value).is_some() => {
                let _ = cmd_tx.send(Command::SetMode(value.to_string()));
            }
            Some(_) => state.push_line("usage: /mode normal|plan|auto", dim()),
            None => open_mode_picker(state),
        },
        "/plan" => {
            let _ = cmd_tx.send(Command::ShowPlan);
        }
        "/skills" => {
            let _ = cmd_tx.send(Command::ShowSkills);
        }
        "/cat" => {
            let pose = parts.next().and_then(Pose::parse).unwrap_or(Pose::Idle);
            state.pose_override = Some((pose, Instant::now() + Duration::from_secs(6)));
        }
        "/compact" => {
            let _ = cmd_tx.send(Command::Compact);
        }
        "/clear" => {
            let _ = cmd_tx.send(Command::Clear);
        }
        "/cost" => {
            state.push_line(
                &format!("tokens in {} / out {}", state.total_in, state.total_out),
                dim(),
            );
        }
        other => state.push_line(&format!("unknown command {other}; try /help"), dim()),
    }
}

fn handle_pending(state: &mut TuiState, key: KeyEvent) {
    if matches!(state.pending, Some(Pending::Ask { .. })) {
        match key.code {
            KeyCode::Enter => {
                let answer = state.ask_input.trim().to_string();
                state.ask_input.clear();
                if let Some(Pending::Ask { options, reply, .. }) = state.pending.take() {
                    let resolved = if answer.is_empty() {
                        None
                    } else if let Ok(index) = answer.parse::<usize>() {
                        options
                            .get(index.saturating_sub(1))
                            .cloned()
                            .or(Some(answer))
                    } else {
                        Some(answer)
                    };
                    let _ = reply.send(resolved);
                }
            }
            KeyCode::Esc => {
                if let Some(Pending::Ask { reply, .. }) = state.pending.take() {
                    let _ = reply.send(None);
                }
                state.ask_input.clear();
            }
            KeyCode::Backspace => {
                state.ask_input.pop();
            }
            KeyCode::Char(ch) => state.ask_input.push(ch),
            _ => {}
        }
        return;
    }

    let decision = match key.code {
        KeyCode::Up => {
            state.approval_selection = state.approval_selection.saturating_sub(1);
            None
        }
        KeyCode::Down => {
            state.approval_selection = (state.approval_selection + 1).min(2);
            None
        }
        KeyCode::Enter => Some(match state.approval_selection {
            0 => ApprovalDecision::Once,
            1 => ApprovalDecision::Always,
            _ => ApprovalDecision::Deny("denied by user".to_string()),
        }),
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalDecision::Once),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(ApprovalDecision::Always),
        KeyCode::Char('n') | KeyCode::Char('N') => {
            Some(ApprovalDecision::Deny("denied by user".to_string()))
        }
        KeyCode::Esc => Some(ApprovalDecision::Deny("cancelled by user".to_string())),
        _ => None,
    };
    if let Some(decision) = decision {
        if let Some(Pending::Approval { reply, .. }) = state.pending.take() {
            let _ = reply.send(decision);
        }
    }
}

fn handle_interaction(state: &mut TuiState, interaction: Interaction) {
    match interaction {
        Interaction::Approval { request, reply } => {
            state.picker = None;
            state.approval_selection = 0;
            state.pending = Some(Pending::Approval { request, reply });
        }
        Interaction::Ask {
            question,
            options,
            reply,
        } => {
            state.picker = None;
            state.ask_input.clear();
            state.pending = Some(Pending::Ask {
                question,
                options,
                reply,
            });
        }
    }
}

fn handle_event(state: &mut TuiState, event: Event) {
    match event {
        Event::UserMessage { content } => {
            state.turn_text = false;
            state.answer_open = false;
            if !state.chunks.is_empty() {
                state.ensure_blank_line();
            }
            state.push_line(
                &format!("❯ {content}"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            );
            state.push_line("", Style::default());
        }
        Event::AssistantDelta { text } => {
            state.turn_text = true;
            if !state.answer_open {
                state.answer_open = true;
                state.ensure_line_start();
                state.push("⏺ ", Style::default());
            }
            state.push(&text, Style::default());
        }
        Event::AssistantReasoning { .. } => {}
        Event::ToolStart { name, args, .. } => {
            state.scanning = matches!(name.as_str(), "read" | "grep" | "glob" | "lsp" | "skill");
            state.ensure_blank_line();
            state.push_line(
                &format!("● {}", describe_tool(&name, &args)),
                Style::default().fg(ACCENT),
            );
        }
        Event::ToolProgress { name, text, .. } => {
            if name == "agent" {
                state.push_line(&format!("  · {text}"), dim());
            }
        }
        Event::ToolResult {
            content, is_error, ..
        } => {
            state.answer_open = false;
            let style = if is_error {
                Style::default().fg(RED)
            } else {
                dim()
            };
            for line in content.lines().take(4) {
                state.push_line(&format!("  └ {}", truncate(line, 160)), style);
            }
        }
        Event::ToolDenied { reason, .. } => {
            state.ensure_line_start();
            state.push_line(&format!("  └ denied: {reason}"), Style::default().fg(RED));
        }
        Event::Reminder { kind, .. } => {
            state.ensure_line_start();
            state.push_line(&format!("[harness] {kind}"), dim());
        }
        Event::TodoUpdated { todos } => {
            state.todos = todos;
        }
        Event::ModeChanged { mode } => {
            state.mode = mode.clone();
            state.plan_selection = 0;
            state.toast = Some((
                format!("mode → {mode}"),
                Instant::now() + Duration::from_secs(2),
            ));
        }
        Event::TurnFinished {
            prompt_tokens,
            completion_tokens,
            ..
        } => {
            state.busy = false;
            state.scanning = false;
            state.answer_open = false;
            state.success_until = Instant::now() + Duration::from_secs(2);
            state.total_in = state.total_in.saturating_add(prompt_tokens);
            state.total_out = state.total_out.saturating_add(completion_tokens);
            if state.mode == "plan" && state.turn_text {
                state.picker = None;
                state.plan_pending = true;
                state.plan_selection = 0;
            }
            state.turn_text = false;
        }
        Event::Compacted { removed } => {
            state.toast = Some((
                format!("context compacted · {removed} messages"),
                Instant::now() + Duration::from_secs(3),
            ));
        }
        Event::Notice { message } => {
            state.ensure_line_start();
            state.push_line(&message, dim());
        }
        Event::HistoryCleared => {
            state.clear();
            state.todos.clear();
            state.toast = Some((
                "history cleared".to_string(),
                Instant::now() + Duration::from_secs(2),
            ));
        }
        Event::Interrupted => {
            state.ensure_line_start();
            state.push_line("interrupted", Style::default().fg(YELLOW));
        }
        Event::Error { message } => {
            state.ensure_line_start();
            state.push_line(&format!("error: {message}"), Style::default().fg(RED));
        }
        Event::TurnStarted { .. } => {
            state.turns = state.turns.saturating_add(1);
        }
        Event::SessionStarted { .. } | Event::AssistantMessage { .. } => {}
    }
}

fn render(frame: &mut Frame, state: &mut TuiState, model: &str, session: &str) {
    let area = frame.area();
    if state.settings.is_some() {
        render_settings(frame, area, state, model, session);
        return;
    }
    if state.picker.is_some() {
        render_picker(frame, area, state);
        return;
    }
    let display_model = if state.model.is_empty() {
        model.to_string()
    } else {
        state.model.clone()
    };
    let dialog_height = if state.plan_pending {
        6
    } else if let Some(pending) = &state.pending {
        match pending {
            Pending::Approval { request, .. } => {
                (request.preview.lines().count().clamp(1, 3) as u16) + 7
            }
            Pending::Ask { options, .. } => (options.len().clamp(1, 4) as u16) + 6,
        }
    } else {
        0
    };
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(8),
        Constraint::Length(1),
        Constraint::Min(6),
    ];
    if dialog_height > 0 {
        constraints.push(Constraint::Length(dialog_height));
    }
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Length(
        input_view_rows(state, area.width, area.height) as u16 + 1,
    ));
    constraints.push(Constraint::Length(1));
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    render_welcome(frame, layout[1], state, &display_model);
    let _ = render_main(frame, layout[3], state);
    let mut index = 4;
    if dialog_height > 0 {
        let row = layout[index];
        let panel = Rect {
            x: row.x + 2,
            y: row.y,
            width: row.width.saturating_sub(4),
            height: row.height,
        };
        if state.plan_pending {
            render_plan_panel(frame, panel, state);
        } else {
            render_pending_panel(frame, panel, state);
        }
        index += 1;
    }
    render_hint(frame, layout[index], state);
    render_input(frame, layout[index + 1], state, &display_model);
    render_footer(frame, layout[index + 2], state);
}

fn render_main(frame: &mut Frame, area: Rect, state: &mut TuiState) -> Rect {
    let (conversation_area, todo_area) = if state.todos.is_empty() {
        (area, None)
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(24), Constraint::Length(30)])
            .split(area);
        (cols[0], Some(cols[1]))
    };

    if conversation_area.width > 8 && conversation_area.height > 2 {
        let inner = Rect {
            x: conversation_area.x,
            y: conversation_area.y + 1,
            width: conversation_area.width,
            height: conversation_area.height - 1,
        };
        let lines = display_lines(&state.chunks, inner.width);
        let total = lines.len();
        let max_scroll = total.saturating_sub(inner.height as usize) as u16;
        if state.follow {
            state.scroll = max_scroll;
        } else {
            state.scroll = state.scroll.min(max_scroll);
        }
        let paragraph = Paragraph::new(lines).scroll((state.scroll, 0));
        frame.render_widget(paragraph, inner);
    }

    if let Some(sidebar) = todo_area {
        render_todos(frame, sidebar, &state.todos);
    }

    conversation_area
}

fn render_settings(frame: &mut Frame, area: Rect, state: &TuiState, model: &str, session: &str) {
    let Some(page) = state.settings.as_ref() else {
        return;
    };
    if area.width < 12 || area.height < 8 {
        return;
    }
    let rows = filtered_setting_rows(page, state, model, session);
    let width = area.width as usize;
    let top = area.y + 1;

    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(format!("❯ {}", page.command)))),
        Rect {
            x: area.x,
            y: top,
            width: area.width,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(FOCUS),
        ))),
        Rect {
            x: area.x,
            y: top + 2,
            width: area.width,
            height: 1,
        },
    );

    let mut tab_spans = Vec::new();
    for (index, label) in settings_tabs(state.deepseek_mode).iter().enumerate() {
        if index > 0 {
            tab_spans.push(Span::raw("  "));
        }
        let style = if index == page.tab {
            Style::default().fg(FOCUS).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        tab_spans.push(Span::styled(*label, style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(tab_spans)),
        Rect {
            x: area.x + 2,
            y: top + 3,
            width: area.width.saturating_sub(2),
            height: 1,
        },
    );

    let search_rect = Rect {
        x: area.x + 2,
        y: top + 6,
        width: area.width.saturating_sub(4),
        height: 3,
    };
    let search = if page.query.is_empty() {
        Line::from(Span::styled(
            "⌕ Search settings…",
            Style::default().fg(MUTED),
        ))
    } else {
        Line::from(vec![
            Span::styled("⌕ ", Style::default().fg(MUTED)),
            Span::raw(page.query.clone()),
        ])
    };
    frame.render_widget(
        Paragraph::new(search).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(FOCUS))
                .padding(Padding::new(1, 1, 0, 0)),
        ),
        search_rect,
    );
    if page.focus == SettingsFocus::Search {
        let cursor_x = search_rect.x + 4 + UnicodeWidthStr::width(page.query.as_str()) as u16;
        let max_x = search_rect.x + search_rect.width.saturating_sub(2);
        frame.set_cursor_position((cursor_x.min(max_x), search_rect.y + 1));
    }

    let list_offset = 11usize;
    let list_start = area.y + list_offset as u16;
    let height = area.height as usize;
    let max_visible = height.saturating_sub(list_offset + 3).min(14).max(1);
    let offset = match page.selected {
        Some(index) if index >= max_visible => index - max_visible + 1,
        _ => 0,
    };
    let visible = rows.len().saturating_sub(offset).min(max_visible);

    for (index, row) in rows.iter().skip(offset).take(visible).enumerate() {
        let selected = page.focus == SettingsFocus::List && page.selected == Some(offset + index);
        let value_width = UnicodeWidthStr::width(row.value.as_str());
        let label_max = SETTINGS_COL_WIDTH.saturating_sub(value_width + 2).max(1);
        let label = if UnicodeWidthStr::width(row.label.as_str()) > label_max {
            truncate(&row.label, label_max.saturating_sub(1))
        } else {
            row.label.clone()
        };
        let label_width = UnicodeWidthStr::width(label.as_str());
        let pad = SETTINGS_COL_WIDTH.saturating_sub(label_width + value_width);
        let marker = if selected { "❯ " } else { "  " };
        let style = if selected {
            Style::default().fg(FOCUS)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{marker}{label}{}{}", " ".repeat(pad), row.value),
                style,
            ))),
            Rect {
                x: area.x + 2,
                y: list_start + index as u16,
                width: area.width.saturating_sub(4),
                height: 1,
            },
        );
    }

    let footer_y = if rows.is_empty() {
        let message = if page.query.is_empty() {
            "No settings available".to_string()
        } else {
            format!("No settings match \"{}\"", page.query)
        };
        if list_start < area.y + area.height {
            frame.render_widget(
                Paragraph::new(Line::from(message)),
                Rect {
                    x: area.x + 2,
                    y: list_start,
                    width: area.width.saturating_sub(4),
                    height: 1,
                },
            );
        }
        list_start + 2
    } else {
        let more = rows.len().saturating_sub(offset + visible);
        if more > 0 && list_start + (visible as u16) < area.y + area.height {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  ↓ {more} more below"),
                    Style::default().fg(MUTED),
                ))),
                Rect {
                    x: area.x,
                    y: list_start + visible as u16,
                    width: area.width,
                    height: 1,
                },
            );
        }
        if more > 0 {
            list_start + visible as u16 + 2
        } else {
            list_start + visible as u16 + 1
        }
    };

    if footer_y < area.y + area.height {
        let hint = match page.focus {
            SettingsFocus::Search => {
                "Type to filter · Enter/↓ to select · ↑ to tabs · Esc to clear"
            }
            SettingsFocus::Tabs => "← → switch tab · ↓ to settings · Esc to close",
            SettingsFocus::List => "Space to change · Enter to save · / to search · Esc to cancel",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {hint}"),
                Style::default().fg(MUTED),
            ))),
            Rect {
                x: area.x,
                y: footer_y,
                width: area.width,
                height: 1,
            },
        );
    }
}

fn render_picker(frame: &mut Frame, area: Rect, state: &TuiState) {
    let Some(picker) = state.picker.as_ref() else {
        return;
    };
    if area.width < 24 || area.height < 10 {
        return;
    }
    let top = area.y + 1;
    let width = area.width as usize;
    let command = match picker.kind {
        PickerKind::Model => "/model",
        PickerKind::Mode => "/mode",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(format!("❯ {command}")))),
        Rect {
            x: area.x,
            y: top,
            width: area.width,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(FOCUS),
        ))),
        Rect {
            x: area.x,
            y: top + 2,
            width: area.width,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {}", picker.title),
            Style::default().fg(FOCUS).add_modifier(Modifier::BOLD),
        ))),
        Rect {
            x: area.x,
            y: top + 4,
            width: area.width,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {}", picker.subtitle),
            dim(),
        ))),
        Rect {
            x: area.x,
            y: top + 5,
            width: area.width,
            height: 1,
        },
    );

    let list_start = top + 7;
    let footer_y = area.y + area.height.saturating_sub(2);
    let available = footer_y.saturating_sub(list_start) as usize;
    let max_visible = available.clamp(1, 10);
    let offset = if picker.selected >= max_visible {
        picker.selected + 1 - max_visible
    } else {
        0
    };
    let visible = picker.options.len().saturating_sub(offset).min(max_visible);

    if picker.options.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  No models configured", dim()))),
            Rect {
                x: area.x,
                y: list_start,
                width: area.width,
                height: 1,
            },
        );
    }

    for (index, option) in picker.options.iter().skip(offset).take(visible).enumerate() {
        let focused = offset + index == picker.selected;
        let marker = if focused { "❯ " } else { "  " };
        let marker_style = if focused {
            Style::default().fg(ACCENT)
        } else {
            dim()
        };
        let label_style = if focused {
            Style::default().fg(FOCUS).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let label = if UnicodeWidthStr::width(option.label.as_str()) > 26 {
            truncate(&option.label, 25)
        } else {
            option.label.clone()
        };
        let pad = 26usize.saturating_sub(UnicodeWidthStr::width(label.as_str()));
        let mut spans = vec![
            Span::styled(marker, marker_style),
            Span::styled(label, label_style),
            Span::raw(" ".repeat(pad + 2)),
            Span::styled(option.description.clone(), dim()),
        ];
        if option.current {
            spans.push(Span::styled("  current", Style::default().fg(OK)));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: area.x + 2,
                y: list_start + index as u16,
                width: area.width.saturating_sub(4),
                height: 1,
            },
        );
    }

    let hidden = picker.options.len().saturating_sub(offset + visible);
    let mut hint = "  ↑/↓ select · Enter confirm · Esc cancel".to_string();
    if offset > 0 {
        hint.push_str(&format!(" · ↑ {offset} above"));
    }
    if hidden > 0 {
        hint.push_str(&format!(" · ↓ {hidden} more"));
    }
    if footer_y < area.y + area.height {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, dim()))),
            Rect {
                x: area.x,
                y: footer_y,
                width: area.width,
                height: 1,
            },
        );
    }
}

fn render_welcome(frame: &mut Frame, area: Rect, state: &TuiState, model: &str) {
    let rect = Rect {
        x: area.x + 2,
        y: area.y,
        width: 62.min(area.width.saturating_sub(4)),
        height: 8,
    };
    if rect.width < 30 {
        return;
    }
    let pose = state.current_pose();
    let border = match pose {
        Pose::Working => ACCENT,
        Pose::Scanning => PLAN,
        Pose::Success => OK,
        Pose::Idle => DIM,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .padding(Padding::new(1, 1, 1, 1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 4 || inner.width < 24 {
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(14),
            Constraint::Length(2),
            Constraint::Min(20),
        ])
        .split(inner);

    let cat = compact_cat_lines(brand::compact_cells(pose, state.mascot_frame));
    frame.render_widget(Paragraph::new(cat), cols[0]);

    let mode_color = match state.mode.as_str() {
        "plan" => PLAN,
        "auto" => ACCENT,
        _ => OK,
    };
    let status = if state.busy {
        format!("working {}", brand::dots(state.spinner).trim_end())
    } else {
        match pose {
            Pose::Scanning => "scanning".to_string(),
            Pose::Success => "purring".to_string(),
            _ => "ready".to_string(),
        }
    };
    let status_color = match pose {
        Pose::Working => ACCENT,
        Pose::Scanning => PLAN,
        Pose::Success => OK,
        Pose::Idle => DIM,
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "whisker",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  a cat for your codebase", dim()),
        ]),
        Line::from(vec![
            Span::styled(format!("model  {model}"), dim()),
            Span::raw("    "),
            Span::styled(
                format!("mode  {}", state.mode),
                Style::default().fg(mode_color),
            ),
        ]),
        Line::from(Span::styled(status, Style::default().fg(status_color))),
        if state.show_tips {
            Line::from(Span::styled("enter send · /help for commands", dim()))
        } else {
            Line::from("")
        },
    ];
    frame.render_widget(Paragraph::new(lines), cols[2]);
}

fn compact_cat_lines(cells: Vec<Vec<brand::CellSpec>>) -> Vec<Line<'static>> {
    cells
        .into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|cell| {
                        let mut style = Style::default();
                        if let Some(fg) = cell.fg {
                            style = style.fg(Color::Indexed(fg));
                        }
                        if let Some(bg) = cell.bg {
                            style = style.bg(Color::Indexed(bg));
                        }
                        Span::styled(cell.ch.to_string(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn render_hint(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(4),
        height: 1,
    };
    if inner.width < 8 {
        return;
    }
    if let Some((text, until)) = &state.toast {
        if Instant::now() < *until {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text.clone(),
                    Style::default().fg(ACCENT),
                ))),
                inner,
            );
            return;
        }
    }
    if state.pending.is_some() || state.plan_pending || !state.show_tips {
        return;
    }
    if state.input.starts_with('/') {
        let filter = state.input.trim_start_matches('/').to_ascii_lowercase();
        const COMMANDS: &[(&str, &str)] = &[
            ("help", "show commands"),
            ("config", "settings and status"),
            ("model", "select model"),
            ("mode", "select mode"),
            ("plan", "show the plan"),
            ("skills", "list skills"),
            ("cat", "mascot poses"),
            ("compact", "summarize history"),
            ("clear", "clear history"),
            ("cost", "token totals"),
        ];
        let text = if filter.is_empty() {
            COMMANDS
                .iter()
                .map(|(name, _)| format!("/{name}"))
                .collect::<Vec<_>>()
                .join("   ")
        } else {
            let mut matches = Vec::new();
            for (name, description) in COMMANDS {
                if name.starts_with(&filter) {
                    matches.push(format!("/{name}  {description}"));
                }
            }
            matches.join("   ")
        };
        if !text.is_empty() {
            let text = format!("  {}", truncate(&text, inner.width as usize - 2));
            frame.render_widget(Paragraph::new(Line::from(Span::styled(text, dim()))), inner);
        }
    }
}

fn render_input(frame: &mut Frame, area: Rect, state: &TuiState, model: &str) {
    let busy = state.busy;
    let border = if busy { DIM } else { ACCENT };
    let title = if busy {
        format!(" working · {model} ")
    } else {
        format!(" {} · {model} ", state.mode)
    };
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title_bottom(Line::from(Span::styled(title, dim())).alignment(Alignment::Right));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width < 5 {
        return;
    }
    let display = input_display(state);
    let wrapped = wrap_input(display, input_text_width(area.width));
    let start = wrapped.len().saturating_sub(inner.height as usize);
    let placeholder = if matches!(state.pending, Some(Pending::Ask { .. })) {
        "Type your answer…"
    } else if state.mode == "plan" {
        "Describe a task to plan…"
    } else {
        "Ask whisker anything…"
    };
    let mut lines: Vec<Line> = Vec::new();
    for (offset, segment) in wrapped.iter().enumerate().skip(start) {
        let mut spans = vec![Span::styled(
            if offset == start { "❯ " } else { "  " },
            Style::default().fg(ACCENT),
        )];
        if display.is_empty() {
            spans.push(Span::styled(placeholder, dim()));
        } else {
            spans.push(Span::raw(segment.clone()));
        }
        lines.push(Line::from(spans));
    }
    let visible_rows = lines.len();
    frame.render_widget(Paragraph::new(lines), inner);
    let last = wrapped.last().map(String::as_str).unwrap_or_default();
    let cursor_x = inner.x + 2 + UnicodeWidthStr::width(last) as u16;
    let cursor_y = inner.y + visible_rows.saturating_sub(1) as u16;
    let max_x = inner.x + inner.width.saturating_sub(1);
    frame.set_cursor_position((
        cursor_x.min(max_x),
        cursor_y.min(inner.y + inner.height - 1),
    ));
}

fn render_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(4),
        height: 1,
    };
    if inner.width < 20 {
        return;
    }
    let left = match &state.pending {
        Some(Pending::Approval { .. }) => "↑↓ select · enter confirm · esc cancel",
        Some(Pending::Ask { .. }) => "enter answer · esc dismiss",
        None if state.plan_pending => "↑↓ select · enter confirm · esc keep planning",
        None if state.busy => "esc interrupt · enter queue a message",
        None => "enter send · shift+tab mode · / commands",
    };
    let right = format!("tokens {} / {}", state.total_in, state.total_out);
    let pad = inner
        .width
        .saturating_sub((left.chars().count() + right.chars().count()) as u16)
        as usize;
    let line = Line::from(vec![
        Span::styled(left, dim()),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, dim()),
    ]);
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_todos(frame: &mut Frame, area: Rect, todos: &[TodoItem]) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .padding(Padding::new(1, 1, 0, 0))
        .title(Line::from(Span::styled("todos", dim())));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    for todo in todos {
        let (marker, color) = match todo.status {
            TodoStatus::Completed => ("☑", OK),
            TodoStatus::InProgress => ("▶", ACCENT),
            TodoStatus::Pending => ("☐", DIM),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), Style::default().fg(color)),
            Span::raw(todo.content.clone()),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_pending_panel(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::new(2, 2, 0, 0));
    let inner = block.inner(area);
    match state.pending.as_ref() {
        Some(Pending::Approval { request, .. }) => {
            let block = block.title(Line::from(Span::styled(
                format!(" approval · {} ", request.tool),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let mut lines: Vec<Line> = Vec::new();
            for line in request.preview.lines().take(3) {
                lines.push(Line::from(Span::styled(truncate(line, 110), dim())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("rule: {}", request.rule),
                dim(),
            )));
            let options = ["Yes", "Yes, allow always", "No"];
            for (index, label) in options.iter().enumerate() {
                let selected = index == state.approval_selection;
                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(
                    format!("{} {label}", if selected { "❯" } else { " " }),
                    style,
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Some(Pending::Ask {
            question, options, ..
        }) => {
            let block = block.title(Line::from(Span::styled(
                " question ",
                Style::default().fg(PLAN).add_modifier(Modifier::BOLD),
            )));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let mut lines = vec![Line::from(Span::raw(question.clone())), Line::from("")];
            for (index, option) in options.iter().take(4).enumerate() {
                lines.push(Line::from(Span::styled(
                    format!("{}. {option}", index + 1),
                    dim(),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Type your answer in the input below.",
                dim(),
            )));
            frame.render_widget(Paragraph::new(lines), inner);
        }
        None => {
            let _ = inner;
        }
    }
}

fn render_plan_panel(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(PLAN))
        .padding(Padding::new(2, 2, 0, 0))
        .title(Line::from(Span::styled(
            " plan ready ",
            Style::default().fg(PLAN).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "The plan is ready in the conversation above.",
            dim(),
        )),
        Line::from(""),
    ];
    let options = ["Execute plan", "Keep planning"];
    for (index, label) in options.iter().enumerate() {
        let selected = index == state.plan_selection;
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(PLAN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{} {label}", if selected { "❯" } else { " " }),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "))
            .collect()
    }

    #[test]
    fn renders_chinese_input_without_filtering() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = TuiState::new("normal".to_string());
        state.input = "鹈鹕在海边骑车，测试中文输入。".to_string();
        terminal
            .draw(|frame| render(frame, &mut state, "mock", "session"))
            .expect("draw");
        let text: String = buffer_text(&terminal)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(text.contains("鹈鹕在海边骑车"));
        assert!(text.contains("测试中文输入"));
        assert_eq!(state.input.chars().count(), 15);
    }

    #[test]
    fn renders_header_and_welcome() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = TuiState::new("normal".to_string());
        terminal
            .draw(|frame| render(frame, &mut state, "deepseek-chat", "20261109-abcdef12"))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("whisker"));
        assert!(text.contains("a cat for your codebase"));
        assert!(text.contains("deepseek-chat"));
        assert!(text.contains('▀') || text.contains('█'));
    }

    #[test]
    fn renders_approval_modal() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = TuiState::new("normal".to_string());
        let (reply, _receiver) = oneshot::channel();
        state.pending = Some(Pending::Approval {
            request: ApprovalRequest {
                tool: "bash".to_string(),
                preview: "$ rm -rf /tmp/demo".to_string(),
                rule: "bash(rm -rf /tmp/demo)".to_string(),
            },
            reply,
        });
        terminal
            .draw(|frame| render(frame, &mut state, "mock", "session"))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("approval"));
        assert!(text.contains("rm -rf /tmp/demo"));
        assert!(text.contains("Yes"));
        assert!(text.contains("allow always"));
    }

    #[test]
    fn mascot_pose_follows_state() {
        let mut state = TuiState::new("normal".to_string());
        assert_eq!(state.current_pose(), Pose::Idle);
        state.busy = true;
        assert_eq!(state.current_pose(), Pose::Working);
        state.scanning = true;
        assert_eq!(state.current_pose(), Pose::Scanning);
        state.busy = false;
        state.scanning = false;
        state.success_until = Instant::now() + Duration::from_secs(1);
        assert_eq!(state.current_pose(), Pose::Success);
        state.pose_override = Some((Pose::Idle, Instant::now() + Duration::from_secs(1)));
        assert_eq!(state.current_pose(), Pose::Idle);
    }

    #[test]
    fn wraps_input_by_display_width() {
        assert_eq!(wrap_input("abc", 10), vec!["abc"]);
        assert_eq!(wrap_input("一二三四五六", 10), vec!["一二三四五", "六"]);
        assert_eq!(wrap_input("a\nb", 10), vec!["a", "b"]);
        assert_eq!(wrap_input("", 10), vec![""]);
        assert_eq!(wrap_input("hello world", 5), vec!["hello", "world"]);
        assert_eq!(wrap_input("abc def", 5), vec!["abc", "def"]);
        assert_eq!(
            wrap_input("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn renders_wrapped_input_with_two_column_alignment() {
        let backend = TestBackend::new(60, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = TuiState::new("normal".to_string());
        state.input = "a".repeat(150);
        terminal
            .draw(|frame| render(frame, &mut state, "mock", "session"))
            .expect("draw");
        let prompt_row = (0..30)
            .find(|y| row_text(&terminal, *y).starts_with('❯'))
            .expect("prompt row");
        let first = row_text(&terminal, prompt_row);
        let second = row_text(&terminal, prompt_row + 1);
        assert!(first.starts_with("❯ a"));
        assert_eq!(
            first.chars().skip(2).take_while(|ch| *ch == 'a').count(),
            57
        );
        assert!(second.starts_with("  a"));
        assert!(row_text(&terminal, prompt_row + 2).starts_with("  a"));
        let border = row_text(&terminal, prompt_row + 3);
        assert!(border.starts_with('─'), "border row: {border:?}");
        assert!(border.contains("normal · mock"));
    }

    #[test]
    fn appends_streaming_text_to_one_line() {
        let mut state = TuiState::new("normal".to_string());
        state.clear();
        state.push("hello", Style::default());
        state.push(" world", Style::default());
        let lines = display_lines(&state.chunks, 100);
        assert_eq!(lines.len(), 1);
        let rendered: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(rendered.trim_start(), "hello world");
    }

    fn settings_state() -> TuiState {
        let mut state = TuiState::new("normal".to_string());
        state.settings = Some(SettingsPage {
            tab: 0,
            focus: SettingsFocus::Search,
            query: String::new(),
            selected: None,
            command: "/config".to_string(),
        });
        state
    }

    #[test]
    fn settings_page_renders_claude_layout() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = settings_state();
        terminal
            .draw(|frame| render(frame, &mut state, "mock-model", "session-id"))
            .expect("draw");
        assert_eq!(row_text(&terminal, 1).trim_end(), "❯ /config");
        assert!(row_text(&terminal, 3).starts_with('─'));
        assert!(row_text(&terminal, 4).starts_with("  Settings"));
        assert!(row_text(&terminal, 7).starts_with("  ╭"));
        assert!(row_text(&terminal, 8).contains("⌕ Search settings…"));
        let row = row_text(&terminal, 11);
        assert!(row.starts_with("    Permission mode"));
        assert!(row.contains("normal"));
        assert_eq!(row.find("normal").map(|index| index + 6), Some(51));
        let footer = row_text(&terminal, 17);
        assert!(footer.starts_with("  Type to filter ·"));
        assert!(!buffer_text(&terminal).contains("a cat for your codebase"));
    }

    fn picker_state() -> TuiState {
        let mut state = TuiState::new("normal".to_string());
        state.model = "deepseek-flash".to_string();
        state.models = vec!["deepseek-flash".to_string(), "deepseek-v4-pro".to_string()];
        state
    }

    #[test]
    fn model_picker_uses_arrows_and_switches_model() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = picker_state();
        open_model_picker(&mut state);
        let picker = state.picker.as_ref().expect("picker");
        assert_eq!(picker.kind, PickerKind::Model);
        assert_eq!(picker.options[picker.selected].value, "deepseek-flash");
        terminal
            .draw(|frame| render(frame, &mut state, "ignored", "session"))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("Select model"));
        assert!(text.contains("DeepSeek Flash"));
        assert!(text.contains("DeepSeek V4 Pro"));
        assert!(text.contains("current"));
        assert!(!text.contains("a cat for your codebase"));

        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(state.picker.as_ref().map(|picker| picker.selected), Some(1));
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(state.picker.is_none());
        assert_eq!(state.model, "deepseek-v4-pro");
        assert!(
            matches!(rx.try_recv(), Ok(Command::SetModel(model)) if model == "deepseek-v4-pro")
        );
    }

    #[test]
    fn mode_picker_uses_arrows_and_sends_mode() {
        let mut state = TuiState::new("normal".to_string());
        open_mode_picker(&mut state);
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &tx,
        );
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(state.picker.is_none());
        assert_eq!(state.mode, "plan");
        assert!(matches!(rx.try_recv(), Ok(Command::SetMode(mode)) if mode == "plan"));
    }

    #[test]
    fn picker_cancel_and_slash_open() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut state = picker_state();
        handle_slash(&mut state, "/model", &tx);
        assert_eq!(
            state.picker.as_ref().map(|picker| picker.kind),
            Some(PickerKind::Model)
        );
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );
        assert!(state.picker.is_none());
        assert!(rx.try_recv().is_err());

        handle_slash(&mut state, "/mode", &tx);
        assert_eq!(
            state.picker.as_ref().map(|picker| picker.kind),
            Some(PickerKind::Mode)
        );
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );

        handle_slash(&mut state, "/model deepseek-v4-pro", &tx);
        assert!(state.picker.is_none());
        assert_eq!(state.model, "deepseek-v4-pro");
        assert!(
            matches!(rx.try_recv(), Ok(Command::SetModel(model)) if model == "deepseek-v4-pro")
        );
    }

    #[test]
    fn settings_model_row_opens_picker() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut state = picker_state();
        state.settings = Some(SettingsPage {
            tab: 0,
            focus: SettingsFocus::List,
            query: String::new(),
            selected: Some(1),
            command: "/config".to_string(),
        });
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        assert!(state.settings.is_none());
        assert_eq!(
            state.picker.as_ref().map(|picker| picker.kind),
            Some(PickerKind::Model)
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn deepseek_tab_renders_and_updates_generation() {
        let dir = std::env::temp_dir().join(format!(
            "whisker-deepseek-settings-{}-tab",
            std::process::id()
        ));
        let path = dir.join("settings.json");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut state = picker_state();
        state.deepseek_mode = true;
        state.context_window = 1_048_576;
        state.generation = GenerationSettings {
            temperature: Some(0.2),
            max_tokens: None,
            top_p: None,
            thinking: Some(true),
            reasoning_effort: Some("high".to_string()),
        };
        state.deepseek_settings_path = path.clone();
        state.settings = Some(SettingsPage {
            tab: 1,
            focus: SettingsFocus::List,
            query: String::new(),
            selected: Some(1),
            command: "/config".to_string(),
        });

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(frame, &mut state, "ignored", "session"))
            .expect("draw");
        assert!(row_text(&terminal, 4).contains("DeepSeek"));
        assert!(row_text(&terminal, 11).contains("Model"));
        assert!(row_text(&terminal, 12).contains("Thinking"));
        assert!(row_text(&terminal, 12).contains("on"));
        assert!(row_text(&terminal, 13).contains("Reasoning effort"));
        assert!(row_text(&terminal, 13).contains("high"));
        assert!(row_text(&terminal, 14).contains("Max output tokens"));
        assert!(row_text(&terminal, 14).contains("auto"));
        assert!(row_text(&terminal, 15).contains("Temperature"));
        assert!(row_text(&terminal, 15).contains("0.2"));
        assert!(row_text(&terminal, 16).contains("Context window"));
        assert!(row_text(&terminal, 16).contains("1M"));

        for selected in [1, 2, 3, 4] {
            if let Some(page) = state.settings.as_mut() {
                page.selected = Some(selected);
            }
            handle_settings_key(
                &mut state,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &tx,
                "mock-model",
                "session-id",
            );
        }
        assert_eq!(state.generation.thinking, Some(false));
        assert_eq!(state.generation.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(state.generation.max_tokens, Some(8_192));
        assert_eq!(state.generation.temperature, Some(0.5));
        let saved = GenerationSettings::load(&path).expect("saved settings");
        assert_eq!(saved.thinking, Some(false));
        assert_eq!(saved.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(saved.max_tokens, Some(8_192));
        assert_eq!(saved.temperature, Some(0.5));
        for _ in 0..4 {
            assert!(matches!(rx.try_recv(), Ok(Command::SetGeneration(_))));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_search_and_toggles() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut state = settings_state();
        for ch in "tips".chars() {
            handle_settings_key(
                &mut state,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &tx,
                "mock-model",
                "session-id",
            );
        }
        let page = state.settings.as_ref().expect("settings page");
        assert_eq!(page.query, "tips");
        let rows = filtered_setting_rows(page, &state, "mock-model", "session-id");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Show tips");
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        assert_eq!(
            state.settings.as_ref().map(|page| page.focus),
            Some(SettingsFocus::List)
        );
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        assert!(!state.show_tips);
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        assert!(state.settings.is_some());
        assert_eq!(
            state.settings.as_ref().map(|page| page.query.as_str()),
            Some("")
        );
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        assert!(state.settings.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn settings_mode_row_sends_cycle_command() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut state = settings_state();
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        handle_settings_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
            "mock-model",
            "session-id",
        );
        assert!(matches!(rx.try_recv(), Ok(Command::CycleMode)));
    }

    #[test]
    fn user_and_assistant_text_align_at_column_two() {
        let mut state = TuiState::new("normal".to_string());
        handle_event(
            &mut state,
            Event::UserMessage {
                content: "question ".repeat(12).trim_end().to_string(),
            },
        );
        handle_event(
            &mut state,
            Event::AssistantDelta {
                text: "answer ".repeat(12).trim_end().to_string(),
            },
        );
        let lines = display_lines(&state.chunks, 60);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(rendered[0].starts_with("❯ question"));
        assert!(rendered.iter().any(|line| line.starts_with("⏺ answer")));
        for line in &rendered {
            if line.trim().is_empty() {
                continue;
            }
            assert!(
                line.starts_with("❯ ") || line.starts_with("⏺ ") || line.starts_with("  "),
                "unexpected prefix: {line:?}"
            );
            assert!(
                line.chars().nth(2) != Some(' '),
                "text drifted from column two: {line:?}"
            );
        }
    }
}
