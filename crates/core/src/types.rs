use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::TodoItem;

fn default_mime() -> String {
    "image/png".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRef {
    pub path: PathBuf,
    #[serde(default = "default_mime")]
    pub mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageRef>,
    },
    Assistant {
        #[serde(default)]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
    },
    Tool {
        tool_call_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
            images: Vec::new(),
        }
    }

    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageRef>) -> Self {
        Self::User {
            content: content.into(),
            images,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    Other(String),
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    Reasoning(String),
    ToolCall(ToolCall),
    Finish(FinishReason),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SessionStarted {
        session_id: String,
        model: String,
    },
    UserMessage {
        content: String,
    },
    AssistantDelta {
        text: String,
    },
    AssistantReasoning {
        text: String,
    },
    AssistantMessage {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    ToolStart {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolProgress {
        id: String,
        #[serde(default)]
        name: String,
        text: String,
    },
    ToolResult {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    ToolDenied {
        id: String,
        name: String,
        reason: String,
    },
    ModeChanged {
        mode: String,
    },
    TodoUpdated {
        todos: Vec<TodoItem>,
    },
    TurnStarted {
        turn: u32,
    },
    TurnFinished {
        turn: u32,
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Compacted {
        removed: usize,
    },
    Reminder {
        kind: String,
        text: String,
    },
    Notice {
        message: String,
    },
    HistoryCleared,
    Interrupted,
    Error {
        message: String,
    },
}
