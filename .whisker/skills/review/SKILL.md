---
name: review
description: Review the current diff for correctness, regressions and missing tests
---

1. Run `git diff` (and `git diff --staged` if there are staged changes) to see what changed.
2. Read the surrounding code for every changed hunk; do not review the diff in isolation.
3. Check for: logic errors, unhandled edge cases, changed public behavior, missing tests,
   and inconsistencies with existing conventions.
4. Verify with a build or test command when the change warrants it.
5. Report findings ordered by severity, each with a file path and line number. If there are
   no findings, say so explicitly. Do not modify files unless the user asked for fixes.
