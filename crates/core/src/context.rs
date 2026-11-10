use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const REMINDER_MIN_TOOL_CALLS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Normal,
    Plan,
    AutoAccept,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Plan => "plan",
            Mode::AutoAccept => "auto",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "normal" => Some(Mode::Normal),
            "plan" => Some(Mode::Plan),
            "auto" | "autoaccept" | "accept-edits" => Some(Mode::AutoAccept),
            _ => None,
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Mode::Normal => Mode::Plan,
            Mode::Plan => Mode::AutoAccept,
            Mode::AutoAccept => Mode::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" | "inprogress" => Some(TodoStatus::InProgress),
            "completed" | "done" => Some(TodoStatus::Completed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub mode: Mode,
    pub todos: Vec<TodoItem>,
    pub edits_since_verify: u32,
    pub tool_calls_since_todo_update: u32,
    pub tools_since_user: u32,
    #[serde(default)]
    pub reminders_sent: HashSet<String>,
}

impl Default for TurnContext {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            todos: Vec::new(),
            edits_since_verify: 0,
            tool_calls_since_todo_update: 0,
            tools_since_user: 0,
            reminders_sent: HashSet::new(),
        }
    }
}

impl TurnContext {
    pub fn begin_turn(&mut self) {
        self.tools_since_user = 0;
        self.reminders_sent.clear();
    }

    pub fn on_tool_call(&mut self) {
        self.tools_since_user += 1;
        self.tool_calls_since_todo_update += 1;
    }

    pub fn on_tool_result(&mut self, tool: &str, is_error: bool, command: Option<&str>) {
        if !is_error && (tool == "edit" || tool == "write") {
            self.edits_since_verify += 1;
            self.reminders_sent.remove("verify");
        }
        if tool == "bash" {
            if let Some(command) = command {
                if is_verification_command(command) {
                    self.edits_since_verify = 0;
                    self.reminders_sent.remove("verify");
                }
            }
        }
    }

    pub fn on_todo_update(&mut self, todos: Vec<TodoItem>) {
        self.todos = todos;
        self.tool_calls_since_todo_update = 0;
        self.reminders_sent.remove("todo_create");
        self.reminders_sent.remove("todo_single");
    }

    pub fn in_progress_count(&self) -> usize {
        self.todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::InProgress)
            .count()
    }

    pub fn due_reminders(&mut self) -> Vec<(String, String)> {
        let mut out = Vec::new();

        if self.todos.is_empty()
            && self.tools_since_user >= REMINDER_MIN_TOOL_CALLS
            && self.reminders_sent.insert("todo_create".to_string())
        {
            out.push((
                "todo_create".to_string(),
                "This looks like a multi-step task and no todo list exists yet. Use the todo_write tool to plan the work and keep exactly one item in_progress while you execute.".to_string(),
            ));
        }

        if !self.todos.is_empty()
            && self.in_progress_count() != 1
            && self.reminders_sent.insert("todo_single".to_string())
        {
            out.push((
                "todo_single".to_string(),
                "The todo list must have exactly one item in_progress at all times. Update it with todo_write now.".to_string(),
            ));
        }

        if self.edits_since_verify > 0 && self.reminders_sent.insert("verify".to_string()) {
            out.push((
                "verify".to_string(),
                "You changed files but have not run a verification command (tests, build, typecheck) since then. Verify the changes before reporting completion.".to_string(),
            ));
        }

        out
    }
}

pub fn is_verification_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "cargo test",
        "cargo build",
        "cargo check",
        "cargo clippy",
        "npm test",
        "npm run test",
        "npm run build",
        "npm run lint",
        "pnpm test",
        "pnpm build",
        "pnpm lint",
        "yarn test",
        "yarn build",
        "pytest",
        "go test",
        "go build",
        "make test",
        "make build",
        "tsc",
        "typecheck",
        "flutter test",
        "flutter analyze",
        "dotnet test",
        "dotnet build",
        "mvn test",
        "gradle test",
        "./gradlew test",
        "ruff check",
        "mypy",
        "eslint",
    ];
    MARKERS.iter().any(|marker| lower.contains(marker))
}
