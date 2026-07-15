---
name: pr-gates
description: This skill should be used when the user asks to "open a PR", "prepare for review", "address review comments", "run gates", or "verify before pushing" in this repository. Enforces preflight/gate workflow, migration safety, and review-evidence closure.
---

# PR Gates

## Mandatory Flow

1. Run `just preflight` before finalizing changes.
2. Run `just gate-pr` before pushing or updating a PR.
3. If the same command fails twice in one session, stop rerunning and switch to root-cause debugging.
4. Do not push when any gate is red.

## Review Feedback Closure

For every P1/P2 review finding, include all three:

- Code change reference (file path and concise rationale)
- Targeted verification command
- Pass/fail evidence from that command

## Async And Stateful Changes

When touching worker lifecycle, cancellation, retries, state transitions, or caches:

- Document terminal states and allowed transitions.
- Explicitly reason about race windows and idempotency.
- Run targeted tests in addition to broad gate runs.
- Capture the exact command proving the behavior.

## Commit Hygiene — Never Sweep Sibling Edits (issue #224)

Workers now run in an isolated per-worker `git worktree` (see
`src/agent/worker_workspace.rs`), but keep these rules as a second layer so a
commit can never contain files this worker did not touch:

- Stage **explicit paths** you changed: `git add <path> [<path> ...]`. Never
  `git add -A` or `git add .` — a blind sweep can pick up unrelated content.
- Verify a **clean, intentional** stage before committing: run
  `git status --porcelain` and confirm every staged path is one you edited.
- If the working tree is unexpectedly dirty at handout (files you did not
  create), stop and investigate rather than committing over them.

## Migration Safety

- Never edit an existing file in `migrations/`.
- Add a new timestamped migration for every schema change.
- If a gate flags migration edits, stop and create a new migration file.

## Handoff Format

- Summary
- Changed files
- Gate commands executed
- P1/P2 finding-to-evidence mapping
- Residual risk
