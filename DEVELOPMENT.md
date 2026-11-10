# Whisker 开发与更新手册

最后更新：2026-11-10 · 当前版本：v0.6.1

这份文档记录 Whisker 的产品思路、代码地图、验证方法、换机恢复和发布流程。
目标是：任何一台机器 clone 仓库后，按本文即可继续开发和发布。

## 1. 核心思路

1. **Rust 做编排器，能力做侧车。** 核心只负责回合循环、工具调度、权限和上下文；
   浏览器、Blender、桌面控制等能力优先走 MCP sidecar，不轻易塞进核心。
2. **学的是 harness 节奏，不是提示词。** 回合由模型的 tool_use / finish 信号驱动；
   read-before-edit、审批、权限、plan 边界由代码强制，而不是靠提示词自觉。
3. **只融合被验证的行为。** 静态提取 + 真实 runtime 探针 + 交叉验证，
   证据放在 `docs/research/`。没有证据的行为不默认开启；不确定的先做 opt-in。
4. **不发明结束启发式。** 模型不调用工具/发出 finish 才结束；`max_turns` 是可选硬上限；
   `terminal_reason` 明确记录 complete / max_turns / max_tokens / interrupted / error。
5. **DeepSeek first，OpenAI-compatible。** 默认 `deepseek-flash` / `deepseek-v4-pro`；
   Qoder 外部模型路线已删除，不要恢复这种黑盒转发。
6. **UI 像 Claude，但内容是 Whisker 自己的。** 布局、状态词、tips、/model 设置参考
   Claude 的交互节奏；提示文案和行为以 Whisker 的真实能力为准。
7. **测试时机和成本由用户控制。** 密钥只放本地；用户说“不跑了”立即停止；
   不做未获批准的 HTTPS 中间人抓包或高成本探针。

## 2. 架构与代码地图

| Crate | 职责 | 关键文件 |
|---|---|---|
| `crates/core` | Agent 回合循环、上下文/提醒、权限、会话、子 agent、事件类型 | `agent.rs`（主循环/Stop/compact/tool search）、`context.rs`（10/10 提醒）、`policy.rs`（审批/plan）、`session.rs`、`subagent.rs`、`types.rs`（TerminalReason/Event） |
| `crates/llm` | OpenAI-compatible 流式 provider | `openai.rs`（SSE、tool calls、reasoning_content） |
| `crates/tools` | 原生工具 | `fs.rs`（read/write/edit/glob/grep）、`bash.rs`（tree-sitter 安全分析）、`todo.rs`、`skills.rs`、`lsp.rs`、`open_app.rs` |
| `crates/mcp` | MCP stdio / Streamable HTTP 侧车 | `lib.rs` |
| `crates/ui` | TUI 与行式 REPL | `tui.rs`（布局/spinner/tips/计时）、`lib.rs`（REPL/run_agent） |
| `crates/cli` | 参数、配置、启动、更新提醒、安装入口 | `main.rs`、`config.rs`、`update.rs` |

主链路：

```
CLI → Agent.run_turn → LLM stream → tool calls → policy/hooks → tools
    → tool results → 下一回合 … → TurnFinished(terminal_reason)
    → Event channel → TUI / REPL
```

关键路径：

- 配置：`~/.whisker/config.toml`
- 密钥：`~/.whisker/.env`（或 shell 环境 / 项目 `.env`）
- 权限记忆：`~/.whisker/permissions.allow`
- 模型参数记忆：`~/.whisker/model-settings.json`
- 会话：`~/.whisker/sessions/<id>/{state.json,events.jsonl,plan.md,...}`
- 更新检查缓存：`~/.whisker/update-check.json`
- 项目 skills：`.whisker/skills` 或 `.claude/skills`
- 项目指令：`AGENTS.md` / `CLAUDE.md`；全局 `~/.whisker/AGENTS.md`

## 3. 开发流程（每个功能都这样做）

1. **找证据。** 先读 `docs/research/` 里的 spec / crosscheck / 10-direction validation；
   新行为必须有静态或 runtime 证据，并在文档里写清楚不确定性。
2. **最小设计。** 能改现有路径就不加抽象；不确定的行为放在配置后面（opt-in），
   不改变默认体验。
3. **实现 + 单测。** 纯逻辑（阈值、版本比较、payload、匹配）必须有单测；
   配置项加解析测试；涉及 UI 的加渲染测试。
4. **真实 E2E。** 用 DeepSeek 真跑，不只看单测。推荐隔离 HOME：

```bash
mkdir -p /tmp/whisker-e2e/home/.whisker /tmp/whisker-e2e/work
cp ~/.whisker/.env /tmp/whisker-e2e/home/.whisker/.env
chmod 600 /tmp/whisker-e2e/home/.whisker/.env
HOME=/tmp/whisker-e2e/home ./target/debug/whisker --config ... -p "..." --output-format stream-json
rm -rf /tmp/whisker-e2e
```

不要打印密钥。UI 改动最好用 pty 或真实 Terminal 跑一遍；布局问题按用户要求截图确认。

5. **全量验证。** `cargo fmt --all`、`cargo check --workspace`、`cargo test --workspace`；
   可选 `cargo clippy --workspace --all-targets`。有真实服务参与的结论要写进提交说明或文档。
6. **落地。** 同步 README / `config.example.toml` / 本手册；安装 release；提交并推送。
   面向用户的改动按第 5 节发布。

**硬约束**（来自已验证的 Claude 行为）：

- 主循环由 finish/tool_use 信号驱动，不用启发式判断“做完了”。
- Stop hook 只在自然结束运行；到 max_turns 上限不运行。
- Pre-tool hook 先于权限检查；被拒绝的调用触发 pre，不触发 post。
- Todo 提醒是 10/10 回合策略，且只在 `todo_write` 可用时注入；阈值可配置。
- Compaction 默认 90% 上下文窗口，可配置比例或绝对窗口。
- Plan 模式是“不能改项目”，不是“不能用工具”；只读检查工具可用。
- 未经运行时验证的 P3（brief/无障碍/teammate tree）不要直接移植。

## 4. 换电脑恢复

系统要求：macOS 或 Linux、Rust stable（README 记录测试版本 1.96）、git、curl、python3。

```bash
git clone https://github.com/bugeng777/whisker.git
cd whisker
mkdir -p ~/.whisker
cp .env.example ~/.whisker/.env       # 填入 DEEPSEEK_API_KEY
cargo test --workspace
./scripts/install.sh                   # 源码构建，安装到 ~/.local/bin
whisker --version
```

可选：

```bash
cp config.example.toml ~/.whisker/config.toml
# 模型/上下文/thinking/effort 在会话内用 /model 配置并持久化
```

如果新机器只是使用、不开发，直接装 release：

```bash
curl -fsSL https://github.com/bugeng777/whisker/releases/latest/download/install.sh | bash
```

仓库自带研究资料：`docs/research/CLAUDE_HARNESS_SPEC.md`、
`docs/research/CLAUDE_HARNESS_CROSSCHECK.md`、
`docs/research/CLAUDE_10_DIRECTION_VALIDATION.md`（含实现状态表）。
`.env` 已在 `.gitignore` 中；只提交 `.env.example`。

## 5. 日常更新与发布

日常开发：

```bash
git pull --ff-only
cargo test --workspace
./scripts/install.sh
```

发布新版本：

1. 修改 `Cargo.toml` 中 `[workspace.package] version`（例如 0.6.1 → 0.6.2），
   运行 `cargo check --workspace` 刷新 `Cargo.lock`。
2. 更新 README / `config.example.toml` / 本手册中受影响的内容。
3. 提交并推送 `main`。
4. 打 tag 并推送，触发 `.github/workflows/release.yml`：

```bash
git tag -a vX.Y.Z -m "Whisker vX.Y.Z"
git push origin vX.Y.Z
```

5. workflow 构建 macOS arm64/x64、Linux x64/arm64，发布 tarball、`install.sh`、
   `SHA256SUMS`。验证：

```bash
gh run watch <run-id> --exit-status
gh release view vX.Y.Z --json assets
curl -fsSL .../install.sh | WHISKER_INSTALL_DIR=/tmp/whisker-check bash
```

发布规则：

- 不做自动更新。`crates/cli/src/update.rs` 每天最多检查一次 GitHub Releases，
  每个新版本只提醒一次；`update_check = false` 或 `WHISKER_DISABLE_UPDATE_CHECK=1` 关闭。
- 已发布 tag 不要移动；修复用新的 patch 版本。
- 目前只有 curl 原生安装；Homebrew tap、Windows 安装器尚未做。

## 6. 当前状态与后续

v0.6.1 已落地并验证：

- P0：terminal_reason、Stop block/续跑、max_turns 跳过 Stop、pre-before-permission、
  10/10 提醒、90% compaction、plan.md 边界。
- P1：thinking/thought-for、10s stall、工具/总耗时、tips 每轮轮换。
- P2：Session/Compact hooks、subagent 深度/回合/并发、opt-in deferred tools + tool_search、
  grep 显式文件修复。
- 发布：四平台 release、校验和安装脚本、每日更新提醒（不自动更新）。

残余（来自 `docs/research/CLAUDE_10_DIRECTION_VALIDATION.md`）：

- P3 brief/transcript/无障碍/teammate tree：仅静态证据，未移植。
- Anthropic 端点兼容：未跑（需要 key）。
- 1M compaction、第二次提醒周期、交互式 task 路径：未实测。
- Homebrew tap、Windows 安装器、macOS 签名/公证：未做。

下一步原则：先补证据再实现；新功能默认 opt-in；P3 不确定就继续留在文档里，
不要为了“像 Claude”而加入未验证的行为。

## 7. 安全与边界

- 永远不要提交或打印真实 API key；`.env`、`~/.whisker/.env` 只在本地。
- 不做未经批准的 HTTPS MITM；不花用户的钱跑未批准的高成本探针。
- 用户控制测试时机；“不跑了”就是立即停止。
- Qoder 外部模型转发已删除，不要重新引入。
- 发布、强推、移动 tag 这类共享状态操作先确认。
- 变更只提交到仓库内的文件；研究文档复制进 `docs/research/` 后再随仓库迁移。

## 8. 常用命令

| 目的 | 命令 |
|---|---|
| 格式化 | `cargo fmt --all` |
| 检查 | `cargo check --workspace` |
| 测试 | `cargo test --workspace` |
| 本地安装 | `./scripts/install.sh` |
| 单次运行 | `whisker -p "..." --cwd /path/to/project` |
| JSON 事件 | `whisker -p "..." --output-format stream-json` |
| 模型/参数 | 会话内 `/model` |
| 发布 | `git tag -a vX.Y.Z -m "Whisker vX.Y.Z" && git push origin vX.Y.Z` |
