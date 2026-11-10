# Claude Code 2.1.311 — Harness & Workflow Spec (Behavior-Level)

Status: draft v1, 2026-11-10. Target: Homebrew Cask `claude-code 2.1.311`
(`/opt/homebrew/Caskroom/claude-code/2.1.311/claude`, SHA-256
`85d8c08707fcfd1e78ffa0204eefbc6ec9e4fe22f375fb2767ef85d957ed3161`).
Runtime backend for probes: DeepSeek `deepseek-v4-pro` through CC Switch.

Evidence labels:

- **Verified** — observed in a controlled runtime probe.
- **Static** — found in the extracted minified JS bundle; not necessarily executed.
- **Inferred** — derived from a combination of static/runtime facts and arithmetic.
- **Unknown** — remote flag, bytecode-only, or untested path.

No HTTPS interception was performed. No original source map is present
(`with_sourcemap: 0`); static evidence is minified JS recovered from the Bun
bundle, not original TypeScript. Extracted source is kept only under `/tmp` and
is not redistributed.

## 1. Runtime Evidence

Probes ran in `/tmp/claude-harness-probe` with `-p`, `--output-format stream-json
--verbose`, `--effort low`, and `--permission-mode acceptEdits`.

- Minimal probe: one user prompt, no tools. Result `stop_reason=end_turn`;
  model reported `deepseek-v4-pro`; context window 200000; max output 32000;
  unknown-model warning on stderr.
- Tool probe: one user prompt caused **4 API round-trips** and 3 tools:
  `Read -> Edit -> Read`, then a final `DONE` message. Result
  `stop_reason=end_turn`, `num_turns=4`; file was edited.
- Hooks probe: exact order observed:
  `UserPromptSubmit -> (PreToolUse -> PostToolUse) per tool -> Stop`.
  Stop payload included `stop_hook_active`, `last_assistant_message`,
  `background_tasks`, `session_crons`.
- Max-turns probe with `CLAUDE_CODE_MAX_TURNS=2`: only `Read + Edit` ran; result
  `stop_reason=tool_use`, `terminal_reason=max_turns`, exit 1, no final answer.
- ToolSearch probe: main prompt saw 14 deferred tools; searches loaded 8/14 then
  11/14. Loaded names included `CronList`, `TaskStop`, `EnterWorktree`,
  `SendMessage`, `CronCreate`, `DesignSync`, `PushNotification`, `WebSearch`,
  `ExitWorktree`, `LSP`, `Monitor`; no `TodoWrite`/task-list tool was discoverable
  in this print-mode environment; model replied `NO_TODO_TOOL`.
- Stop-hook probe: the first `{"decision":"block","reason":"..."}` from a Stop
  hook prevented the turn from ending, produced a second API turn, and the next
  Stop without block let the turn finish. Result `num_turns=2`.
- Real transcript scan (user sessions): tools observed were `Edit`, `Bash`,
  `Read`, `Write`; no todo/task tool calls were present in those sessions.

Evidence files: `/tmp/claude-re/runtime/{minimal,tools,hooks,maxturns,toolsearch,stophook}.jsonl`,
matching `*.debug.log`, and `/tmp/claude311-inspect.json`.

## 2. Agent Loop

**Verified / Static:**

- One user prompt starts a user turn; each model API request is an agentic turn.
- Assistant tool-use blocks set the continue flag:
  `if (toolUseBlocks.length > 0) { needsFollowUp = true; ... }`
  (`claude311-js.txt:17173`).
- Tool results are returned as user-side `tool_result` messages; the next API
  request is made if `needsFollowUp` remains true.
- The runtime tool probe shows the model deliberately continued after each
  tool result and ended only after the final assistant message.
- Exit conditions include no tool use/`end_turn`; runtime result
  `stop_reason=end_turn`, `terminal_reason=completed`.
- Other loop flags (static): `toolRequestedEndTurn`, `shouldPreventContinuation`
  (hook-driven), `toolWasDeferred`.
- A Stop hook returning `{"decision":"block","reason":"..."}` is a real
  continuation gate: it prevented the first stop, produced another API turn, and
  a later non-blocking Stop finished the run. The debug log reported the hook
  result as a permission denial with the supplied reason.
- `maxTurns` is an optional agent/subagent setting; `CLAUDE_CODE_MAX_TURNS`
  provides a process-level cap, parsed as a positive integer (`claude311-js.txt:333`).
  With value 2, the runtime stopped before the task finished and returned
  `terminal_reason=max_turns`; the result can still report `num_turns=3` because
  displayed turns are not the cap counter. Treat `terminal_reason` as the
  authoritative stop signal and implement an explicit round-trip counter.
- Retry/fallback paths exist (overloaded/server_error, fallback chains, request
  watchdogs). These are static in this pass; no forced failure was generated.
- Stop hooks run after the turn; the Stop/StopFailure payload includes
  `stop_hook_active`, `last_assistant_message`, `background_tasks`, and
  `session_crons`.

**Whisker implication:** keep the finish-signal loop; add an explicit
`terminal_reason`/max-turn result and make hook continuation a first-class gate.

## 3. Tool Pipeline

**Verified / Static:**

- Main print-mode tool set observed: `Task`, `Bash`, `Cron*`, `DesignSync`,
  `Edit`, `EnterWorktree`/`ExitWorktree`, `ListAgents`, `LSP`, `Monitor`,
  `NotebookEdit`, `PushNotification`, `Read`, `ReportFindings`,
  `ScheduleWakeup`, `SendMessage`, `Skill`, `TaskStop`, `ToolSearch`,
  `WebFetch`, `WebSearch`, `Workflow`, `Write`.
- 14 tools were deferred; `ToolSearch` loads them dynamically. Runtime searches
  increased the loaded set from 0/14 to 8/14 and 11/14; loaded names included
  Cron/task/worktree/messaging/web/LSP/monitor tools, not TodoWrite.
- No `TodoWrite`/TodoRead was available in this environment; real interactive
  transcripts also showed no todo tool calls.
- Tool order: assistant `tool_use` -> PreToolUse hook -> permission/safety ->
  execute -> PostToolUse hook -> `tool_result` user message -> next API turn.
- Tool results include structured metadata; Edit returns a structured patch and
  the message “file state is current in your context — no need to Read it back”.
- Permission modes observed/in setting docs: default/acceptEdits/plan/auto;
  `--allowedTools` auto-approves listed tools. A Round 4 Edit under `dontAsk`
  produced a `permission_denials` entry with the full input and left the file
  unchanged; PreToolUse ran, PostToolUse did not.
- Tool concurrency and streaming are managed by a streaming tool executor;
  static env: `CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY`.

**Unknown:** exact read-before-edit enforcement for every tool; tool-specific
retry semantics; bytecode-only paths.

## 4. Reminders and Task Tracking

**Static:**

- Legacy todo path requires the `TodoWrite` tool:
  `Jwr()` returns a `todo_reminder` only when `TodoWrite` is in `options.tools`.
- Thresholds are hardcoded: `EEe = { TURNS_SINCE_WRITE: 10,
  TURNS_BETWEEN_REMINDERS: 10 }` (`claude311-js.txt:11742`).
- Todo reminders are gated by `CLAUDE_CODE_TODO_REMINDER_MODE` and feature flag
  `tengu_soft_slate_nudge`; brief mode suppresses the legacy reminder path.
- A parallel task-management path emits `task_reminder`; it is gated by a task
  tool (`lA`/`Zk` minified refs) and a feature gate (`dM()`), with the same
  10/10 thresholds.
- Reminders are injected as attachments/user-visible `<system-reminder>` content;
  other branches include verify/no-verification, plan, output-limit, and total
  token reminders.

**Verified (Round 3, 2026-11-10):** with `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`
and `CLAUDE_CODE_ENABLE_TASKS=false`, a 12-step sequential Bash task with an
initial `TodoWrite` produced a `todo_reminder` after the 10th Bash turn. The
next API request contained the reminder text; the model continued to step 12 and
finished with a second `TodoWrite`. Result `num_turns=16`, cost $0.2514.
`CLAUDE_CODE_ENABLE_TASKS=0` did **not** disable the task branch (it still
selected the task reminder path, where `TaskUpdate` was absent); use `false` to
force the legacy todo path. The reminder attachment was present in the persisted
transcript but absent from `stream-json` output.

**Verified:** earlier short probes produced no todo reminder because no todo
tool was available; the hook probe produced the normal hook sequence, not todo
reminders.

**Inference:** the 10/10 policy means a reminder can appear only after ten agentic
turns without a todo write and ten turns since the previous reminder. The
Round 3 run confirms the first half of that policy; the second half follows from
the same `Xwr()` state counter and is not separately exercised.

## 5. Hooks

**Static event list:** PreToolUse, PostToolUse, PostToolUseFailure, PostToolBatch,
Notification, UserPromptSubmit, UserPromptExpansion, SessionStart, SessionEnd,
Stop, StopFailure, SubagentStart, SubagentStop, PreCompact, PostCompact,
PreModelSwitch, PostModelSwitch, PermissionRequest, PermissionDenied, Setup,
TeammateIdle, TaskCreated, TaskCompleted, Elicitation, ElicitationResult,
ConfigChange, WorktreeCreate, WorktreeRemove, InstructionsLoaded, CwdChanged,
FileChanged, DirectoryAdded, MessageDisplay (`claude311-js.txt:758`).

**Verified ordering:** `UserPromptSubmit` -> `PreToolUse` -> `PostToolUse` (per
tool) -> `Stop`. Hook payloads contain session id, transcript path, cwd, prompt
id, permission mode, effort, tool name/input/result, duration, and stop context.

**Verified edge cases (Round 4):** a max-turn termination did **not** run the
Stop hook; a permission-denied Edit ran PreToolUse but not PostToolUse, and the
denial appeared in `permission_denials` with the full tool input.

**Verified Stop gate:** `{"decision":"block","reason":"..."}` from a Stop hook
blocked the end of the turn and caused another model turn; the next Stop without
block let it finish. The debug log records the first as a permission denial with
the hook reason.

**Static hook contract:** PreToolUse can allow/deny/defer, update input, add
context, or prevent continuation; Stop/StopFailure can block/continue; hooks can
be command/exec/async with shell selection, timeout, and failure policy
(`continue` default, `block` for exit code 2/validation failures). Stop hooks are
not run for async hooks on Stop/SubagentStop/TaskCompleted/TeammateIdle.

## 6. Context and Compaction

**Verified:** with the unknown DeepSeek model, Claude Code assumed a 200000-token
window and auto-compacted at an effective window of 180000 tokens (90%). The
result payload reported `contextWindow=200000`, `maxOutputTokens=32000`.

**Static/known env:**

- `CLAUDE_CODE_MAX_CONTEXT_TOKENS` overrides the assumed window for an
  unrecognized/custom model. Round 4 confirmed the reported window becomes
  1,000,000, but the unknown-model warning still appeared on stderr. The docs'
  “warning disappears” claim is not reliable for this version/backend; Whisker
  should own its warning policy.
- `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT=1` disables proactive
  unknown-model window enforcement.
- `autoCompactWindow` is configurable per model (100k–1M); 1M-native models use
  their native window; `[1m]` suffix opts into 1M.
- Compaction has PreCompact/PostCompact hooks and produces a summary; the exact
  summary prompt was not runtime-captured in this pass.

**Inference:** the 180k/200k ratio matches a 90% auto-compact threshold for this
runtime. Do not generalize the 90% to 1M models without a dedicated probe.

## 7. Models and Effort

**Static:** the model registry includes Fable 5.2, Opus 5.5, Sonnet 5.5, and
Haiku 5.5 families, with per-model context (native 1M), max output (up to 128K),
capabilities (`effort`, `max_effort`, `xhigh_effort`, `adaptive_thinking`,
`fast_mode`, `context_management`, etc.), and default effort `high`. Per-model
`modelSettings` stores effort and `autoCompactWindow`; `fast_mode` is off under
the SDK unless opted in.

**Verified:** the probe used `--effort low`; result `fast_mode_state=off`,
`fast_mode_disabled_reason=sdk_opt_in_required`; effort was propagated in hook
payloads (`effort: {level: "low"}`). Unknown model names fall back to 200k and
produce a warning unless the context env is set.

## 8. Subagents, Teammates, Background

**Static:** tools `Task`, `TaskStop`, `ListAgents`, `SendMessage`, `Workflow`,
`Monitor`, `CronCreate/CronList/CronDelete`; subagent result stats include
spawned/requested/background/completed/failed/killed/refused by depth,
concurrency and budget; `maxTurns` and per-agent context windows; background
sessions (`--bg`, `logs`, `attach`, `stop`). Spinner/task-tree UI exists for
teammates.

**Verified (Round 4):** a Task/Agent delegation spawned one `general-purpose`
subagent, completed one, max depth 1, with tools reported as `Agent` + `Read`;
the main run took 2 turns. Depth/concurrency/budget refusals are still not
stress-tested.

### Plan mode (Round 2/4)

**Verified:** `--permission-mode plan` allowed `Read`/`Bash` inspection and
ToolSearch; it wrote only a plan artifact under `~/.claude/plans/...` and left
the project file unchanged. Round 2 took 8 turns; Round 4 took 9 turns and used
Read/Bash/Write/ToolSearch. Plan mode is “no project edits”, not “no tools”.
Plan-file naming/policy is model-chosen and should not be copied as a fixed
Whisker path without a separate design.

## 9. UI and Status Style

**Static/runtime fragments:**

- Spinner verbs are a fixed list; `spinnerVerbs` supports append/replace custom
  verbs; random verb is chosen at mount and task activeForm has priority.
- Spinner tips: `spinnerTipsEnabled`, `spinnerTipsOverride`
  (`excludeDefault`, `tips`, `tipsFile`, `label`); tip picked once per turn,
  session relevance/cooldown/priority; default prefix “Tip”.
- Stall telemetry exists (`[Stall] tool_dispatch_start/end`, permission latency,
  duration); stalled spinner UI/telemetry events (`tengu_spinner_stalled_ui`,
  `tengu_spinner_stall_cleared`).
- Thinking status has a minimum 2s display, duration/token/effort suffix.
- Key bindings: `ctrl+t` todos, `ctrl+o` transcript, `ctrl+shift+b` brief,
  `ctrl+r` search; `shift+tab` cycles permission modes (platform variant uses
  meta+m); `--ax-screen-reader` gives flat, non-decorative output.
- Model picker supports `availableModels`, `modelPicker` lineups,
  `modelSettings`, `modelOverrides`; aliases and display names are separate from
  provider IDs.

## 10. Whisker Gap Analysis

| Area | Claude 2.1.311 | Whisker today | Gap / action |
|---|---|---|---|
| Loop exit | finish/tool-use driven; terminal_reason; optional maxTurns; hook continuation gate | finish_reason driven; optional `max_turns`; no terminal_reason; no hook stop gate | Add terminal reason and Stop-hook continuation control |
| Compaction | 90% effective window (180k/200k in probe); per-model autoCompactWindow | fixed `COMPACT_RATIO = 0.7` | Make ratio/window configurable; consider 90% default |
| Reminders | todo 10/10 or task 10/10; feature-gated | todo/verify/plan reminders; no task path | Add task-style reminder path and configurable thresholds |
| Tools | 25 visible + 14 deferred; ToolSearch; streaming executor | native + MCP; no deferred search | Consider deferred tools + ToolSearch |
| Hooks | 30+ lifecycle events; permission/input/stop mutation | pre/post tool hooks only | Add Stop, PermissionRequest/Denied, Session, Compact hooks |
| Subagents | Task/teammates/background/budget stats | subagent tool; no teammate/background | Lower priority |
| Models/effort | registry with capabilities, per-model effort/context, fast mode | per-model settings; no capability registry; no fast mode | Add capability metadata and fast-mode opt-in if desired |
| UI | spinner verbs + tips + stall + thinking duration + brief/accessibility | spinner verbs + todo priority; no tips/stall/thinking duration | Add tips/stall/thinking status next |

## 11. Priority Backlog

1. **P0 — harness correctness:** configurable compaction ratio/window; Stop-hook
   continuation; terminal reason/max-turn reporting; verify no-tool reminder path.
2. **P1 — visibility:** spinner tips, stall/elapsed status, thinking duration;
   per-model context/effort already started in Whisker.
3. **P2 — extensibility:** deferred tools/ToolSearch; expanded hook events;
   background/subagent budgets.
4. **P3 — presentation:** brief/transcript mode, accessibility flat mode,
   teammate spinner tree.

## 12. Cross-Validation Round 2

Three unrelated probes were run with the same DeepSeek backend: Q&A (1 API
turn), Bash task (3 turns, parallel Bash calls), and plan mode (8 turns).
Findings and evidence are recorded in
`CLAUDE_HARNESS_CROSSCHECK.md`; the key invariants (tool-driven loop, per-tool
hook order, end_turn completion, 200k assumed context, optional max turns) held,
while tool counts and plan-mode behavior varied by task.

## 13. Unknowns / Next Probes

- Interactive (non-`-p`) tool set and TodoWrite/task behavior; print mode omitted
  the todo tool unless `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` was set.
- Compaction trigger and summary prompt at 1M/other models; only 200k assumed
  runtime was observed.
- The second half of the 10/10 reminder policy (10 turns between reminders) was
  not separately exercised.
- Subagent depth/concurrency/budget refusals were not stress-tested.
- Subagent/teammate spawning, depth/concurrency/budget refusals.
- Remote feature flags (GrowthBook) and bytecode-only modules.
- Exact HTTP request bodies/system prompt; a local MITM proxy would be required and
  is intentionally not enabled.

## Appendix — Evidence Index

- `/tmp/claude-re/runtime/minimal.jsonl` — no-tool baseline.
- `/tmp/claude-re/runtime/tools.jsonl` — 4-turn Read/Edit/Read loop.
- `/tmp/claude-re/runtime/tools.debug.log` — engine turns, autocompact 180k, stall timing.
- `/tmp/claude-re/runtime/hooks.jsonl` + `hooks.log` — hook order/payloads.
- `/tmp/claude-re/runtime/maxturns.jsonl` — `CLAUDE_CODE_MAX_TURNS=2`.
- `/tmp/claude-re/runtime/toolsearch.jsonl` + `.debug.log` — deferred tools 0/14, 8/14, 11/14.
- `/tmp/claude-re/runtime/stophook.jsonl` + `.debug.log` — Stop block/continue, num_turns=2.
- `/tmp/claude-re/runtime2/{qa,bash,plan}.jsonl` + debug logs — different question types and plan mode.
- `/tmp/claude-re/runtime2/longrem2.jsonl` + `.debug.log` — 12-step run, 16 turns, 10/10 reminder.
- `~/.claude/projects/-private-tmp-claude-harness-probe2/0b5e7951-978b-473b-b732-1c657afa4ff6.jsonl` — persisted transcript containing the `todo_reminder` attachment and reminder text.
- `/tmp/claude-re/runtime2/hooks-probe.log` — per-tool hook order across three probes.
- `CLAUDE_HARNESS_CROSSCHECK.md` — Round 2/3 comparison and residuals.
- `CLAUDE_10_DIRECTION_VALIDATION.md` — Round 4: 10-direction validation and Whisker integration gate.
- `/tmp/claude-re/runtime3/` — Round 4 summary, JSONL, debug and hook logs.
- `/tmp/claude311-js.txt` — extracted minified JS; cited lines are bundle lines.
- `/tmp/claude311-inspect.json` — 2,615 modules, 2,866 graph files, 0 source maps.
