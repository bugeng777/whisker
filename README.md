# whisker — a cat-minded terminal coding agent

A terminal coding agent written in Rust for OpenAI-compatible models (DeepSeek first).
The workflow is modelled on Claude Code's harness-driven style: the model makes local
decisions, while the harness enforces the process (read-before-edit, approvals, reminders,
plan mode, context compaction).

> Sense first. Pounce once.

New machine or contributing? See [DEVELOPMENT.md](DEVELOPMENT.md) for the
architecture, validation workflow, setup and release process.

## Install

```bash
curl -fsSL https://github.com/bugeng777/whisker/releases/latest/download/install.sh | bash
```

Supported: macOS arm64/x64 and Linux x64/arm64. The installer downloads the
matching release, verifies its SHA256 checksum, and installs to
`~/.local/bin/whisker`. Interactive sessions check GitHub Releases at most once
per day and print a reinstall hint when a newer version exists; Whisker never
updates itself (`update_check = false` disables the check). Pin a version or
choose another directory:

```bash
curl -fsSL https://github.com/bugeng777/whisker/releases/latest/download/install.sh \
  | WHISKER_VERSION=v0.6.1 WHISKER_INSTALL_DIR="$HOME/.local/bin" bash
```

### Build from source

```bash
cargo build --release
./scripts/install.sh          # installs to ~/.local/bin/whisker
```

Requires a recent stable Rust (tested on 1.96).

## Quick start

```bash
# 1. Put your key in ~/.whisker/.env (DEEPSEEK_API_KEY=sk-...)
# 2. Run:
whisker --cwd /path/to/your/project          # TUI
whisker --no-tui --cwd /path/to/your/project # line REPL
whisker -p "fix the failing test" --auto --cwd /path/to/project
whisker -p "..." --output-format stream-json --cwd /path/to/project
whisker --plan --cwd /path/to/project
```

For source builds, replace `whisker` with `./target/release/whisker`.

Keys are resolved from (highest priority first): shell environment, `./.env`,
`WHISKER_ENV_FILE`, `~/.whisker/.env`.

## Features

- **OpenAI-compatible providers** with streaming SSE, tool-call assembly, retries,
  and a configurable `reasoning_content` policy (DeepSeek reasoner friendly).
- **Native tools**: `read`, `write`, `edit`, `glob`, `grep`, `bash` (tree-sitter safety
  analysis), `open_app`, `todo_write`, `ask_user`, `skill`, `lsp`. Tools can be deferred
  behind `tool_search` (`deferred_tools` / `defer_mcp_tools`) to keep large MCP sets
  out of the prompt.
- **Harness invariants**: read-before-edit is a hard gate; destructive commands require
  approval; plan mode is read-only; event-driven reminders cover todos and verification.
  Todo reminders use the validated 10/10 turn policy, fire only when `todo_write` is
  available, and are configurable via `todo_reminder_after` / `todo_reminder_interval`.
- **Loop termination**: the loop waits for the provider finish reason — `stop` / no tool
  calls completes, `tool_calls` executes and continues, `max_tokens` ends with a notice.
  `max_turns` is optional; omit it to let the model decide when to stop.
- **Subagents**: `agent` tool runs a read-only exploration loop with an isolated context
  and returns a concise report. Depth is fixed at 1; turns and concurrency are capped by
  `subagent_max_turns` / `subagent_max_concurrency`.
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
  settings page with searchable tabs for settings, status and token usage. The
  header rotates one tip per prompt and shows thinking, stall and tool timers.

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
- **Reminders**: todo reminders follow the 10/10 turn policy; unverified edits still
  produce a one-shot verification reminder.
- **Context**: tool output over 32K chars spills to disk; context auto-compacts at 90%
  of the configured window (`compact_ratio`) or at an absolute `compact_window` token
  count. Token estimation is CJK-aware.

## Slash commands

```
/help    /model    /mode    /plan    /skills    /compact    /clear    /cost
/cat idle|scanning|working|success
```

`/model` and `/mode` open an interactive picker: ↑/↓ to move, Enter to confirm, Esc to
cancel. `/model` switches the provider model and includes inline settings for the
highlighted model. Press `→`/`Tab` to edit thinking on/off, reasoning effort
(low/high/max), max output tokens, temperature, and context window; `←` returns to the
model list. Settings are saved per model in `~/.whisker/model-settings.json` (override
with `WHISKER_MODEL_SETTINGS`), together with the last active model. `provider.models`
in the config file sets the available options (DeepSeek defaults to flash/v4-pro).

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
stop = ["..."]
```

The args are passed as JSON on stdin and in `WHISKER_TOOL_ARGS`; `WHISKER_TOOL_NAME` and
`WHISKER_HOOK_PHASE` are set. A non-zero pre-hook blocks the tool; a non-zero post-hook marks
the result as failed. The pre-hook runs before the permission check, so a denied call still
triggers `pre_tool_use` but never `post_tool_use`.

The `stop` hook runs once when the model finishes without tool calls. It receives
`{"hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"...",
"terminal_reason":"completed"}` on stdin plus `WHISKER_HOOK_EVENT` and
`WHISKER_STOP_HOOK_ACTIVE`. Printing a JSON line with `"decision":"block"` and a `reason`
asks for one more model turn; a second block ends the turn with a notice. `stop_hook_active`
is true on that continuation, mirroring Claude Code.

Lifecycle hooks are observational and never block: `session_start`, `session_end`,
`pre_compact`, `post_compact`. They receive `hook_event_name`, `session_id`,
`transcript_path`, `cwd`, and `model` on stdin; compaction hooks add `trigger`
(`auto`/`manual`) and `post_compact` adds `messages_removed`. Failures surface as
notices (or stderr after the session ends).

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
