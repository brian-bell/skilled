# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->


## Project Status

Skilled is an early Rust 2024 and Ratatui terminal application for inspecting
and eventually managing global coding-agent skills. The current implementation
covers the first vertical slice only: first-run setup, agent detection and
selection, SQLite-backed setup persistence, an empty Inventory view, Settings
setup reset, responsive size handling, and guarded terminal restoration.

Source registration, installation inventory, Doctor findings, and all
filesystem or Git mutation workflows are not implemented yet. Do not turn the
current wizard placeholders into behavior unless the active Beads issue places
that work in scope. [GitHub issue #3](https://github.com/brian-bell/skilled/issues/3)
is the product and technical source of truth. The tracked
`spec/tui-prototype.html` is the visual design reference.

## Build and Test

The project requires stable Rust 1.97 or newer.

```bash
cargo run
cargo build --release
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Tests use temporary homes and application-data directories. Never point a test
at the real user home or real agent skill roots. Ratatui layouts are verified
with Insta snapshots under `tests/snapshots/`.

## Application Architecture

- `src/app.rs`: application state, actions, pure reducer transitions, and typed
  persistence effects.
- `src/agents.rs`: Claude Code, Codex, and OpenCode adapters plus non-executing
  root and executable detection. Agent path conventions and documentation
  snapshots belong here.
- `src/store.rs`: private versioned SQLite metadata and migrations. Newer
  unknown schemas fail closed.
- `src/tui.rs`: pure Ratatui rendering; it does not access SQLite, the
  filesystem, or the terminal event source.
- `src/input.rs`: contextual key-event to action mapping.
- `src/runner.rs`: terminal event loop and effect execution boundary.
- `src/terminal.rs`: Crossterm raw-mode/alternate-screen ownership and
  restoration guards.
- `src/paths.rs`: injectable home, application-data, and executable search
  paths.

Keep `update` free of filesystem and database work. New external work should be
represented as typed effects and performed outside the reducer. Keep agent
conventions behind adapters rather than spreading paths or enablement rules
through UI and scanner code. Production dependencies require explicit review.
