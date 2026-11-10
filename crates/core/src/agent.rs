use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::cancel::CancelHandle;
use crate::context::{Mode, TodoItem, TodoStatus};
use crate::llm::LlmClient;
use crate::policy::{rule_key, ApprovalDecision, ApprovalRequest, Decision, Interactor, Policy};
use crate::session::SessionStore;
use crate::tool::{ProgressSink, ReadLedger, Tool, ToolCtx, ToolOutput, ToolRegistry};
use crate::types::{
    ChatRequest, Event, FinishReason, GenerationSettings, Message, StreamEvent, ToolCall, Usage,
};

pub const MAX_TOOL_RESULT_CHARS: usize = 32 * 1024;
const PREVIEW_CHARS: usize = 16 * 1024;
const COMPACT_RATIO: f64 = 0.7;
const TOOL_TIMEOUT: Duration = Duration::from_secs(600);

pub struct AgentConfig {
    pub model: String,
    pub cwd: PathBuf,
    pub vision: bool,
    pub context_window: usize,
    /// Optional safety cap. `None` waits for the model's finish signal.
    pub max_turns: Option<u32>,
    pub system_prompt: String,
    pub generation: GenerationSettings,
    pub skills: Vec<(String, String)>,
    pub pre_tool_use: Vec<String>,
    pub post_tool_use: Vec<String>,
    pub permissions_file: Option<PathBuf>,
}

pub struct Agent {
    pub llm: Arc<dyn LlmClient>,
    pub tools: Arc<ToolRegistry>,
    pub policy: Arc<Mutex<Policy>>,
    pub interactor: Arc<dyn Interactor>,
    pub events: mpsc::UnboundedSender<Event>,
    pub session: SessionStore,
    pub config: AgentConfig,
    pub cancel: CancelHandle,
    pub queue: Arc<Mutex<VecDeque<String>>>,
    pub read_ledger: ReadLedger,
}

struct PreparedCall {
    tool: Arc<dyn Tool>,
    args: serde_json::Value,
    sink: ProgressSink,
}

impl Agent {
    pub fn emit(&self, event: Event) {
        if !matches!(
            event,
            Event::AssistantDelta { .. }
                | Event::AssistantReasoning { .. }
                | Event::ToolProgress { .. }
        ) {
            let _ = self.session.append_event(&event);
        }
        let _ = self.events.send(event);
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.policy.lock().expect("policy lock").set_mode(mode);
        self.session.state.ctx.mode = mode;
        self.emit(Event::ModeChanged {
            mode: mode.as_str().to_string(),
        });
    }

    pub fn cycle_mode(&mut self) -> Mode {
        let next = self.policy.lock().expect("policy lock").mode.next();
        self.set_mode(next);
        next
    }

    pub fn set_model(&mut self, model: String) {
        let vision = self.llm.set_model(&model);
        self.config.model = model.clone();
        self.config.vision = vision;
        self.session.state.model = model.clone();
        self.emit(Event::Notice {
            message: format!("model → {model}"),
        });
    }

    pub fn set_generation(&mut self, generation: GenerationSettings) {
        self.config.generation = generation.clone();
        let thinking = match generation.thinking {
            Some(true) => "on",
            Some(false) => "off",
            None => "auto",
        };
        self.emit(Event::Notice {
            message: format!(
                "params → thinking {thinking} · effort {} · max {} · temp {}",
                generation.reasoning_effort.as_deref().unwrap_or("auto"),
                generation
                    .max_tokens
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "auto".to_string()),
                generation
                    .temperature
                    .map(|value| format!("{value:.1}"))
                    .unwrap_or_else(|| "auto".to_string())
            ),
        });
    }

    pub async fn compact_now(&mut self) -> Result<()> {
        let token = self.cancel.token();
        let result = self.compact_if_needed(true, &token).await;
        let _ = self.session.save();
        result
    }

    pub fn clear_history(&mut self) {
        let mode = self.policy.lock().expect("policy lock").mode;
        self.session.state.messages.clear();
        self.session.state.ctx = crate::context::TurnContext::default();
        self.session.state.ctx.mode = mode;
        self.session.state.plan = None;
        let _ = std::fs::remove_file(self.session.dir.join("plan.md"));
        let _ = self.session.save();
        self.emit(Event::HistoryCleared);
    }

    pub async fn run_turn(&mut self, input: String) -> Result<()> {
        self.cancel.reset();
        self.session.state.ctx.begin_turn();
        self.session
            .state
            .messages
            .push(Message::user(input.clone()));
        self.emit(Event::UserMessage { content: input });

        let token = self.cancel.token();
        let usage_before = self.session.state.usage.clone();
        let mut turn = 0u32;

        loop {
            turn += 1;
            if let Some(max_turns) = self.config.max_turns {
                if turn > max_turns.max(1) {
                    self.emit(Event::Error {
                        message: format!("max turns ({max_turns}) reached"),
                    });
                    break;
                }
            }

            self.emit(Event::TurnStarted { turn });
            self.drain_queue();

            if let Err(err) = self.compact_if_needed(false, &token).await {
                if err.to_string() != "cancelled" {
                    self.emit(Event::Error {
                        message: format!("compaction failed: {err}"),
                    });
                }
            }
            if token.is_cancelled() {
                self.emit(Event::Interrupted);
                break;
            }

            self.inject_reminders();

            let request = ChatRequest {
                messages: self.build_messages(),
                tools: self.tools.specs(),
                generation: self.config.generation.clone(),
            };

            let streamed = match self.stream_completion(request, &token).await {
                Ok(value) => value,
                Err(err) => {
                    if err.to_string() == "cancelled" {
                        self.emit(Event::Interrupted);
                    } else {
                        self.emit(Event::Error {
                            message: err.to_string(),
                        });
                    }
                    break;
                }
            };
            let (content, reasoning, calls, usage, finish) = streamed;

            self.session.state.usage.prompt_tokens = self
                .session
                .state
                .usage
                .prompt_tokens
                .saturating_add(usage.prompt_tokens);
            self.session.state.usage.completion_tokens = self
                .session
                .state
                .usage
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            self.session.state.usage.total_tokens = self
                .session
                .state
                .usage
                .total_tokens
                .saturating_add(usage.total_tokens);

            let assistant_text = content.clone().unwrap_or_default();
            self.session.state.messages.push(Message::Assistant {
                content,
                tool_calls: if calls.is_empty() {
                    None
                } else {
                    Some(calls.clone())
                },
                reasoning_content: reasoning,
            });
            self.emit(Event::AssistantMessage {
                content: assistant_text.clone(),
                tool_calls: calls.clone(),
            });

            if matches!(finish, Some(FinishReason::MaxTokens)) {
                self.emit(Event::Notice {
                    message: "model reached its output token limit; ending turn".to_string(),
                });
                break;
            }

            if calls.is_empty() {
                if self.session.state.ctx.mode == Mode::Plan && !assistant_text.trim().is_empty() {
                    self.session.state.plan = Some(assistant_text.clone());
                    let _ = std::fs::write(self.session.dir.join("plan.md"), &assistant_text);
                }
                break;
            }

            self.execute_tool_calls(&calls, &token).await;

            if token.is_cancelled() {
                self.emit(Event::Interrupted);
                break;
            }
        }

        let _ = self.session.save();
        self.emit(Event::TurnFinished {
            turn,
            prompt_tokens: self
                .session
                .state
                .usage
                .prompt_tokens
                .saturating_sub(usage_before.prompt_tokens),
            completion_tokens: self
                .session
                .state
                .usage
                .completion_tokens
                .saturating_sub(usage_before.completion_tokens),
        });
        Ok(())
    }

    fn drain_queue(&mut self) {
        let queued: Vec<String> = {
            let mut queue = self.queue.lock().expect("queue lock");
            queue.drain(..).collect()
        };
        for message in queued {
            self.emit(Event::UserMessage {
                content: message.clone(),
            });
            self.session.state.messages.push(Message::user(message));
        }
    }

    fn inject_reminders(&mut self) {
        let due = self.session.state.ctx.due_reminders();
        for (kind, text) in due {
            self.emit(Event::Reminder {
                kind,
                text: text.clone(),
            });
            self.session.state.messages.push(Message::user(format!(
                "<system-reminder>\n{text}\n</system-reminder>"
            )));
        }
    }

    fn build_messages(&self) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.session.state.messages.len() + 1);
        messages.push(Message::system(self.config.system_prompt.clone()));
        messages.extend(self.session.state.messages.iter().cloned());
        messages
    }

    async fn stream_completion(
        &self,
        request: ChatRequest,
        token: &CancellationToken,
    ) -> Result<(
        Option<String>,
        Option<String>,
        Vec<ToolCall>,
        Usage,
        Option<FinishReason>,
    )> {
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let llm = self.llm.clone();
        let worker_token = token.clone();
        let handle = tokio::spawn(async move { llm.chat_stream(request, tx, worker_token).await });

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        let mut finish = None;

        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    handle.abort();
                    return Err(anyhow!("cancelled"));
                }
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(StreamEvent::Delta(text)) => {
                            content.push_str(&text);
                            self.emit(Event::AssistantDelta { text });
                        }
                        Some(StreamEvent::Reasoning(text)) => {
                            reasoning.push_str(&text);
                            self.emit(Event::AssistantReasoning { text });
                        }
                        Some(StreamEvent::ToolCall(call)) => calls.push(call),
                        Some(StreamEvent::Finish(reason)) => finish = Some(reason),
                    }
                }
            }
        }

        let usage = match handle.await {
            Ok(Ok(usage)) => usage,
            Ok(Err(err)) => return Err(err),
            Err(join_error) => return Err(anyhow!("llm task failed: {join_error}")),
        };

        let content = if content.is_empty() {
            None
        } else {
            Some(content)
        };
        let reasoning = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
        Ok((content, reasoning, calls, usage, finish))
    }

    async fn execute_tool_calls(&mut self, calls: &[ToolCall], token: &CancellationToken) {
        let mut prepared: Vec<PreparedCall> = Vec::new();

        for (index, call) in calls.iter().enumerate() {
            let name = call.function.name.clone();
            let raw_args = call.function.arguments.clone();
            let args = match parse_tool_args(&raw_args) {
                Ok(value) => value,
                Err(err) => {
                    self.emit(Event::ToolStart {
                        id: call.id.clone(),
                        name: name.clone(),
                        args: serde_json::Value::String(raw_args.clone()),
                    });
                    let output = ToolOutput::err(format!(
                        "Invalid JSON arguments: {err}. Re-issue the tool call with valid JSON matching the schema."
                    ));
                    self.finish_tool_call(call, &name, args_for_record(&raw_args), output, None);
                    continue;
                }
            };

            self.emit(Event::ToolStart {
                id: call.id.clone(),
                name: name.clone(),
                args: args.clone(),
            });

            let tool = match self.tools.get(&name) {
                Some(tool) => tool,
                None => {
                    let output = ToolOutput::err(format!(
                        "Unknown tool '{name}'. Available tools: {}",
                        self.tools.names().join(", ")
                    ));
                    self.finish_tool_call(call, &name, args.clone(), output, None);
                    continue;
                }
            };

            let tool_ctx = ToolCtx {
                cwd: self.config.cwd.clone(),
                read_ledger: self.read_ledger.clone(),
                cancel: token.clone(),
                spill_dir: self.session.spill_dir(),
                interactor: self.interactor.clone(),
            };

            if let Err(message) = tool.validate(&args, &tool_ctx).await {
                let output = ToolOutput::err(message);
                self.finish_tool_call(call, &name, args.clone(), output, None);
                continue;
            }

            let decision = {
                let policy = self.policy.lock().expect("policy lock");
                policy.decide(tool.as_ref(), &args)
            };

            match decision {
                Decision::Allow => {}
                Decision::Deny(reason) => {
                    self.emit(Event::ToolDenied {
                        id: call.id.clone(),
                        name: name.clone(),
                        reason: reason.clone(),
                    });
                    let output = ToolOutput::err(format!("Tool use denied: {reason}"));
                    self.finish_tool_call(call, &name, args.clone(), output, None);
                    continue;
                }
                Decision::Ask => {
                    let preview = tool
                        .preview(&args)
                        .unwrap_or_else(|| render_args(&name, &args));
                    let request = ApprovalRequest {
                        tool: name.clone(),
                        preview,
                        rule: rule_key(&name, &args),
                    };
                    match self.interactor.approve(request).await {
                        ApprovalDecision::Once => {}
                        ApprovalDecision::Always => {
                            self.policy
                                .lock()
                                .expect("policy lock")
                                .allow_always(&name, &args);
                            self.persist_permission(&name, &args);
                        }
                        ApprovalDecision::Deny(feedback) => {
                            self.emit(Event::ToolDenied {
                                id: call.id.clone(),
                                name: name.clone(),
                                reason: feedback.clone(),
                            });
                            let output = if feedback.trim().is_empty() {
                                ToolOutput::err("User denied this tool call.")
                            } else {
                                ToolOutput::err(format!(
                                    "User denied this tool call with feedback: {feedback}"
                                ))
                            };
                            self.finish_tool_call(call, &name, args.clone(), output, None);
                            continue;
                        }
                    }
                }
            }

            if !self.config.pre_tool_use.is_empty() {
                if let Some(failure) = self
                    .run_hooks(&self.config.pre_tool_use, "pre_tool_use", &name, &args)
                    .await
                {
                    let output =
                        ToolOutput::err(format!("Blocked by pre_tool_use hook: {failure}"));
                    self.finish_tool_call(call, &name, args.clone(), output, None);
                    continue;
                }
            }

            self.session.state.ctx.on_tool_call();
            let sink = ProgressSink::new(self.events.clone(), call.id.clone(), name.clone());
            prepared.push(PreparedCall { tool, args, sink });

            let _ = index;
        }

        if prepared.is_empty() {
            return;
        }

        let parallel = prepared.len() > 1
            && prepared
                .iter()
                .all(|item| item.tool.concurrency_safe(&item.args));

        let mut outputs: Vec<Option<ToolOutput>> = prepared.iter().map(|_| None).collect();

        if parallel {
            let mut set = JoinSet::new();
            for (index, item) in prepared.into_iter().enumerate() {
                let tool = item.tool.clone();
                let args = item.args.clone();
                let sink = item.sink.clone();
                let ctx = ToolCtx {
                    cwd: self.config.cwd.clone(),
                    read_ledger: self.read_ledger.clone(),
                    cancel: token.clone(),
                    spill_dir: self.session.spill_dir(),
                    interactor: self.interactor.clone(),
                };
                let worker_token = token.clone();
                set.spawn(async move {
                    let output = run_one(tool, args, ctx, sink, worker_token).await;
                    (index, output)
                });
            }
            while let Some(joined) = set.join_next().await {
                if let Ok((index, output)) = joined {
                    outputs[index] = Some(output);
                }
            }
        } else {
            for (index, item) in prepared.into_iter().enumerate() {
                let ctx = ToolCtx {
                    cwd: self.config.cwd.clone(),
                    read_ledger: self.read_ledger.clone(),
                    cancel: token.clone(),
                    spill_dir: self.session.spill_dir(),
                    interactor: self.interactor.clone(),
                };
                let output = run_one(
                    item.tool.clone(),
                    item.args.clone(),
                    ctx,
                    item.sink.clone(),
                    token.clone(),
                )
                .await;
                outputs[index] = Some(output);
            }
        }

        for (index, call) in calls.iter().enumerate() {
            let mut output = outputs
                .get_mut(index)
                .and_then(|slot| slot.take())
                .unwrap_or_else(|| ToolOutput::err("Tool produced no output"));
            if !self.config.post_tool_use.is_empty() {
                let name = call.function.name.clone();
                let args =
                    parse_tool_args(&call.function.arguments).unwrap_or(serde_json::Value::Null);
                if let Some(failure) = self
                    .run_hooks(&self.config.post_tool_use, "post_tool_use", &name, &args)
                    .await
                {
                    output
                        .content
                        .push_str(&format!("\n\n[post_tool_use hook failed]\n{failure}"));
                    output.is_error = true;
                }
            }
            self.finish_tool_call_with_index(call, output);
        }
    }

    fn finish_tool_call(
        &mut self,
        call: &ToolCall,
        name: &str,
        args: serde_json::Value,
        output: ToolOutput,
        command: Option<&str>,
    ) {
        let ToolOutput {
            content: raw,
            is_error,
            images,
        } = output;
        let mut content = self.spill(name, &call.id, raw);
        if !images.is_empty() && !self.config.vision {
            content.push_str(&format!(
                "\n[{} image(s) saved; this model has no vision so they were not attached.]",
                images.len()
            ));
        }
        self.emit(Event::ToolResult {
            id: call.id.clone(),
            name: name.to_string(),
            content: content.clone(),
            is_error,
        });
        self.session
            .state
            .messages
            .push(Message::tool(call.id.clone(), name.to_string(), content));
        if !images.is_empty() && self.config.vision {
            self.session.state.messages.push(Message::user_with_images(
                format!("Image returned by {name}."),
                images,
            ));
            self.emit(Event::Notice {
                message: format!("image attached from {name}"),
            });
        }
        self.session
            .state
            .ctx
            .on_tool_result(name, is_error, command);
        self.update_todos_from_args(name, &args, is_error);
    }

    fn finish_tool_call_with_index(&mut self, call: &ToolCall, output: ToolOutput) {
        let name = call.function.name.clone();
        let args: serde_json::Value =
            parse_tool_args(&call.function.arguments).unwrap_or(serde_json::Value::Null);
        let command = if name == "bash" {
            args.get("command")
                .and_then(|value| value.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };
        let ToolOutput {
            content: raw,
            is_error,
            images,
        } = output;
        let mut content = self.spill(&name, &call.id, raw);
        if !images.is_empty() && !self.config.vision {
            content.push_str(&format!(
                "\n[{} image(s) saved; this model has no vision so they were not attached.]",
                images.len()
            ));
        }
        self.emit(Event::ToolResult {
            id: call.id.clone(),
            name: name.clone(),
            content: content.clone(),
            is_error,
        });
        self.session
            .state
            .messages
            .push(Message::tool(call.id.clone(), name.clone(), content));
        if !images.is_empty() && self.config.vision {
            self.session.state.messages.push(Message::user_with_images(
                format!("Image returned by {name}."),
                images,
            ));
            self.emit(Event::Notice {
                message: format!("image attached from {name}"),
            });
        }
        self.session
            .state
            .ctx
            .on_tool_result(&name, is_error, command.as_deref());
        self.update_todos_from_args(&name, &args, is_error);
    }

    fn update_todos_from_args(&mut self, name: &str, args: &serde_json::Value, is_error: bool) {
        if name != "todo_write" || is_error {
            return;
        }
        if let Some(todos) = parse_todos(args) {
            self.session.state.ctx.on_todo_update(todos.clone());
            self.emit(Event::TodoUpdated { todos });
        }
    }

    fn persist_permission(&self, tool: &str, args: &serde_json::Value) {
        let Some(path) = &self.config.permissions_file else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{}", rule_key(tool, args));
        }
    }

    async fn run_hooks(
        &self,
        commands: &[String],
        phase: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<String> {
        let args_json = serde_json::to_string(args).unwrap_or_default();
        for command in commands {
            let mut child = match tokio::process::Command::new("bash")
                .arg("-lc")
                .arg(command)
                .current_dir(&self.config.cwd)
                .env("WHISKER_HOOK_PHASE", phase)
                .env("WHISKER_TOOL_NAME", tool)
                .env("WHISKER_TOOL_ARGS", &args_json)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(err) => return Some(format!("hook failed to spawn: {err}")),
            };

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(args_json.as_bytes()).await;
            }

            let result =
                tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await;
            match result {
                Ok(Ok(output)) if output.status.success() => continue,
                Ok(Ok(output)) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let code = output.status.code().unwrap_or(-1);
                    return Some(format!(
                        "hook exited with {code}\n{}{}",
                        stdout.trim(),
                        stderr.trim()
                    ));
                }
                Ok(Err(err)) => return Some(format!("hook failed: {err}")),
                Err(_) => return Some("hook timed out after 30s".to_string()),
            }
        }
        None
    }

    fn spill(&self, name: &str, call_id: &str, content: String) -> String {
        if content.chars().count() <= MAX_TOOL_RESULT_CHARS {
            return content;
        }
        let dir = self.session.spill_dir();
        let _ = std::fs::create_dir_all(&dir);
        let safe_name: String = name
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect();
        let path = dir.join(format!("{safe_name}-{call_id}.txt"));
        let preview: String = content.chars().take(PREVIEW_CHARS).collect();
        match std::fs::write(&path, &content) {
            Ok(_) => format!(
                "[Tool output truncated: {} chars total. Full output saved to {}]\n\n{}",
                content.chars().count(),
                path.display(),
                preview
            ),
            Err(_) => preview,
        }
    }

    async fn compact_if_needed(&mut self, force: bool, token: &CancellationToken) -> Result<()> {
        let total = estimate_messages(&self.session.state.messages)
            + estimate_text(&self.config.system_prompt);
        if !force && (total as f64) < self.config.context_window as f64 * COMPACT_RATIO {
            return Ok(());
        }
        let length = self.session.state.messages.len();
        if length <= 6 {
            if force {
                self.emit(Event::Notice {
                    message: "Not enough history to compact.".to_string(),
                });
            }
            return Ok(());
        }
        let keep = 6;
        let mut split = length.saturating_sub(keep);
        while split < length {
            match &self.session.state.messages[split] {
                Message::User { .. } => break,
                _ => split += 1,
            }
        }
        if split == 0 || split >= length {
            if force {
                self.emit(Event::Notice {
                    message: "Not enough history to compact.".to_string(),
                });
            }
            return Ok(());
        }
        let old: Vec<Message> = self.session.state.messages.drain(0..split).collect();
        let transcript = render_transcript(&old);
        let request = ChatRequest {
            messages: vec![
                Message::system(
                    "You compress agent conversation history. Preserve decisions, files changed, commands run, errors, open questions and user constraints. Be concise and factual. Do not add new information.",
                ),
                Message::user(format!("Summarize this conversation segment:\n\n{transcript}")),
            ],
            tools: Vec::new(),
            generation: GenerationSettings {
                temperature: Some(0.2),
                max_tokens: Some(1200),
                thinking: Some(false),
                ..GenerationSettings::default()
            },
        };
        let summary = self.collect_text(request, token).await?;
        let plan_section = self
            .session
            .state
            .plan
            .as_ref()
            .map(|plan| format!("\n\nCurrent plan:\n{plan}"))
            .unwrap_or_default();
        let reminder = format!(
            "<conversation-summary>\n{}\n</conversation-summary>\n\nCurrent todos:\n{}{}",
            summary,
            render_todos(&self.session.state.ctx.todos),
            plan_section
        );
        self.session
            .state
            .messages
            .insert(0, Message::user(reminder));
        self.emit(Event::Compacted { removed: split });
        Ok(())
    }

    async fn collect_text(
        &self,
        request: ChatRequest,
        token: &CancellationToken,
    ) -> Result<String> {
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let llm = self.llm.clone();
        let worker_token = token.clone();
        let handle = tokio::spawn(async move { llm.chat_stream(request, tx, worker_token).await });

        let mut text = String::new();
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    handle.abort();
                    return Err(anyhow!("cancelled"));
                }
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(StreamEvent::Delta(delta)) => text.push_str(&delta),
                        Some(StreamEvent::Reasoning(_))
                        | Some(StreamEvent::ToolCall(_))
                        | Some(StreamEvent::Finish(_)) => {}
                    }
                }
            }
        }
        handle.await??;
        Ok(text)
    }
}

async fn run_one(
    tool: Arc<dyn Tool>,
    args: serde_json::Value,
    ctx: ToolCtx,
    sink: ProgressSink,
    token: CancellationToken,
) -> ToolOutput {
    if token.is_cancelled() {
        return ToolOutput::err("Cancelled by user.");
    }
    match tokio::time::timeout(TOOL_TIMEOUT, tool.execute(args, ctx, sink)).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => ToolOutput::err(format!("Tool failed: {err}")),
        Err(_) => ToolOutput::err(format!(
            "Tool timed out after {} seconds.",
            TOOL_TIMEOUT.as_secs()
        )),
    }
}

fn args_for_record(raw: &str) -> serde_json::Value {
    serde_json::Value::String(raw.to_string())
}

fn parse_todos(args: &serde_json::Value) -> Option<Vec<TodoItem>> {
    let array = args.get("todos")?.as_array()?;
    let mut todos = Vec::new();
    for (index, item) in array.iter().enumerate() {
        let content = item.get("content")?.as_str()?.trim().to_string();
        if content.is_empty() {
            continue;
        }
        let status = item
            .get("status")
            .and_then(|value| value.as_str())
            .and_then(TodoStatus::parse)
            .unwrap_or(TodoStatus::Pending);
        let id = item
            .get("id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| (index + 1).to_string());
        todos.push(TodoItem {
            id,
            content,
            status,
        });
    }
    Some(todos)
}

fn render_args(name: &str, args: &serde_json::Value) -> String {
    if name == "bash" {
        return args
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
    }
    let raw = serde_json::to_string_pretty(args).unwrap_or_default();
    let truncated: String = raw.chars().take(1200).collect();
    truncated
}

fn render_todos(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "(none)".to_string();
    }
    todos
        .iter()
        .map(|todo| format!("- [{}] {}", todo.status.as_str(), todo.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        let line = match message {
            Message::System { content } => format!("SYSTEM: {content}"),
            Message::User { content, images } => {
                let suffix = if images.is_empty() {
                    String::new()
                } else {
                    format!(" [{} image(s)]", images.len())
                };
                format!("USER: {content}{suffix}")
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut text = format!("ASSISTANT: {}", content.clone().unwrap_or_default());
                if let Some(calls) = tool_calls {
                    for call in calls {
                        text.push_str(&format!(
                            "\nTOOL_CALL: {}({})",
                            call.function.name, call.function.arguments
                        ));
                    }
                }
                text
            }
            Message::Tool {
                content,
                name,
                tool_call_id,
            } => format!(
                "TOOL_RESULT {} ({tool_call_id}): {content}",
                name.clone().unwrap_or_default()
            ),
        };
        let line: String = line.chars().take(2000).collect();
        out.push_str(&line);
        out.push_str("\n\n");
    }
    out
}

fn parse_tool_args(raw: &str) -> Result<serde_json::Value, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({}));
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(match value {
            serde_json::Value::String(inner) => {
                serde_json::from_str(&inner).unwrap_or(serde_json::Value::String(inner))
            }
            other => other,
        });
    }
    if let Some(candidate) = extract_json_candidate(trimmed) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate) {
            return Ok(value);
        }
        let repaired = strip_trailing_commas(&candidate);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&repaired) {
            return Ok(value);
        }
    }
    Err(format!("invalid JSON arguments: {trimmed}"))
}

fn extract_json_candidate(text: &str) -> Option<String> {
    let start = text.find(|ch| ch == '{' || ch == '[')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(text[start..end].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == ',' {
            let mut look = index + 1;
            while look < chars.len() && chars[look].is_whitespace() {
                look += 1;
            }
            if look < chars.len() && (chars[look] == '}' || chars[look] == ']') {
                index += 1;
                continue;
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

fn estimate_text(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    (cjk as f64 / 1.5 + other as f64 / 3.5).ceil() as usize
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
    )
}

fn estimate_messages(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| match message {
            Message::System { content } => estimate_text(content),
            Message::User { content, images } => {
                estimate_text(content) + images.len().saturating_mul(1024)
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut total = estimate_text(content.as_deref().unwrap_or_default());
                if let Some(calls) = tool_calls {
                    for call in calls {
                        total += estimate_text(&call.function.arguments);
                    }
                }
                total
            }
            Message::Tool { content, .. } => estimate_text(content),
        })
        .sum()
}
