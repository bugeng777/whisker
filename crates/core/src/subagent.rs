use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::llm::LlmClient;
use crate::policy::{ApprovalDecision, ApprovalRequest, Interactor};
use crate::tool::{ProgressSink, Tool, ToolCtx, ToolOutput, ToolRegistry};
use crate::types::{ChatRequest, GenerationSettings, Message, StreamEvent, ToolCall, Usage};

const SUBAGENT_TOOL_TIMEOUT: Duration = Duration::from_secs(300);
const SUBAGENT_RESULT_LIMIT: usize = 8_000;

struct ReadOnlyInteractor;

#[async_trait]
impl Interactor for ReadOnlyInteractor {
    async fn approve(&self, _request: ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Deny("subagents may only use read-only tools".to_string())
    }

    async fn ask(&self, _question: String, _options: Vec<String>) -> Option<String> {
        None
    }
}

pub struct SubagentTool {
    pub llm: Arc<dyn LlmClient>,
    pub tools: Arc<ToolRegistry>,
    pub max_turns: u32,
    pub slots: Semaphore,
    pub system_prompt: String,
    pub generation: GenerationSettings,
}

impl SubagentTool {
    async fn stream_completion(
        &self,
        request: ChatRequest,
        token: &CancellationToken,
    ) -> Result<(Option<String>, Vec<ToolCall>, Usage)> {
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let llm = self.llm.clone();
        let worker_token = token.clone();
        let handle = tokio::spawn(async move { llm.chat_stream(request, tx, worker_token).await });

        let mut content = String::new();
        let mut calls = Vec::new();
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    handle.abort();
                    return Err(anyhow!("cancelled"));
                }
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(StreamEvent::Delta(text)) => content.push_str(&text),
                        Some(StreamEvent::Reasoning(_)) | Some(StreamEvent::Finish(_)) => {}
                        Some(StreamEvent::ToolCall(call)) => calls.push(call),
                    }
                }
            }
        }
        let usage = handle.await??;
        let content = if content.is_empty() {
            None
        } else {
            Some(content)
        };
        Ok((content, calls, usage))
    }

    async fn run_tool_call(&self, name: &str, raw_args: &str, ctx: &ToolCtx) -> String {
        if name == "agent" {
            return "Nested subagents are not allowed (max depth 1).".to_string();
        }
        let args: Value = if raw_args.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(raw_args) {
                Ok(value) => value,
                Err(err) => return format!("Invalid JSON arguments: {err}"),
            }
        };

        let Some(tool) = self.tools.get(name) else {
            return format!(
                "Unknown tool '{name}'. Available tools: {}",
                self.tools.names().join(", ")
            );
        };

        if !tool.is_read_only(&args) || tool.is_destructive(&args) {
            return format!("Tool '{name}' is not available to subagents (read-only tools only).");
        }

        let sub_ctx = ToolCtx {
            cwd: ctx.cwd.clone(),
            read_ledger: Arc::new(Mutex::new(HashMap::new())),
            cancel: ctx.cancel.clone(),
            spill_dir: ctx.spill_dir.clone(),
            interactor: Arc::new(ReadOnlyInteractor),
        };

        match tokio::time::timeout(
            SUBAGENT_TOOL_TIMEOUT,
            tool.execute(args, sub_ctx, ProgressSink::default()),
        )
        .await
        {
            Ok(Ok(output)) => {
                let mut text = output.content;
                if output.is_error {
                    text = format!("Tool error: {text}");
                }
                text.chars().take(SUBAGENT_RESULT_LIMIT).collect()
            }
            Ok(Err(err)) => format!("Tool failed: {err}"),
            Err(_) => "Tool timed out.".to_string(),
        }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Delegate a read-only research or exploration task to a subagent with an isolated context. Use it for broad codebase searches that would flood the main context; do not use it for tasks you can finish in a few reads. Returns a concise report with file references. The subagent cannot modify files or ask the user."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "What to investigate and what the report must contain"
                },
                "context": {
                    "type": "string",
                    "description": "Optional background the subagent needs to start"
                }
            },
            "required": ["task"]
        })
    }

    fn is_read_only(&self, _args: &Value) -> bool {
        true
    }

    fn concurrency_safe(&self, _args: &Value) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolCtx,
        progress: ProgressSink,
    ) -> Result<ToolOutput> {
        let task = args
            .get("task")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("missing task"))?
            .trim()
            .to_string();
        if task.is_empty() {
            return Ok(ToolOutput::err("task must not be empty"));
        }
        let extra = args
            .get("context")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let prompt = if extra.is_empty() {
            task
        } else {
            format!("{task}\n\nAdditional context:\n{extra}")
        };

        let _permit = tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(ToolOutput::err("Subagent cancelled.")),
            permit = self.slots.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => return Ok(ToolOutput::err("Subagent unavailable.")),
            },
        };

        let mut messages = vec![
            Message::system(self.system_prompt.clone()),
            Message::user(prompt),
        ];
        let mut usage = Usage::default();
        let mut final_text = String::new();

        for turn in 1..=self.max_turns.max(1) {
            if ctx.cancel.is_cancelled() {
                return Ok(ToolOutput::err("Subagent cancelled."));
            }
            progress.emit(format!("subagent turn {turn}"));

            let request = ChatRequest {
                messages: messages.clone(),
                tools: self.tools.specs(),
                generation: GenerationSettings {
                    temperature: Some(0.1),
                    max_tokens: None,
                    ..self.generation.clone()
                },
            };
            let (content, calls, turn_usage) =
                match self.stream_completion(request, &ctx.cancel).await {
                    Ok(value) => value,
                    Err(err) if err.to_string() == "cancelled" => {
                        return Ok(ToolOutput::err("Subagent cancelled."));
                    }
                    Err(err) => return Ok(ToolOutput::err(format!("Subagent failed: {err}"))),
                };

            usage.prompt_tokens = usage.prompt_tokens.saturating_add(turn_usage.prompt_tokens);
            usage.completion_tokens = usage
                .completion_tokens
                .saturating_add(turn_usage.completion_tokens);
            usage.total_tokens = usage.total_tokens.saturating_add(turn_usage.total_tokens);

            let text = content.unwrap_or_default();
            if !text.trim().is_empty() {
                final_text = text.clone();
            }
            messages.push(Message::Assistant {
                content: if text.is_empty() { None } else { Some(text) },
                tool_calls: if calls.is_empty() {
                    None
                } else {
                    Some(calls.clone())
                },
                reasoning_content: None,
            });

            if calls.is_empty() {
                break;
            }

            for call in calls {
                let result = self
                    .run_tool_call(&call.function.name, &call.function.arguments, &ctx)
                    .await;
                messages.push(Message::tool(
                    call.id.clone(),
                    call.function.name.clone(),
                    result,
                ));
            }
        }

        progress.emit(format!(
            "finished: in {} / out {} tokens",
            usage.prompt_tokens, usage.completion_tokens
        ));
        let report = if final_text.trim().is_empty() {
            "Subagent finished without producing a report.".to_string()
        } else {
            final_text
        };
        Ok(ToolOutput::ok(report))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct ProbeLlm {
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay: Duration,
    }

    #[async_trait]
    impl LlmClient for ProbeLlm {
        fn model(&self) -> String {
            "probe".to_string()
        }

        fn set_model(&self, _model: &str) -> bool {
            false
        }

        fn context_window(&self) -> usize {
            1_000
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
            sink: mpsc::UnboundedSender<StreamEvent>,
            _cancel: CancellationToken,
        ) -> Result<Usage> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            let _ = sink.send(StreamEvent::Delta("done".to_string()));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(Usage::default())
        }
    }

    fn test_tool(slots: usize, delay: Duration) -> (SubagentTool, Arc<ProbeLlm>) {
        let llm = Arc::new(ProbeLlm {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            delay,
        });
        let tool = SubagentTool {
            llm: llm.clone(),
            tools: Arc::new(ToolRegistry::new(Vec::new())),
            max_turns: 1,
            slots: Semaphore::new(slots),
            system_prompt: "test".to_string(),
            generation: GenerationSettings::default(),
        };
        (tool, llm)
    }

    fn test_ctx() -> ToolCtx {
        ToolCtx {
            cwd: PathBuf::from("."),
            read_ledger: Arc::new(Mutex::new(HashMap::new())),
            cancel: CancellationToken::new(),
            spill_dir: PathBuf::from("."),
            interactor: Arc::new(ReadOnlyInteractor),
        }
    }

    #[tokio::test]
    async fn concurrency_limit_serializes_subagents() {
        let (tool, llm) = test_tool(1, Duration::from_millis(60));
        let args = json!({"task": "probe"});
        let (first, second) = tokio::join!(
            tool.execute(args.clone(), test_ctx(), ProgressSink::default()),
            tool.execute(args, test_ctx(), ProgressSink::default()),
        );
        assert!(first.expect("first").content.contains("done"));
        assert!(second.expect("second").content.contains("done"));
        assert_eq!(llm.max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrency_limit_allows_configured_parallelism() {
        let (tool, llm) = test_tool(2, Duration::from_millis(60));
        let args = json!({"task": "probe"});
        let (first, second) = tokio::join!(
            tool.execute(args.clone(), test_ctx(), ProgressSink::default()),
            tool.execute(args, test_ctx(), ProgressSink::default()),
        );
        assert!(first.expect("first").content.contains("done"));
        assert!(second.expect("second").content.contains("done"));
        assert_eq!(llm.max_active.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn nested_subagents_are_refused() {
        let (tool, _) = test_tool(1, Duration::ZERO);
        let output = tool.run_tool_call("agent", "{}", &test_ctx()).await;
        assert!(output.contains("max depth 1"));
    }
}
