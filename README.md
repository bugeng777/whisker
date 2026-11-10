# whisker — a cat-minded terminal coding agent

A terminal coding agent written in Rust for OpenAI-compatible models (DeepSeek first).
The workflow is modelled on Claude Code's harness-driven style: the model makes local
decisions, while the harness enforces the process (read-before-edit, approvals, reminders,
plan mode, context compaction).

> Sense first. Pounce once.

## Features

- **OpenAI-compatible providers** with streaming SSE, tool-call assembly, retries,
  and a configurable `reasoning_content` policy (DeepSeek reasoner friendly).
- **Native tools**: `read`, `write`, `edit`, `glob`, `grep`, `bash` (tree-sitter safety
  analysis), `open_app`, `todo_write`, `ask_user`, `skill`, `lsp`.
- **Harness invariants**: read-before-edit is a hard gate; destructive commands require
  approval; plan mode is read-only; event-driven reminders cover todos and verification.
- **Loop termination**: the loop waits for the provider finish reason — `stop` / no tool
  calls completes, `tool_calls` executes and continues, `max_tokens` ends with a notice.
  `max_turns` is optional; omit it to let the model decide when to stop.
- **Subagents**: `agent` tool runs a read-only exploration loop with an isolated context
  and returns a concise report.
- **Skills**: markdown procedures loaded on demand from `.whisker/skills` or `.claude/skills`.
- **MCP**: stdio and Streamable HTTP sidecars share the same approval/truncation pipeline.
- **Vision**: models declared with `vision = true` (for example `deepseek-flash`) receive
  MCP screenshots as `image_url` content parts; images are saved to the session spill and
  injected as user messages. Text-only models get a short text note instead.
- **LSP**: diagnostics, definition, hover and document symbols through configured servers.
- **Sandbox (macOS)**: opt-in seatbelt profile that restricts bash file writes to the
  workspace and temp dirs.
- **PowerShell tool** on Windows (pwsh/powershell) with the same approval rules.
- **Desktop apps**: `open_app` launches or activates an application, optionally with a
  file or URL, and uses the normal approval rules. Full OS-level clicking/typing is
  left to an MCP sidecar.
- **Machine-readable output**: `--output-format stream-json` for IDE/SDK integrations.
- **Sessions**: append-only JSONL event log, snapshots, `--continue`/`--resume`, and
  context compaction.
- **UI**: the full-screen ratatui TUI starts by default on a terminal; `--no-tui`
  falls back to the line REPL. The TUI shows a compact 14×4 cat (Claude-sized) in a
  66-column welcome box, with the conversation full-width below. The cat switches
  between idle / scanning / working / purring poses (whiskers, pupils, purr waves)
  and `/cat <pose>` pins one for a few seconds. `/config` opens a Claude-style
  settings page with searchable tabs for settings, status and token usage.

## Build and install

```bash
cargo build --release
./scripts/install.sh          # installs to ~/.local/bin/whisker
```

Requires a recent stable Rust (tested on 1.96), macOS or Linux.

## Quick start

```bash
# 1. Put your key in agent/.env (DEEPSEEK_API_KEY=sk-...)
# 2. Run from the agent directory so .env is picked up:
./target/release/whisker --cwd /path/to/your/project          # TUI
./target/release/whisker --no-tui --cwd /path/to/your/project # line REPL
./target/release/whisker -p "fix the failing test" --auto --cwd /path/to/project
./target/release/whisker -p "..." --output-format stream-json --cwd /path/to/project
./target/release/whisker --plan --cwd /path/to/project
```

Keys are resolved from (highest priority first): shell environment, `./.env`,
`WHISKER_ENV_FILE`, `~/.whisker/.env`.

## Configuration

Copy `config.example.toml` to `~/.whisker/config.toml` and edit. Any OpenAI-compatible
endpoint works:

```toml
[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"
model = "deepseek-flash"
vision = true                   # accept image parts; defaults from the model name
context_window = 1048576
reasoning_content = "drop"     # use "echo" if your provider requires it back
sandbox = false                 # true = restrict bash writes (macOS seatbelt)
```

Provider selection: `--profile`, `[agent] profile`, or the built-in DeepSeek default.

## Workflow

- **Read before edit**: the edit tool refuses files that were not fully read, and refuses
  stale reads. Error messages explain how to recover.
- **Approvals**: writes and state-changing commands ask for `[y] once / [a] always /
  [n] deny + feedback`. "Always" rules are appended to `~/.whisker/permissions.allow`.
- **Plan mode**: only read-only tools are available. The plan is stored in `plan.md` and
  re-injected after context compaction.
- **Reminders**: if a multi-step task has no todo list, or edits are unverified, the
  harness injects a one-shot system reminder.
- **Context**: tool output over 32K chars spills to disk; context is compacted at 70% of
  the window (CJK-aware token estimation).

## Slash commands

```
/help    /model    /mode    /plan    /skills    /compact    /clear    /cost
/cat idle|scanning|working|success
```

`/model` and `/mode` open an interactive picker: ↑/↓ to move, Enter to confirm, Esc to
cancel. `/model` switches the provider model for subsequent requests; `provider.models`
in the config file sets the options (DeepSeek defaults to flash/v4-pro).

For DeepSeek, `/config` → **DeepSeek** exposes thinking on/off, reasoning effort
(low/high/max), max output tokens, temperature, and context window. Changes apply to the
next request and persist to `~/.whisker/deepseek-settings.json`, which overrides the
provider defaults. The same parameters can be set in `config.toml` as `thinking`,
`reasoning_effort`, `max_tokens`, `top_p`, and `temperature`.

## Skills

```
.whisker/skills/review/SKILL.md   (project)   ~/.whisker/skills/...   (global)
.claude/skills/...                        ~/.claude/skills/...
```

```markdown
---
name: review
description: Review the current diff for correctness and missing tests
---
1. Run git diff ...
```

The model sees only the name and description; it calls the `skill` tool to load the
procedure when a task matches.

## Hooks

```toml
[hooks]
pre_tool_use = ["...shell command..."]
post_tool_use = ["..."]
```

The args are passed as JSON on stdin and in `WHISKER_TOOL_ARGS`; `WHISKER_TOOL_NAME` and
`WHISKER_HOOK_PHASE` are set. A non-zero pre-hook blocks the tool; a non-zero post-hook marks
the result as failed.

## MCP sidecars

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[mcp_servers.playwright]
command = "npx"
args = ["-y", "@playwright/mcp@latest"]

[mcp_servers.remote]
url = "https://example.com/mcp"
```

## LSP

```toml
[lsp.rust]
command = "rust-analyzer"
extensions = ["rs"]

[lsp.typescript]
command = "typescript-language-server"
args = ["--stdio"]
extensions = ["ts", "tsx", "js", "jsx"]
language_id = "typescript"
```

## Eval

`eval/run.py` copies each fixture into a temporary workspace, runs `whisker -p` with a real
provider, then verifies the result and records tool calls, turns and tokens from the
session log.

```bash
cargo build --release
python3 eval/run.py            # needs a key in agent/.env
python3 eval/run.py --dry-run  # validate tasks without calling the model
```

## Architecture

```
crates/core   agent loop, TurnContext, policy, reminders, sessions, subagents
crates/llm    OpenAI-compatible streaming provider
crates/tools  native tools, bash AST safety, skills, LSP
crates/mcp    stdio + Streamable HTTP MCP client and tool bridge
crates/ui     line REPL, TUI, cat branding
crates/cli    configuration, wiring, headless mode
```

## Status

Done: core agent, harness, tools, sessions, MCP (stdio/HTTP), skills, LSP, subagents,
hooks, permission persistence, macOS write sandbox, PowerShell tool, desktop app launch,
line REPL, TUI, packaging, stream-json output, eval harness (6/6 baseline), CI.

Out of scope for this release: full IDE plugins / remote bridge UI (stream-json is the
integration primitive), voice, and OS-level computer use. Windows is supported through
the PowerShell tool but has not been end-to-end tested on a Windows host.
