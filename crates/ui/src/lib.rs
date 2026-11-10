mod brand;
mod tui;

pub use tui::run_tui;

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

use agent_core::{
    Agent, ApprovalDecision, ApprovalRequest, CancelHandle, Event, GenerationSettings, Interactor,
    Mode, TerminalReason, TodoItem, TodoStatus,
};
use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub enum Command {
    UserInput(String),
    CycleMode,
    SetMode(String),
    SetModel(String),
    SetGeneration(GenerationSettings),
    SetContextWindow(usize),
    Compact,
    Clear,
    ShowPlan,
    ShowSkills,
    /// Invoke a loaded skill by its slash name, for example `/impeccable`.
    Skill {
        name: String,
        args: String,
    },
    Quit,
}

pub enum Interaction {
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

pub struct ChannelInteractor {
    tx: mpsc::UnboundedSender<Interaction>,
}

impl ChannelInteractor {
    pub fn new(tx: mpsc::UnboundedSender<Interaction>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Interactor for ChannelInteractor {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalDecision {
        let (sender, receiver) = oneshot::channel();
        if self
            .tx
            .send(Interaction::Approval {
                request,
                reply: sender,
            })
            .is_err()
        {
            return ApprovalDecision::Deny("ui channel closed".to_string());
        }
        receiver
            .await
            .unwrap_or(ApprovalDecision::Deny("ui closed".to_string()))
    }

    async fn ask(&self, question: String, options: Vec<String>) -> Option<String> {
        let (sender, receiver) = oneshot::channel();
        if self
            .tx
            .send(Interaction::Ask {
                question,
                options,
                reply: sender,
            })
            .is_err()
        {
            return None;
        }
        receiver.await.unwrap_or(None)
    }
}

pub async fn run_agent(mut agent: Agent, mut commands: mpsc::UnboundedReceiver<Command>) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::UserInput(text) => {
                if let Err(err) = agent.run_turn(text).await {
                    agent.emit(Event::Error {
                        message: err.to_string(),
                    });
                }
            }
            Command::CycleMode => {
                agent.cycle_mode();
                let _ = agent.session.save();
            }
            Command::SetMode(value) => {
                if let Some(mode) = Mode::parse(&value) {
                    agent.set_mode(mode);
                    let _ = agent.session.save();
                }
            }
            Command::SetModel(value) => {
                agent.set_model(value);
                let _ = agent.session.save();
            }
            Command::SetGeneration(generation) => {
                agent.set_generation(generation);
                let _ = agent.session.save();
            }
            Command::SetContextWindow(window) => {
                agent.config.context_window = window.max(1024);
            }
            Command::Compact => {
                if let Err(err) = agent.compact_now().await {
                    agent.emit(Event::Error {
                        message: err.to_string(),
                    });
                }
            }
            Command::Clear => {
                agent.clear_history();
            }
            Command::ShowPlan => {
                let message = match agent.session.state.plan.clone() {
                    Some(plan) => format!("Current plan:\n{plan}"),
                    None => "No plan recorded yet. Use /mode plan to research first.".to_string(),
                };
                agent.emit(Event::Notice { message });
            }
            Command::ShowSkills => {
                let message = if agent.config.skills.is_empty() {
                    "No skills found. Add markdown files under .whisker/skills/ or ~/.whisker/skills/"
                        .to_string()
                } else {
                    let mut text = String::from("Available skills:\n");
                    for (name, description) in &agent.config.skills {
                        text.push_str(&format!("- /{name}: {description}\n"));
                    }
                    text
                };
                agent.emit(Event::Notice { message });
            }
            Command::Skill { name, args } => {
                if agent.config.skills.iter().any(|(skill, _)| skill == &name) {
                    let text = skill_invocation(&name, &args);
                    if let Err(err) = agent.run_turn(text).await {
                        agent.emit(Event::Error {
                            message: err.to_string(),
                        });
                    }
                } else {
                    agent.emit(Event::Notice {
                        message: format!("unknown command /{name}; try /help or /skills"),
                    });
                }
            }
            Command::Quit => break,
        }
    }
}

struct RawGuard;

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn out(text: &str) {
    let mut stdout = std::io::stdout();
    let text = text.replace('\n', "\r\n");
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn out_line(text: &str) {
    out(&format!("{text}\n"));
}

fn prompt() {
    out("\n\x1b[38;5;173m❯\x1b[0m ");
}

pub(crate) fn skill_command(command: &str, args: &str) -> Option<Command> {
    let name = command.strip_prefix('/')?;
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    Some(Command::Skill {
        name: name.to_string(),
        args: args.trim().to_string(),
    })
}

pub(crate) fn skill_invocation(name: &str, args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        format!(
            "Use the `{name}` skill. Load it with the skill tool and follow it. The tool result names the skill directory; use it as <skill-base-dir>."
        )
    } else {
        format!(
            "Use the `{name}` skill.\nRequest: {args}\nLoad it with the skill tool and follow it. The tool result names the skill directory; use it as <skill-base-dir>."
        )
    }
}

pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "…"
    }
}

pub(crate) fn spawn_key_reader() -> mpsc::UnboundedReceiver<KeyEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(event::Event::Key(key)) => {
                if tx.send(key).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    });
    rx
}

enum Incoming {
    Key(KeyEvent),
    Agent(Event),
    Interaction(Interaction),
    Tick,
}

pub async fn run_repl(
    cmd_tx: mpsc::UnboundedSender<Command>,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
    mut interaction_rx: mpsc::UnboundedReceiver<Interaction>,
    cancel: CancelHandle,
    queue: Arc<Mutex<VecDeque<String>>>,
    model: String,
    session_id: String,
    initial_mode: &str,
) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let _raw = RawGuard;

    out(&brand::terminal_title(&model));
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    for line in brand::banner(
        &model,
        &session_id,
        initial_mode,
        term_cols >= 64 && term_rows >= 20,
    ) {
        out_line(&line);
    }
    out_line("");

    let mut key_rx = spawn_key_reader();
    let mut input = String::new();
    let mut busy = false;
    let mut mode = initial_mode.to_string();
    let mut turn_text = false;
    let mut total_in: u32 = 0;
    let mut total_out: u32 = 0;
    let mut waiting = false;
    let mut spinner_frame = 0usize;
    prompt();

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
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
                if busy && waiting {
                    spinner_frame = spinner_frame.wrapping_add(1);
                    out(&format!("\r\x1b[2K  {} ", brand::spinner(spinner_frame)));
                }
            }
            Incoming::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl && matches!(key.code, KeyCode::Char('c')) {
                    if busy {
                        cancel.cancel();
                        out_line("\n\x1b[33minterrupting…\x1b[0m");
                    } else if !input.is_empty() {
                        input.clear();
                        out_line("\n\x1b[2mcleared\x1b[0m");
                        prompt();
                    } else {
                        out_line("");
                        let _ = cmd_tx.send(Command::Quit);
                        break;
                    }
                    continue;
                }
                if ctrl && matches!(key.code, KeyCode::Char('d')) {
                    out_line("");
                    let _ = cmd_tx.send(Command::Quit);
                    break;
                }
                if ctrl && matches!(key.code, KeyCode::Char('l')) {
                    out("\x1b[2J\x1b[H");
                    prompt();
                    continue;
                }

                match key.code {
                    KeyCode::Esc => {
                        if busy {
                            cancel.cancel();
                            out_line("\n\x1b[33minterrupting…\x1b[0m");
                        } else {
                            input.clear();
                            out_line("");
                            prompt();
                        }
                    }
                    KeyCode::BackTab => {
                        if !busy {
                            let _ = cmd_tx.send(Command::CycleMode);
                        }
                    }
                    KeyCode::Enter => {
                        if input.trim().is_empty() {
                            continue;
                        }
                        let text = input.trim().to_string();
                        input.clear();
                        out_line("");
                        if text.starts_with('/') {
                            if busy {
                                out_line("\x1b[2m[busy — command ignored]\x1b[0m");
                            } else {
                                handle_slash(&text, &cmd_tx, total_in, total_out);
                            }
                            prompt();
                            continue;
                        }
                        if busy {
                            if let Ok(mut pending) = queue.lock() {
                                pending.push_back(text);
                            }
                            out_line("\x1b[2m[queued — delivered after the current step]\x1b[0m");
                            prompt();
                        } else if cmd_tx.send(Command::UserInput(text)).is_err() {
                            break;
                        } else {
                            busy = true;
                            waiting = true;
                            spinner_frame = 0;
                            out(&format!("\r\x1b[2K  {} ", brand::spinner(0)));
                        }
                    }
                    KeyCode::Backspace => {
                        if input.pop().is_some() {
                            out("\x08 \x08");
                        }
                    }
                    KeyCode::Char(ch) => {
                        if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) {
                            input.push(ch);
                            out(&ch.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Incoming::Agent(event) => match event {
                Event::AssistantDelta { text } => {
                    if waiting {
                        out("\r\x1b[2K");
                        waiting = false;
                    }
                    turn_text = true;
                    out(&text)
                }
                Event::AssistantReasoning { text } => {
                    if std::env::var("WHISKER_SHOW_REASONING").is_ok() {
                        out(&format!("\x1b[2m{text}\x1b[0m"));
                    }
                }
                Event::ToolStart { name, args, .. } => {
                    if waiting {
                        out("\r\x1b[2K");
                        waiting = false;
                    }
                    out(&format!(
                        "\n\x1b[38;5;173m⏺ {}\x1b[0m\n",
                        describe_tool(&name, &args)
                    ));
                }
                Event::ToolResult {
                    content, is_error, ..
                } => render_result(&content, is_error),
                Event::ToolDenied { reason, .. } => {
                    out_line(&format!("  \x1b[31m⎿  denied: {reason}\x1b[0m"));
                }
                Event::ToolProgress { name, text, .. } => {
                    if name == "agent" || std::env::var("WHISKER_SHOW_TOOL_PROGRESS").is_ok() {
                        out_line(&format!("  \x1b[2m· {text}\x1b[0m"));
                    }
                }
                Event::Reminder { kind, .. } => {
                    out_line(&format!("\x1b[2m[harness] {kind}\x1b[0m"));
                }
                Event::TodoUpdated { todos } => render_todos(&todos),
                Event::ModeChanged { mode: new_mode } => {
                    mode = new_mode;
                    out_line(&format!("\x1b[2mmode → {mode}\x1b[0m"));
                }
                Event::TurnFinished {
                    prompt_tokens,
                    completion_tokens,
                    terminal_reason,
                    ..
                } => {
                    if waiting {
                        out("\r\x1b[2K");
                        waiting = false;
                    }
                    busy = false;
                    total_in = total_in.saturating_add(prompt_tokens);
                    total_out = total_out.saturating_add(completion_tokens);
                    out_line(&format!(
                        "\x1b[2m— tokens in {prompt_tokens} / out {completion_tokens}\x1b[0m"
                    ));
                    if terminal_reason != TerminalReason::Complete {
                        out_line(&format!(
                            "\x1b[2mterminal: {}\x1b[0m",
                            terminal_reason.as_str()
                        ));
                    }
                    if mode == "plan" && turn_text {
                        turn_text = false;
                        if prompt_plan_approval(&mut key_rx).await {
                            let _ = cmd_tx.send(Command::SetMode("normal".to_string()));
                        }
                    }
                    prompt();
                }
                Event::Compacted { removed } => {
                    out_line(&format!(
                        "\x1b[2m[context compacted: {removed} messages summarized]\x1b[0m"
                    ));
                }
                Event::Notice { message } => {
                    out_line(&format!("\x1b[2m{message}\x1b[0m"));
                }
                Event::HistoryCleared => {
                    out_line("\x1b[2m[history cleared]\x1b[0m");
                }
                Event::Interrupted => out_line("\x1b[33minterrupted\x1b[0m"),
                Event::Error { message } => {
                    out_line(&format!("\x1b[31merror: {message}\x1b[0m"));
                }
                Event::UserMessage { .. } => {
                    turn_text = false;
                }
                Event::SessionStarted { .. }
                | Event::AssistantMessage { .. }
                | Event::TurnStarted { .. } => {}
            },
            Incoming::Interaction(interaction) => match interaction {
                Interaction::Approval { request, reply } => {
                    let decision = prompt_approval(&mut key_rx, &request).await;
                    let _ = reply.send(decision);
                }
                Interaction::Ask {
                    question,
                    options,
                    reply,
                } => {
                    out_line(&format!("\n\x1b[36m?\x1b[0m {question}"));
                    for (index, option) in options.iter().enumerate() {
                        out_line(&format!("  \x1b[2m{}. {}\x1b[0m", index + 1, option));
                    }
                    out("\x1b[2m> \x1b[0m");
                    let answer = read_line(&mut key_rx).await.unwrap_or_default();
                    let answer = answer.trim().to_string();
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
                    prompt();
                }
            },
        }
    }

    out("\x1b]0;whisker\x07");
    out("\n");
    Ok(())
}

pub(crate) fn describe_tool(name: &str, args: &serde_json::Value) -> String {
    let arg = |key: &str| args.get(key).and_then(|value| value.as_str()).unwrap_or("");
    match name {
        "bash" => format!("Bash({})", truncate(arg("command"), 120)),
        "read" => format!("Read({})", arg("path")),
        "write" => format!("Write({})", arg("path")),
        "edit" => format!("Edit({})", arg("path")),
        "grep" => format!("Grep({})", truncate(arg("pattern"), 80)),
        "glob" => format!("Glob({})", truncate(arg("pattern"), 80)),
        "todo_write" => "Update todos".to_string(),
        "ask_user" => format!("Ask user({})", truncate(arg("question"), 100)),
        _ => name.to_string(),
    }
}

fn render_result(content: &str, is_error: bool) {
    let color = if is_error { "\x1b[31m" } else { "\x1b[2m" };
    let lines: Vec<&str> = content.lines().collect();
    let shown = 4.min(lines.len());
    let mut text = String::new();
    for (index, line) in lines.iter().take(shown).enumerate() {
        if index == 0 {
            text.push_str(&format!("  {color}⎿  {}\x1b[0m\n", truncate(line, 160)));
        } else {
            text.push_str(&format!("     {color}{}\x1b[0m\n", truncate(line, 160)));
        }
    }
    if lines.len() > shown {
        text.push_str(&format!(
            "     \x1b[2m… {} more lines\x1b[0m\n",
            lines.len() - shown
        ));
    }
    out(&text);
}

fn render_todos(todos: &[TodoItem]) {
    if todos.is_empty() {
        return;
    }
    let mut text = String::from("\x1b[2m");
    for todo in todos {
        let marker = match todo.status {
            TodoStatus::Completed => "☑",
            TodoStatus::InProgress => "▶",
            TodoStatus::Pending => "☐",
        };
        text.push_str(&format!("  {marker} {}\n", todo.content));
    }
    text.push_str("\x1b[0m");
    out(&text);
}

fn handle_slash(
    text: &str,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    total_in: u32,
    total_out: u32,
) {
    let mut parts = text.split_whitespace();
    let command = parts.next().unwrap_or_default();
    match command {
        "/help" => {
            out_line("\x1b[2m/help        show commands");
            out_line("/mode <m>    normal | plan | auto");
            out_line("/plan        show the recorded plan");
            out_line("/skills      list available skills");
            out_line("/<skill>     run a skill, e.g. /impeccable polish");
            out_line("/cat <pose>  idle | scanning | working | success");
            out_line("/compact     summarize history now");
            out_line("/clear       clear conversation history");
            out_line("/cost        show token totals\x1b[0m");
        }
        "/mode" => match parts.next() {
            Some(value) if Mode::parse(value).is_some() => {
                let _ = cmd_tx.send(Command::SetMode(value.to_string()));
            }
            _ => out_line("usage: /mode normal|plan|auto"),
        },
        "/plan" => {
            let _ = cmd_tx.send(Command::ShowPlan);
        }
        "/skills" => {
            let _ = cmd_tx.send(Command::ShowSkills);
        }
        "/cat" => {
            let pose = parts
                .next()
                .and_then(brand::Pose::parse)
                .unwrap_or(brand::Pose::Idle);
            for line in brand::art_lines(pose, true) {
                out_line(&format!("  {line}"));
            }
        }
        "/compact" => {
            let _ = cmd_tx.send(Command::Compact);
        }
        "/clear" => {
            let _ = cmd_tx.send(Command::Clear);
        }
        "/cost" => {
            out_line(&format!(
                "\x1b[2mtokens in {total_in} / out {total_out}\x1b[0m"
            ));
        }
        other => {
            let args = parts.collect::<Vec<_>>().join(" ");
            if let Some(command) = skill_command(other, &args) {
                let _ = cmd_tx.send(command);
            } else {
                out_line(&format!("unknown command {other}; try /help"));
            }
        }
    }
}

async fn prompt_plan_approval(key_rx: &mut mpsc::UnboundedReceiver<KeyEvent>) -> bool {
    out_line(
        "\n\x1b[33mplan ready\x1b[0m  \x1b[2m[y] approve and execute · [n] keep planning\x1b[0m",
    );
    loop {
        let Some(key) = key_rx.recv().await else {
            return false;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            out_line("keeping plan mode");
            return false;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                out_line("approved");
                return true;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                out_line("keeping plan mode");
                return false;
            }
            _ => {}
        }
    }
}

async fn prompt_approval(
    key_rx: &mut mpsc::UnboundedReceiver<KeyEvent>,
    request: &ApprovalRequest,
) -> ApprovalDecision {
    out_line("");
    out_line(&format!(
        "\x1b[33m⚑ approval required\x1b[0m \x1b[2m{}\x1b[0m",
        request.tool
    ));
    let lines: Vec<&str> = request.preview.lines().collect();
    for line in lines.iter().take(30) {
        out_line(&format!("  \x1b[2m{}\x1b[0m", truncate(line, 200)));
    }
    if lines.len() > 30 {
        out_line(&format!(
            "  \x1b[2m… {} more lines\x1b[0m",
            lines.len() - 30
        ));
    }
    out_line(&format!("  \x1b[2mrule: {}\x1b[0m", request.rule));
    out("\n\x1b[2m[y] once   [a] always   [n] deny   [Esc] cancel\x1b[0m  > ");

    loop {
        let key = match key_rx.recv().await {
            Some(key) => key,
            None => return ApprovalDecision::Deny("ui closed".to_string()),
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                out_line("once");
                return ApprovalDecision::Once;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                out_line("always");
                return ApprovalDecision::Always;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                out("\n\x1b[2mfeedback (optional): \x1b[0m");
                let feedback = read_line(key_rx).await.unwrap_or_default();
                return ApprovalDecision::Deny(feedback);
            }
            KeyCode::Esc => {
                out_line("cancel");
                return ApprovalDecision::Deny("cancelled by user".to_string());
            }
            _ => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'))
                {
                    out_line("cancel");
                    return ApprovalDecision::Deny("cancelled by user".to_string());
                }
            }
        }
    }
}

async fn read_line(key_rx: &mut mpsc::UnboundedReceiver<KeyEvent>) -> Option<String> {
    let mut buffer = String::new();
    loop {
        let key = key_rx.recv().await?;
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return None;
        }
        match key.code {
            KeyCode::Enter => {
                out_line("");
                return Some(buffer);
            }
            KeyCode::Esc => return None,
            KeyCode::Backspace => {
                if buffer.pop().is_some() {
                    out("\x08 \x08");
                }
            }
            KeyCode::Char(ch) => {
                buffer.push(ch);
                out(&ch.to_string());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_skill_becomes_an_invocation() {
        match skill_command("/impeccable", "polish the header") {
            Some(Command::Skill { name, args }) => {
                assert_eq!(name, "impeccable");
                assert_eq!(args, "polish the header");
            }
            other => panic!("expected skill command, got {other:?}"),
        }
        assert!(skill_command("/bad name", "").is_none());
        let prompt = skill_invocation("impeccable", "audit");
        assert!(prompt.contains("impeccable"));
        assert!(prompt.contains("Request: audit"));
        assert!(prompt.contains("<skill-base-dir>"));
    }
}
