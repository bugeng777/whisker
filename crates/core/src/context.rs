use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub struct ReminderPolicy {
    pub todo_after_turns: u32,
    pub todo_interval_turns: u32,
}

impl Default for ReminderPolicy {
    fn default() -> Self {
        Self {
            todo_after_turns: 10,
            todo_interval_turns: 10,
        }
    }
}

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
    #[serde(default)]
    pub turns_since_todo_write: u32,
    #[serde(default)]
    pub turns_since_todo_reminder: u32,
    #[serde(default)]
    pub todo_reminders_sent: u32,
    #[serde(default)]
    pub reminders_sent: HashSet<String>,
}

impl Default for TurnContext {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            todos: Vec::new(),
            edits_since_verify: 0,
            turns_since_todo_write: 0,
            turns_since_todo_reminder: 0,
            todo_reminders_sent: 0,
            reminders_sent: HashSet::new(),
        }
    }
}

impl TurnContext {
    pub fn begin_turn(&mut self) {
        self.reminders_sent.clear();
    }

    pub fn on_assistant_turn(&mut self) {
        self.turns_since_todo_write = self.turns_since_todo_write.saturating_add(1);
        self.turns_since_todo_reminder = self.turns_since_todo_reminder.saturating_add(1);
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
        self.turns_since_todo_write = 0;
        self.turns_since_todo_reminder = 0;
        self.todo_reminders_sent = 0;
        self.reminders_sent.remove("todo_single");
    }

    pub fn in_progress_count(&self) -> usize {
        self.todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::InProgress)
            .count()
    }

    pub fn due_reminders(
        &mut self,
        todo_tool_available: bool,
        policy: ReminderPolicy,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();

        let after = policy.todo_after_turns.max(1);
        let interval = policy.todo_interval_turns.max(1);
        if todo_tool_available
            && self.turns_since_todo_write >= after
            && (self.todo_reminders_sent == 0 || self.turns_since_todo_reminder >= interval)
        {
            self.turns_since_todo_reminder = 0;
            self.todo_reminders_sent = self.todo_reminders_sent.saturating_add(1);
            let text = if self.todos.is_empty() {
                "The todo tool has not been used yet for this task. Use the todo_write tool to plan the work and keep exactly one item in_progress while you execute."
            } else {
                "The todo list has not been updated recently. Use the todo_write tool to update statuses and keep exactly one item in_progress."
            };
            out.push(("todo_reminder".to_string(), text.to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn one_in_progress() -> Vec<TodoItem> {
        vec![TodoItem {
            id: "1".to_string(),
            content: "work".to_string(),
            status: TodoStatus::InProgress,
        }]
    }

    fn todo_reminders(due: &[(String, String)]) -> usize {
        due.iter()
            .filter(|(kind, _)| kind == "todo_reminder")
            .count()
    }

    #[test]
    fn todo_reminder_waits_for_ten_agentic_turns() {
        let mut ctx = TurnContext::default();
        ctx.on_todo_update(one_in_progress());
        let policy = ReminderPolicy::default();

        for _ in 0..9 {
            ctx.on_assistant_turn();
            assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 0);
        }
        ctx.on_assistant_turn();
        let due = ctx.due_reminders(true, policy);
        assert_eq!(todo_reminders(&due), 1);
        assert!(due[0].1.contains("not been updated"));

        for _ in 0..9 {
            ctx.on_assistant_turn();
            assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 0);
        }
        ctx.on_assistant_turn();
        assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 1);
    }

    #[test]
    fn todo_reminder_requires_the_todo_tool() {
        let mut ctx = TurnContext::default();
        let policy = ReminderPolicy::default();
        for _ in 0..10 {
            ctx.on_assistant_turn();
        }
        assert_eq!(todo_reminders(&ctx.due_reminders(false, policy)), 0);
        let due = ctx.due_reminders(true, policy);
        assert_eq!(todo_reminders(&due), 1);
        assert!(due[0].1.contains("has not been used yet"));
    }

    #[test]
    fn todo_write_resets_reminder_counters() {
        let mut ctx = TurnContext::default();
        let policy = ReminderPolicy::default();
        for _ in 0..10 {
            ctx.on_assistant_turn();
        }
        ctx.on_todo_update(one_in_progress());
        for _ in 0..9 {
            ctx.on_assistant_turn();
            assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 0);
        }
        ctx.on_assistant_turn();
        assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 1);
    }

    #[test]
    fn todo_reminder_thresholds_are_configurable() {
        let mut ctx = TurnContext::default();
        let policy = ReminderPolicy {
            todo_after_turns: 2,
            todo_interval_turns: 3,
        };
        ctx.on_assistant_turn();
        assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 0);
        ctx.on_assistant_turn();
        assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 1);

        ctx.on_assistant_turn();
        ctx.on_assistant_turn();
        assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 0);
        ctx.on_assistant_turn();
        assert_eq!(todo_reminders(&ctx.due_reminders(true, policy)), 1);
    }
}
