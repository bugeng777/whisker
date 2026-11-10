mod config;

use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::{
    Agent, AgentConfig, ApprovalDecision, ApprovalRequest, CancelHandle, Event, GenerationSettings,
    Interactor, LlmClient, Mode, ModelSettingsStore, Policy, SessionStore, Tool, ToolRegistry,
};
use agent_llm::{OpenAiProvider, ReasoningPolicy};
use agent_tools::native_tools;
use agent_ui::{ChannelInteractor, Command};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use clap::Parser;
use config::ProviderConfig;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(
    name = "whisker",
    version,
    about = "🐈 whisker — a Rust terminal coding agent for OpenAI-compatible models"
)]
struct Args {
    /// Run a single prompt headlessly and exit
    #[arg(short = 'p', long = "print")]
    print: Option<String>,

    /// Headless output format: text or stream-json
    #[arg(long, default_value = "text")]
    output_format: String,

    /// Resume the most recent session
    #[arg(long = "continue")]
    continue_session: bool,

    /// Resume a session by id prefix
    #[arg(long)]
    resume: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long = "base-url")]
    base_url: Option<String>,

    /// Auto-approve tool calls
    #[arg(long)]
    auto: bool,

    /// Start in plan mode (read-only tools)
    #[arg(long)]
    plan: bool,

    /// Force the full-screen TUI (default when attached to a terminal)
    #[arg(long)]
    tui: bool,

    /// Use the line REPL instead of the TUI
    #[arg(long = "no-tui")]
    no_tui: bool,

    #[arg(long)]
    cwd: Option<PathBuf>,

    #[arg(long)]
    config: Option<PathBuf>,

    #[arg(long)]
    profile: Option<String>,
}

fn resolve_tui(force: bool, disable: bool, stdin_tty: bool, stdout_tty: bool) -> bool {
    if disable {
        false
    } else if force {
        true
    } else {
        stdin_tty && stdout_tty
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let tui = resolve_tui(
        args.tui,
        args.no_tui,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    );
    load_env_files();

    if std::env::var("WHISKER_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init();
    }

    let cwd = match &args.cwd {
        Some(path) => path.clone(),
        None => std::env::current_dir()?,
    };
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let skill_catalog = Arc::new(agent_tools::SkillCatalog::load(&cwd));
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| config::default_path(&home));
    if args.config.is_some() && !config_path.is_file() {
        return Err(anyhow!("config file not found: {}", config_path.display()));
    }
    let config = config::load(&config_path)?;

    let profile_name = args
        .profile
        .clone()
        .or(config.agent.profile.clone())
        .unwrap_or_else(|| "deepseek".to_string());
    let provider = config
        .providers
        .get(&profile_name)
        .cloned()
        .unwrap_or_else(|| {
            config::Config::default()
                .providers
                .get("deepseek")
                .cloned()
                .expect("built-in provider")
        });

    let configured_model = args.model.clone().unwrap_or_else(|| provider.model.clone());
    let mut models = provider
        .models
        .clone()
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| config::default_models(&profile_name));
    if models.is_empty() {
        models.push(configured_model.clone());
    } else if !models.iter().any(|choice| choice == &configured_model) {
        models.insert(0, configured_model.clone());
    }
    let model_store =
        ModelSettingsStore::load(&ModelSettingsStore::default_path()).unwrap_or_default();
    let model = if args.model.is_none() {
        model_store
            .active
            .clone()
            .filter(|active| models.iter().any(|model| model == active))
            .unwrap_or_else(|| configured_model.clone())
    } else {
        configured_model.clone()
    };
    if !models.iter().any(|choice| choice == &model) {
        models.insert(0, model.clone());
    }
    let vision = provider
        .vision
        .unwrap_or_else(|| config::default_vision(&model));
    let base_url = args
        .base_url
        .clone()
        .unwrap_or_else(|| provider.base_url.clone());
    let api_key = resolve_api_key(&provider, &config_path)?;

    let deepseek_mode = profile_name.eq_ignore_ascii_case("deepseek")
        || base_url.contains("api.deepseek.com")
        || model.to_ascii_lowercase().contains("deepseek");
    let mut fallback_generation = GenerationSettings {
        temperature: provider.temperature.or(config.agent.temperature),
        max_tokens: provider.max_tokens,
        top_p: provider.top_p,
        thinking: provider.thinking,
        reasoning_effort: provider.reasoning_effort.clone(),
    };
    if deepseek_mode {
        if fallback_generation.thinking.is_none() {
            fallback_generation.thinking = Some(true);
        }
        if fallback_generation.reasoning_effort.is_none() {
            fallback_generation.reasoning_effort = Some("high".to_string());
        }
    }
    let saved = model_store.get(&model);
    let mut generation = saved
        .map(|settings| settings.generation.clone())
        .unwrap_or(fallback_generation);
    if deepseek_mode {
        if generation.thinking.is_none() {
            generation.thinking = Some(true);
        }
        if generation.reasoning_effort.is_none() {
            generation.reasoning_effort = Some("high".to_string());
        }
    }
    let context_window = saved
        .and_then(|settings| settings.context_window)
        .or(provider.context_window)
        .unwrap_or(65536);

    let reasoning_policy = match provider.reasoning_content.as_deref() {
        Some("echo") => ReasoningPolicy::Echo,
        _ => ReasoningPolicy::Drop,
    };
    let llm: Arc<dyn LlmClient> = Arc::new(OpenAiProvider::new(
        base_url,
        api_key,
        model.clone(),
        context_window,
        reasoning_policy,
    )?);

    let session_base = home.join(".whisker").join("sessions");
    let mut session = if let Some(prefix) = &args.resume {
        SessionStore::resume(&session_base, prefix)?
    } else if args.continue_session {
        SessionStore::resume_latest(&session_base)?
    } else {
        SessionStore::create(&session_base, &model)?
    };
    let session_id = session.state.id.clone();

    let sandbox = config.agent.sandbox.unwrap_or(false);
    let mut tools = native_tools(sandbox);
    if !skill_catalog.is_empty() {
        tools.push(Arc::new(agent_tools::SkillTool::new(skill_catalog.clone())));
    }
    if !config.lsp.is_empty() {
        let servers: Vec<agent_tools::LspServerConfig> = config.lsp.values().cloned().collect();
        tools.push(Arc::new(agent_tools::LspTool::new(cwd.clone(), servers)));
    }
    if let Some(program) = agent_tools::powershell::find_program() {
        tools.push(Arc::new(agent_tools::PowerShellTool::new(program)));
    }
    let (mcp_tools, warnings) = agent_mcp::load_tools(&config.mcp_servers).await;
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    tools.extend(mcp_tools);

    let sub_tools: Vec<Arc<dyn Tool>> = tools
        .iter()
        .filter(|tool| matches!(tool.name(), "read" | "grep" | "glob" | "bash"))
        .cloned()
        .collect();
    tools.push(Arc::new(agent_core::SubagentTool {
        llm: llm.clone(),
        tools: Arc::new(ToolRegistry::new(sub_tools)),
        max_turns: 8,
        system_prompt: include_str!("../../../prompts/subagent.md").to_string(),
        generation: generation.clone(),
    }));

    let registry = Arc::new(ToolRegistry::new(tools));

    let mode = if args.plan {
        Mode::Plan
    } else if args.auto {
        Mode::AutoAccept
    } else {
        session.state.ctx.mode
    };
    session.state.ctx.mode = mode;

    let permissions_path = home.join(".whisker").join("permissions.allow");
    let mut allow_rules = config.permissions.allow.clone();
    if let Ok(raw) = std::fs::read_to_string(&permissions_path) {
        for line in raw.lines() {
            let rule = line.trim();
            if rule.is_empty() || rule.starts_with('#') {
                continue;
            }
            if !allow_rules.iter().any(|existing| existing == rule) {
                allow_rules.push(rule.to_string());
            }
        }
    }
    let policy = Arc::new(Mutex::new(Policy::new(
        mode,
        &allow_rules,
        &config.permissions.deny,
    )));

    let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();
    let (interaction_tx, interaction_rx) = mpsc::unbounded_channel();

    let interactor: Arc<dyn Interactor> = if args.print.is_some() {
        Arc::new(HeadlessInteractor {
            auto: args.auto || mode == Mode::AutoAccept,
        })
    } else {
        Arc::new(ChannelInteractor::new(interaction_tx))
    };

    let cancel = CancelHandle::new();
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let read_ledger = Arc::new(Mutex::new(Default::default()));
    let mut system_prompt = build_system_prompt(&cwd);
    system_prompt.push_str(&skill_catalog.prompt_section());
    let max_turns = config.agent.max_turns;

    let agent = Agent {
        llm,
        tools: registry,
        policy,
        interactor,
        events: event_tx,
        session,
        config: AgentConfig {
            model: model.clone(),
            cwd: cwd.clone(),
            vision,
            context_window,
            max_turns,
            system_prompt,
            generation: generation.clone(),
            skills: skill_catalog.summaries(),
            pre_tool_use: config.hooks.pre_tool_use.clone(),
            post_tool_use: config.hooks.post_tool_use.clone(),
            permissions_file: Some(permissions_path.clone()),
        },
        cancel: cancel.clone(),
        queue: queue.clone(),
        read_ledger,
    };
    agent.emit(Event::SessionStarted {
        session_id: session_id.clone(),
        model: model.clone(),
    });

    if let Some(prompt) = args.print.clone() {
        run_headless(agent, prompt, event_rx, args.output_format == "stream-json").await
    } else {
        run_interactive(
            agent,
            event_rx,
            interaction_rx,
            cancel,
            queue,
            model,
            models,
            generation,
            context_window,
            deepseek_mode,
            session_id,
            mode,
            tui,
        )
        .await
    }
}

fn load_env_files() {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("WHISKER_ENV_FILE") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".env"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".whisker").join(".env"));
    }
    for path in candidates {
        load_env_file(&path);
    }
}

fn load_env_file(path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || std::env::var(key).is_ok() {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        std::env::set_var(key, value);
    }
}

fn resolve_api_key(provider: &ProviderConfig, config_path: &Path) -> Result<String> {
    if let Some(key) = &provider.api_key {
        if !key.trim().is_empty() {
            return Ok(key.clone());
        }
    }
    let env_name = provider
        .api_key_env
        .clone()
        .unwrap_or_else(|| "DEEPSEEK_API_KEY".to_string());
    if let Ok(key) = std::env::var(&env_name) {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    Err(anyhow!(
        "no API key found. Set {env_name} or add api_key to the provider in {}",
        config_path.display()
    ))
}

fn build_system_prompt(cwd: &Path) -> String {
    let mut prompt = include_str!("../../../prompts/system.md").to_string();

    prompt.push_str("\n\n# Environment\n");
    prompt.push_str(&format!(
        "- Working directory: {}\n- Platform: {} ({})\n- Date: {}\n",
        cwd.display(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Local::now().format("%Y-%m-%d")
    ));

    if let Some(status) = git_status(cwd) {
        prompt.push_str("\n## Git status\n```\n");
        prompt.push_str(&status);
        prompt.push_str("\n```\n");
    }

    if let Some((path, text)) = project_instructions(cwd) {
        prompt.push_str(&format!(
            "\n# Project instructions\nSource: {}\n\n{}\n",
            path.display(),
            text
        ));
    }

    prompt
}

fn git_status(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--short", "--branch"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.lines().take(30).collect::<Vec<_>>().join("\n");
    if trimmed.trim().is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn project_instructions(cwd: &Path) -> Option<(PathBuf, String)> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            let path = dir.join(name);
            if path.is_file() {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if !text.trim().is_empty() {
                        return Some((path, text));
                    }
                }
            }
        }
        current = dir.parent();
    }
    let global = dirs::home_dir()?.join(".whisker").join("AGENTS.md");
    if global.is_file() {
        if let Ok(text) = std::fs::read_to_string(&global) {
            if !text.trim().is_empty() {
                return Some((global, text));
            }
        }
    }
    None
}

struct HeadlessInteractor {
    auto: bool,
}

#[async_trait]
impl Interactor for HeadlessInteractor {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalDecision {
        if self.auto {
            ApprovalDecision::Once
        } else {
            ApprovalDecision::Deny(format!(
                "non-interactive session: tool '{}' requires approval. Re-run with --auto or add a permission rule.",
                request.tool
            ))
        }
    }

    async fn ask(&self, _question: String, _options: Vec<String>) -> Option<String> {
        None
    }
}

async fn run_headless(
    mut agent: Agent,
    prompt: String,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
    stream_json: bool,
) -> Result<()> {
    if stream_json {
        let printer = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                if let Ok(line) = serde_json::to_string(&event) {
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                }
            }
        });
        let result = agent.run_turn(prompt).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        printer.abort();
        return result;
    }

    let color = std::io::stdout().is_terminal();
    let printer = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::AssistantDelta { text } => {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
                Event::AssistantReasoning { .. } => {}
                Event::ToolStart { name, args, .. } => {
                    let line = format!("⏺ {name} {}", compact_args(&args));
                    if color {
                        eprintln!("\x1b[2m{line}\x1b[0m");
                    } else {
                        eprintln!("{line}");
                    }
                }
                Event::ToolResult {
                    content, is_error, ..
                } => {
                    let first = content.lines().next().unwrap_or("");
                    let marker = if is_error { "error" } else { "ok" };
                    let line = format!("  ⎿ {marker}: {}", truncate(first, 160));
                    if color {
                        eprintln!("\x1b[2m{line}\x1b[0m");
                    } else {
                        eprintln!("{line}");
                    }
                }
                Event::ToolDenied { reason, .. } => eprintln!("  ⎿ denied: {reason}"),
                Event::Reminder { kind, .. } => eprintln!("[harness] {kind}"),
                Event::ModeChanged { mode } => eprintln!("mode → {mode}"),
                Event::Compacted { removed } => {
                    eprintln!("[context compacted: {removed} messages]")
                }
                Event::Interrupted => eprintln!("interrupted"),
                Event::Error { message } => eprintln!("error: {message}"),
                Event::TurnFinished {
                    prompt_tokens,
                    completion_tokens,
                    ..
                } => eprintln!("— tokens in {prompt_tokens} / out {completion_tokens}"),
                _ => {}
            }
        }
    });

    let result = agent.run_turn(prompt).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    printer.abort();
    result
}

async fn run_interactive(
    agent: Agent,
    event_rx: mpsc::UnboundedReceiver<Event>,
    interaction_rx: mpsc::UnboundedReceiver<agent_ui::Interaction>,
    cancel: CancelHandle,
    queue: Arc<Mutex<VecDeque<String>>>,
    model: String,
    models: Vec<String>,
    generation: GenerationSettings,
    context_window: usize,
    deepseek_mode: bool,
    session_id: String,
    mode: Mode,
    tui: bool,
) -> Result<()> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(agent_ui::run_agent(agent, cmd_rx));
    let result = if tui {
        agent_ui::run_tui(
            cmd_tx.clone(),
            event_rx,
            interaction_rx,
            cancel,
            queue,
            model,
            models,
            generation,
            context_window,
            deepseek_mode,
            session_id,
            mode.as_str(),
        )
        .await
    } else {
        agent_ui::run_repl(
            cmd_tx.clone(),
            event_rx,
            interaction_rx,
            cancel,
            queue,
            model,
            session_id,
            mode.as_str(),
        )
        .await
    };
    let _ = cmd_tx.send(Command::Quit);
    let _ = handle.await;
    result
}

fn compact_args(args: &serde_json::Value) -> String {
    let arg = |key: &str| {
        args.get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
    };
    if !arg("command").is_empty() {
        return truncate(arg("command"), 120);
    }
    if !arg("path").is_empty() {
        return arg("path").to_string();
    }
    if !arg("pattern").is_empty() {
        return arg("pattern").to_string();
    }
    let raw = serde_json::to_string(args).unwrap_or_default();
    truncate(&raw, 100)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_tui;

    #[test]
    fn tui_defaults_to_terminals() {
        assert!(resolve_tui(false, false, true, true));
        assert!(!resolve_tui(false, false, false, true));
        assert!(!resolve_tui(false, false, true, false));
    }

    #[test]
    fn flags_override_tui_default() {
        assert!(!resolve_tui(false, true, true, true));
        assert!(resolve_tui(true, false, false, false));
        assert!(!resolve_tui(true, true, true, true));
    }
}
