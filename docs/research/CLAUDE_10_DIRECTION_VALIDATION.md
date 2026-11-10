# Claude Code 2.1.311 — 10-Direction Validation & Whisker Integration Gate

Date: 2026-11-10. Target: Claude Code 2.1.311 native package. Backend:
DeepSeek `deepseek-v4-pro`, effort low. No model/auth configuration was changed.
The 10 probes ran in a visible macOS Terminal window; the same window was kept
open for review. Raw logs: `/tmp/claude-re/runtime3/`.

Cost: **$1.2525 total** for 10 directions. Each probe used the current CC Switch
DeepSeek profile; only per-probe harness env flags were added where stated.

## 1. Direction Matrix

| # | Direction | Tools observed | API turns | Terminal reason | Key result |
|---|---|---|---:|---|---|
| 01 | Pure reasoning Q&A, no tools | none | 1 | completed | answered directly; `end_turn` |
| 02 | Read-only file analysis | Read | 2 | completed | read `notes.txt`; no edit |
| 03 | Bash system info | Bash ×2 | 3 | completed | `wc -l` → 2, `uname -s` → Darwin |
| 04 | Edit + shell verification | Read, Edit, Bash | 4 | completed | appended `validated`; `cat` verified |
| 05 | Plan mode proposal | Read, Bash, Read, Bash, Write, Bash, ToolSearch ×2 | 9 | completed | plan file written; project file unchanged |
| 06 | TodoWrite + 10-turn reminder | ToolSearch, TodoWrite, Bash ×12, TodoWrite | 16 | completed | one `todo_reminder`; final count 12 |
| 07 | Stop-hook block/continue | none | 2 | completed | block once, then final turn |
| 08 | `CLAUDE_CODE_MAX_TURNS=2` | Bash ×2 | 3 | **max_turns** | `subtype=error_max_turns`, exit 1 |
| 09 | Permission denial + 1M context | Read, Edit (denied) | 3 | completed | Edit denied; file unchanged; context=1,000,000 |
| 10 | Subagent/Task delegation | Agent, Read | 2 | completed | 1 general-purpose subagent spawned/completed |

Hook order remained consistent: prompt → per-tool Pre/Post → Stop. Parallel
tools showed `pre,pre,post,post`. The max-turn probe had no Stop hook; the
permission-denied Edit had PreToolUse but no PostToolUse.

## 2. Comparison With Earlier Rounds

Confirmed invariants:

- The loop is tool-driven. Across 1, 2, 3, 4, 9, and 16-turn runs, it stops only
  on no tool use/`end_turn`; tools cause continuation.
- Q&A without tools is one API turn; Read is two; two parallel Bash tools are
  three; Read/Edit/Bash is four; plan and reminder scale with tool use.
- Hooks are per tool, not per API turn; a Stop hook can block and add a turn.
- Stop hook does **not** run when the turn ends with `terminal_reason=max_turns`.
- Permission-denied tools still trigger PreToolUse; PostToolUse is absent, and the
  denial is surfaced in `permission_denials` with the exact tool input.
- The legacy todo path still injects one reminder after 10 turns without a todo
  write, when `TodoWrite` is exposed and `CLAUDE_CODE_ENABLE_TASKS=false`.
- Unknown-model context defaults to 200k; result payload reports 32k max output.
- `CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000` changes the reported context window to
  1M, but the unknown-model warning **still appeared on stderr**. Do not promise
  warning suppression from the override alone.

New / corrected observations:

- **Max-turn semantics are not `num_turns`.** With env value 2, the result had
  `num_turns=3`, `stop_reason=tool_use`, `terminal_reason=max_turns`. The cap is
  enforced (error subtype), but the counter can exceed the configured value
  depending on turn accounting and batching. Whisker must implement its own
  explicit round-trip cap and reason, not derive it from a displayed turn count.
- **Plan mode is not read-only.** It used Read/Bash, wrote only a special plan
  file under `~/.claude/plans/notes-txt-notes-txt-steady-moler.md`, and left the
  project file unchanged. It also searched for `ExitPlanMode` via ToolSearch.
  The probe-created plan file was removed after inspection.
- **Context override is only partially a fix.** It changes the effective window
  but does not remove the unknown-model warning in this version/backend.
- **Subagent tooling is viable.** The main tool call appeared as `Agent`; the
  result reported `spawned=1`, `completed=1`, `max_depth=1`,
  `by_type={"general-purpose":1}`.
- The todo reminder still requires the harness flags; the default print-mode tool
  set does not expose `TodoWrite`.

## 3. Whisker Target Validation

| Whisker target | Evidence | Verdict / cautious action |
|---|---|---|
| Tool-driven loop + explicit terminal reason | 1/2/3/4/9/16-turn probes | Port; keep finish-signal loop, add `terminal_reason` |
| Max-turn cap | env=2 → `max_turns`, `num_turns=3` | Implement a separately counted round-trip cap; do not equate with display turns; define Stop-hook behavior explicitly |
| Hook ordering + Stop gate | hooks logs; Stop block; max-turn no Stop | Add Stop/Permission hooks carefully; skip Stop on cap; preserve Pre-before-denial semantics |
| Permission denial feedback | Edit denied, file unchanged, denial payload | Surface tool name/input/reason to the model; do not run PostToolUse on denial |
| Reminder 10/10 | one `todo_reminder` after 10th Bash turn | Make thresholds configurable; keep tool/feature gating; test task path separately |
| Context/compaction | 200k assumed, 1M override works, warning remains | Add explicit window/cap config and own warning; verify compaction thresholds per model; do not copy docs assumptions |
| Plan mode | 9 turns, plan-file Write, Bash/ToolSearch | Treat as “no project edits”, not “no tools”; add a dedicated plan artifact path and tests |
| Subagent/teammate | Agent spawn, stats, completed 1 | Add depth/concurrency/budget limits; keep behind opt-in; add tests before exposing |
| Spinner/status/tips | previous rounds only | Not re-tested here; keep prior evidence and UI tests |

## 4. Recommended Integration Order (Cautious)

1. **Harness semantics first:** finish/tool-use loop, `terminal_reason`, a clearly
   defined max-round-trip cap, context-window config, Pre/Post/Stop hook ordering.
   Add unit tests for denial and cap paths before TUI exposure.
2. **Feedback/status:** 10/10 reminder policy as a configurable rule; spinner
   tips/stall/thinking duration from prior evidence; do not make reminders noisy.
3. **Optional capabilities:** plan artifact handling, then subagent with explicit
   depth/concurrency/budget limits. Keep both behind feature gates.
4. **Compatibility audit:** verify behavior against both DeepSeek and Anthropic;
   the 200k/1M and warning findings are backend-specific. No MITM or source-map
   claims.

## 5. Residual Unknowns / Risks

- Interactive (non-`-p`) default TodoWrite/task behavior is still unprobed; the
  todo probe required the TODO_TOOLS flag.
- Only the first 10/10 reminder condition was exercised; the “10 turns between
  reminders” half needs a second reminder cycle.
- One plan-mode run is not enough to generalize the exact plan-file policy.
- Subagent depth/concurrency/budget refusals were not stress-tested.
- 1M-model compaction, per-model auto-compact windows, and raw HTTP requests are
  unverified; no HTTPS interception was performed.
- The visible terminal session and probe files are test artifacts, not Whisker
  changes. No Whisker code was modified in this validation.

## 6. Whisker Implementation Status (2026-11-10)

All items below are implemented in the working tree, covered by the workspace
test suite (82 tests), and installed as a release binary. Evidence is from the
current Whisker source plus real DeepSeek runs, not from Claude probes.

| Gate item | Status | Implementation | Verification |
|---|---|---|---|
| terminal_reason / max-turn / Stop gate | Done | `core/types.rs`, `core/agent.rs` | E2E block→continue, cap skips Stop |
| Pre-hook before permission, no Post on denial | Done | `core/agent.rs` | E2E denied bash fires pre only |
| 10/10 todo reminders, configurable | Done | `core/context.rs`, `cli/config.rs` | Unit tests + 1/1 E2E |
| 90% compaction, configurable window | Done | `core/agent.rs`, `cli/config.rs` | 7-message compaction E2E |
| Spinner thinking/stall/tool timers | Done | `ui/tui.rs` | Unit/render tests + live pty |
| Spinner tips rotation | Done | `ui/tui.rs` | Rotation/render tests + live pty |
| Lifecycle hooks (Session/Compact) | Done | `core/agent.rs`, `cli/main.rs` | E2E ordering and payloads |
| Subagent depth/turns/concurrency | Done | `core/subagent.rs`, `cli/main.rs` | Concurrency tests + live 3-turn cap |
| Deferred tools + ToolSearch (opt-in) | Done | `core/agent.rs`, `cli/config.rs` | Discover→read E2E |
| Grep on explicit file paths | Fixed | `tools/fs.rs` | Unit test + live `grep Cargo.toml` |
| Plan mode / artifact | Verified | `core/policy.rs`, session `plan.md` | Policy tests + live plan run; project file unchanged |

Residuals: no Anthropic-endpoint compatibility run (no key); 1M compaction and
the second reminder cycle remain unprobed; P3 brief/accessibility modes are
static-only and intentionally not ported. The changes are uncommitted.

## Evidence

- `/tmp/claude-re/runtime3/summary.json` — machine-readable results.
- `/tmp/claude-re/runtime3/summary.md` — human-readable probe summaries.
- `/tmp/claude-re/runtime3/01_*.jsonl` … `10_*.jsonl`, matching debug/hook logs.
- `/tmp/claude-re/runtime3/06_todo_reminder` transcript:
  `~/.claude/projects/-private-tmp-claude-10-probe/b242bdd8-5247-4921-8f26-6149bb73617d.jsonl`.
- Prior canonical spec: `CLAUDE_HARNESS_SPEC.md`; prior cross-check:
  `CLAUDE_HARNESS_CROSSCHECK.md`.
