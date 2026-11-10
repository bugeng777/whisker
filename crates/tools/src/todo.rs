use agent_core::tool::ToolOutput;
use agent_core::{ProgressSink, Tool, ToolCtx};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        "Replace the task todo list. Use for multi-step work. Exactly one item must be in_progress while working; mark items completed as you finish them."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn is_read_only(&self, _args: &Value) -> bool {
        true
    }

    async fn validate(&self, args: &Value, _ctx: &ToolCtx) -> Result<(), String> {
        let Some(items) = args.get("todos").and_then(|value| value.as_array()) else {
            return Err("todos must be an array".to_string());
        };
        if items.len() > 20 {
            return Err("todo list is limited to 20 items".to_string());
        }
        let mut in_progress = 0;
        for item in items {
            let content = item
                .get("content")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if content.trim().is_empty() {
                return Err("every todo needs non-empty content".to_string());
            }
            let status = item
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("pending");
            if !matches!(status, "pending" | "in_progress" | "completed") {
                return Err(format!("invalid todo status: {status}"));
            }
            if status == "in_progress" {
                in_progress += 1;
            }
        }
        if in_progress > 1 {
            return Err(
                "exactly one todo may be in_progress at a time; update the list".to_string(),
            );
        }
        Ok(())
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolCtx,
        _progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let items = args
            .get("todos")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            return Ok(ToolOutput::ok("Todo list cleared.".to_string()));
        }
        let mut lines = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let content = item
                .get("content")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let status = item
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("pending");
            let marker = match status {
                "completed" => "[x]",
                "in_progress" => "[>]",
                _ => "[ ]",
            };
            lines.push(format!("{}. {} {}", index + 1, marker, content));
        }
        Ok(ToolOutput::ok(format!(
            "Todo list updated:\n{}",
            lines.join("\n")
        )))
    }
}
