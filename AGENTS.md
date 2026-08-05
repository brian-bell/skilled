# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd prime` for full workflow context.

> **Architecture in one line:** Issues live in a local Dolt database
> (`.beads/dolt/`); cross-machine sync uses `bd dolt push/pull` (a
> git-compatible protocol), stored under `refs/dolt/data` on your git
> remote — separate from `refs/heads/*` where your code lives.
> `.beads/issues.jsonl` is a passive export, not the wire protocol.
>
> See [SYNC_CONCEPTS.md](https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md)
> for the one-screen overview and anti-patterns (don't treat JSONL as the
> source of truth; don't `bd import` during normal operation; don't
> reach for third-party Dolt hosting before trying the default).

## Project Status

Skilled is an early Rust 2024 and Ratatui terminal application for inspecting
and eventually managing global coding-agent skills. Implemented so far:
prototype-aligned seven-step first-run setup with segmented progress, agent
detection and selection, SQLite-backed setup persistence, local Git source
registration with catalog confirmation, responsive three-region Sources
browsing of registered repositories, skill variants, and structured details,
an empty Inventory view, shared-dialog Settings setup reset, reducer-owned
contextual help, responsive size handling, and guarded terminal restoration.

Installation inventory, Doctor findings, Updates, and every filesystem or
network mutation beyond the private metadata database are not implemented yet.
Do not turn the current placeholders into behavior unless the active Beads
issue places that work in scope, and do not display a count, finding, status,
or key hint the code cannot currently produce.
[GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
product and technical source of truth. The tracked `spec/tui-prototype.html` is
the visual design reference.

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
two ways: Insta snapshots under `tests/snapshots/` capture the text of a whole
screen, and `tests/tui_shell.rs` asserts styles cell by cell against the
rendered buffer. A status or focus signal needs both, because colour alone is
not an acceptable cue.

## Application Architecture

- `src/app.rs`: application state, actions, pure reducer transitions, and typed
  persistence effects.
- `src/agents.rs`: Claude Code, Codex, and OpenCode adapters plus non-executing
  root and executable detection. Agent path conventions and documentation
  snapshots belong here.
- `src/source.rs`: local Git source inspection, catalog discovery, and skill
  candidate validation.
- `src/validation.rs`: portable `SKILL.md` front-matter validation.
- `src/store.rs`: private versioned SQLite metadata and migrations. Newer
  unknown schemas fail closed.
- `src/theme.rs`: semantic presentation tokens translated from the prototype
  palette. Every colour in the application is defined here; a screen asks for a
  role such as `Tone::Warning` or `nav_active()` rather than naming a colour. A
  test enforces that `Color::` appears in no other module. Information-bearing
  text must meet WCAG 4.5:1 against its surface; a theme unit test guards
  every such role, and the two accepted `FAINT` exemptions (`empty_glyph`,
  `nav_disabled`) are recorded on their doc comments.
- `src/viewport.rs`: responsive viewport classes and workspace region geometry.
  Screens ask whether the terminal is `Compact` or `Wide` instead of comparing
  raw widths.
- `src/components.rs`: pure shared primitives — status badges, list rows, pane
  headers, empty states, segmented setup progress, the modal dialog frame and
  footer regions, and the key-hint bar.
- `src/tui.rs`: composes the persistent shell (title bar, navigation, session
  status, workspace, Setup and Settings dialogs, contextual help, key hints)
  from those primitives. Pure: it does not access SQLite, the filesystem, or
  the terminal event source.
- `src/input.rs`: contextual key-event to action mapping.
- `src/runner.rs`: terminal event loop and effect execution boundary.
- `src/terminal.rs`: Crossterm raw-mode/alternate-screen ownership and
  restoration guards.
- `src/paths.rs`: injectable home, application-data, and executable search
  paths.

Keep `update` free of filesystem and database work. New external work should be
represented as typed effects and performed outside the reducer. Keep agent
conventions behind adapters rather than spreading paths or enablement rules
through UI and scanner code. Build new screens from `components` primitives and
`theme` tokens rather than ad hoc styles. The key-hint bar and the navigation
row are contracts: a hint or destination may only appear when `src/input.rs`
actually handles it in that context. Production dependencies require explicit
review.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
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
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
