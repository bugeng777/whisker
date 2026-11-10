# Claude Code 2.1.311 — Runtime Cross-Validation Round 2

Status: completed 2026-11-10. Same target and backend as Round 1
(Claude Code 2.1.311, DeepSeek `deepseek-v4-pro`, effort low). No model or
auth configuration was changed.

Purpose: use unrelated, different-type prompts to test whether the workflow
observed in Round 1 is task-specific or harness-invariant.

## Probes

All probes ran in `/tmp/claude-harness-probe2` with the same hook logger
(`UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`).

| Probe | Question type | Tools used | API turns | Stop / terminal | Result |
|---|---|---|---|---|---|
| QA | Correlation vs. causation, 3 bullets, no tools | none | 1 | `end_turn` / `completed` | direct answer |
| Bash | Run `printf ... \| wc -l` and `uname -s` | `Bash` ×2 (parallel) | 3 | `end_turn` / `completed` | `3`, `Darwin` |
| Plan | Read `notes.txt`, propose a 2-step plan | `Read`, `Bash` ×2, `Read`, `ToolSearch` ×2, `Write` (plan file) | 8 | `end_turn` / `completed` | plan document + summary |

Hook order observed:

- QA: `prompt -> stop`.
- Bash: `prompt -> pretool -> pretool -> posttool -> posttool -> stop`.
- Plan: `prompt -> (pre/post for Read, Bash, Bash, Read) -> stop`.
  `ToolSearch` and the plan-file `Write` were not matched by the
  `Read|Edit|Bash` hook matcher, so they correctly produced no tool hooks.

Plan mode wrote a plan file under `~/.claude/plans/...` (outside the project),
not `notes.txt`; the probe-created plan file was removed after inspection.
`permission_denials` was empty in all three probes. `ExitPlanMode` and
`AskUserQuestion` were not available through ToolSearch in this environment.

## Cross-Validation Against Round 1

Invariants confirmed:

- The loop is tool-use driven: no tools -> 1 API turn; two Bash tools -> 3
  turns; seven tool-use blocks -> 8 turns. Round 1's Read/Edit/Read produced 4.
- A tool result is a user-side `tool_result`; the harness sends another model
  request only while tool use/continuation requires it.
- `stop_reason=end_turn`, `terminal_reason=completed` for all direct completions.
- Hook order is per tool call, not per API turn; parallel tools can produce
  `pre, pre, post, post`. Stop fires after the turn.
- Context/model fields repeat: assumed `contextWindow=200000`,
  `maxOutputTokens=32000`, unknown-model warning; Round 1's effective
  auto-compact window was 180000 (90%).
- No reminder fired in these probes. Round 2 stayed below the hardcoded
  10-turn-since-write / 10-turn-between-reminders thresholds, and the current
  environment has no `TodoWrite` tool.
- Plan mode is not "no tools": it allows read-only inspection and shell queries,
  and permits a special plan-file write outside the project. It did not edit the
  project file.
- The optional max-turns behavior from Round 1 was not contradicted: default
  runs had no turn cap (plan reached 8); Round 1's `CLAUDE_CODE_MAX_TURNS=2`
  hard stop remains the verified cap behavior.
- Deferred-tool loading is query-dependent: Round 1 loaded up to 11/14 tools and
  found no todo tool; Round 2's plan-mode searches for `ExitPlanMode`/
  `AskUserQuestion` returned no usable tools.

Differences are task/tool driven, not harness contradictions:

- A no-tool Q&A completes in one model request.
- Parallel Bash calls can share one API turn and produce two pre/post hook pairs.
- Plan mode can spend many turns on inspection and plan authoring; it may use
  `ToolSearch` for plan-exit/question tools that are unavailable in `-p`.

## Evidence

- `/tmp/claude-re/runtime2/qa.jsonl`, `qa.debug.log`
- `/tmp/claude-re/runtime2/bash.jsonl`, `bash.debug.log`
- `/tmp/claude-re/runtime2/plan.jsonl`, `plan.debug.log`
- `/tmp/claude-re/runtime2/hooks-probe.log` (combined prompt/pre/post/stop payloads)
- Round 1: `/tmp/claude-re/runtime/{minimal,tools,hooks,maxturns,toolsearch,stophook}.jsonl`
- Canonical spec: `CLAUDE_HARNESS_SPEC.md` (same directory)

## Round 3 — >10 Turn Reminder Injection

The legacy todo reminder was not reachable in the default print-mode tool set.
Two flags controlled the path:

- `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` exposed `TodoWrite`.
- `CLAUDE_CODE_ENABLE_TASKS=false` forced the legacy `Jwr()` todo path. Setting
  the value to `0` was insufficient: the harness still selected the task branch
  (`eTr()`), which requires `TaskUpdate`; no reminder was injected.

Probe: initial `TodoWrite` (2 items), then exactly 12 sequential Bash turns
(increment `counter.txt`), no TodoWrite updates until the end. Final counter 12.

Observed:

- `num_turns=16`; final `stop_reason=end_turn`; cost $0.2514.
- TodoWrite at assistant/tool positions 4 and 30; 12 Bash steps between them.
- A `todo_reminder` attachment appeared after the 10th Bash turn (transcript
  entry 77, assistant index 24). The next API request carried the hardcoded
  reminder text (“The TodoWrite tool hasn't been used recently…”) plus the
  current todo list.
- The model continued steps 11–12, marked both todos complete, and replied DONE.
- The attachment and reminder text were present in the **persisted transcript**;
  `stream-json` did not include the `todo_reminder` attachment.

This confirms the hardcoded 10-turn-since-write threshold and the legacy todo
reminder injection path. The previous long run with `CLAUDE_CODE_ENABLE_TASKS=0`
produced no reminder; that is a gate-selection difference, not a threshold
contradiction.

Evidence: `/tmp/claude-re/runtime2/longrem2.jsonl` + `.debug.log`;
`~/.claude/projects/-private-tmp-claude-harness-probe2/0b5e7951-978b-473b-b732-1c657afa4ff6.jsonl`
(attachment entry 77).

## Round 4 — 10-Direction Validation

A visible-Terminal run covered 10 distinct directions: no-tool Q&A, read-only,
Bash, edit+verify, plan mode, TodoWrite reminder, Stop hook, max-turn cap,
permission denial + 1M context, and subagent delegation. Key cross-checks:

- Loop counts scaled as expected (1/2/3/4/9/16 turns), all direct completions
  `end_turn`/`completed`.
- Hooks stayed per-tool; parallel tools produced `pre,pre,post,post`.
- Reminder 10/10 fired again (one `todo_reminder` at the 10th Bash turn).
- Max-turn cap produced `terminal_reason=max_turns` but `num_turns=3` for env 2;
  the display count is not the cap counter, and Stop hooks did not run.
- Permission-denied Edit produced a `permission_denials` entry, no PostToolUse,
  and left the file unchanged.
- Context override reported 1M but did not suppress the unknown-model warning.
- Plan mode wrote only a `~/.claude/plans/...` plan file; subagent delegation
  spawned/completed one general-purpose agent.

Full matrix, costs, and the Whisker integration gate:
`CLAUDE_10_DIRECTION_VALIDATION.md`.

## Residual Unknowns

- Interactive (non-`-p`) default TodoWrite behavior; this probe required the
  TODO_TOOLS launch flag.
- The second half of the 10/10 policy (ten turns between reminders) was not
  separately exercised; only one reminder occurred.
- Interactive plan approval UI, 1M-model compaction, and subagent/teammate
  execution remain unprobed.
- Exact Stop-hook mutation precedence and remote feature-flag behavior remain
  static/unknown; no HTTPS interception was used.
