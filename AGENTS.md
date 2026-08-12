# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd prime` for full
workflow context; the managed Beads sections at the end of this file cover
commands, the sync model, and the session-close protocol.

## Project Status

Skilled is an early Rust 2024 / Ratatui terminal application for inspecting
and managing global coding-agent skills. First-run setup, local Git source
registration, Sources browsing, a read-only installation inventory, OpenCode
effective resolution across its documented roots, a read-only Doctor findings
view, installation, guarded uninstall, and metadata-only Forget Source — each
previewed and confirmed, with post-operation verification — are implemented.

Filesystem mutation is narrow on purpose: install creates one directory
symbolic link per agent (and its documented skill root only when the root's own
parent exists), while uninstall removes only an exact receipted link after
rechecking its type, target, root, and ownership. Forget Source deletes private
registration/catalog/receipt metadata only after proving every described link
inactive; it never deletes a checkout or skill content. Updates, repair,
adoption of links Skilled did not create, and all network operations remain
unimplemented: do not turn the current placeholders into behavior unless the
active Beads issue places that work in scope, and do not display a count,
finding, status, or key hint the code cannot currently produce. Doctor lists
what was observed and states that no repair exists; it offers no key that would
perform one.

[GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
product and technical source of truth. The tracked `spec/tui-prototype.html`
is the visual design reference. Design rationale — including every recorded
departure from the prototype — lives as doc comments on the constants and
functions that implement it, not in this file; read the module you are
changing before overriding a bound, style, or phrase it documents.

## Build and Test

Requires stable Rust 1.97 or newer.

```bash
cargo run
cargo run -- install --source <id-or-path> --skill <name> --agents claude-code
cargo run -- uninstall --skill <name> --agent claude-code
cargo build --release
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Tests use temporary homes and application-data directories; never point a test
at the real user home or real agent skill roots. Ratatui layouts are verified
two ways — Insta snapshots under `tests/snapshots/` capture a screen's text,
and `tests/tui_shell.rs` asserts styles cell by cell — and a status or focus
signal needs both, because colour alone is not an acceptable cue.

## Architecture

- `src/app.rs`: application state, actions, pure reducer transitions, typed
  effects.
- `src/agents.rs`: Claude Code, Codex, and OpenCode adapters; agent path
  conventions and non-executing detection belong here.
- `src/source.rs`: local Git source inspection, catalog discovery, and skill
  candidate validation.
- `src/inventory.rs`: read-only scan of the native agent skill roots; owns the
  finding codes, the state vocabulary, and the count-or-phrase verdict.
- `src/operations.rs`: pure install, uninstall, and Forget Source planners plus
  guarded executors. Each filesystem or metadata mutation re-reads its target
  immediately before writing; uninstall verifies the link gone and content
  survived before deleting its receipt, and forget rechecks the entire receipt
  set and link liveness before its transaction.
- `src/cli.rs`: the `skilled install` and `skilled uninstall` commands —
  hand-parsed surfaces over the same planners, guards, rescans, and verification
  the screens run, with distinguishable exit statuses.
- `src/resolution.rs`: pure per-agent variant selection and OpenCode effective
  resolution; decides which registered variant an agent resolves a name to and
  what OpenCode would load, over data the caller already holds. It states no
  findings — `inventory.rs` maps its verdicts to codes and severities.
- `src/validation.rs`: portable `SKILL.md` front-matter validation.
- `src/store.rs`: private versioned SQLite metadata and migrations; newer
  unknown schemas fail closed.
- `src/theme.rs`: every colour in the application, as semantic roles.
- `src/viewport.rs`: responsive viewport classes and workspace geometry.
- `src/components.rs`: pure shared UI primitives.
- `src/tui.rs`: composes the shell from those primitives; pure, no I/O.
- `src/input.rs`: contextual key-event to action mapping.
- `src/runner.rs`: terminal event loop and effect execution boundary.
- `src/terminal.rs`: raw-mode/alternate-screen ownership and restoration.
- `src/paths.rs`: injectable home, application-data, and executable search
  paths, plus the session identity (user, host, operating system) gathered
  once at startup — every segment optional, omitted rather than invented, and
  injected by tests so they never read the real environment.
- `src/main.rs`: no arguments runs the interactive application; anything else is
  a command, reported through an exit status.

## Invariants

- Keep `update` free of filesystem, process, and database work: external work
  is a typed `Effect` performed by the runner (see `Effect::ScanInstallations`
  and the snapshot reset in `enter_inventory`).
- The reducer is geometry-blind, and stays that way. What only the renderer can
  measure crosses back the other way: `tui::render` returns a `RenderFeedback`
  and the runner notes it before reading the next key. `None` there means the
  frame did not draw the thing, which is not the same as measuring zero.
- Truthfulness is a hard requirement of the inventory. Every summarising
  surface asks `InventorySnapshot` — `stated_skill_count`, `scan_pending`,
  `no_agent_configured` — rather than re-deriving what may be claimed, and
  the scanner's distinctions (read, unreadable, absent, not selected, not
  scanned yet; "not registered" apart from "could not tell") are never
  flattened into one another.
- Every colour comes from a `theme.rs` role; a test enforces that `Color::`
  appears in no other module, and information-bearing text must meet WCAG
  4.5:1 against its surface.
- Build screens from `components` primitives and `theme` tokens; keep agent
  conventions behind the `agents.rs` adapters.
- A variant's stored compatibility set records which agents *discover* a
  catalog root, not which can use the edition in it: OpenCode reads Claude
  Code's and Codex's roots, so a `.claude/skills` catalog is registered for
  OpenCode while holding another agent's edition. Anything deciding usability
  asks `VariantRef::usable_by`, which reads the catalog's own path through
  `AgentAdapter::owns_source_catalog`.
- Key hints and counts stay truthful: a key-bar hint appears only when
  `src/input.rs` handles it in that context, and a count only when the data
  behind it supports one. A tab's route digit is caption rather than hint —
  every available destination shows its number, the active view's included,
  where pressing it is simply inert; a destination this release cannot open
  still shows none. Counts render as `·N` so a bare amber digit cannot read
  as the next tab's route key — the prototype separates the two classes by
  colour alone, which a terminal may not rest on.
- Nothing is written until a plan the user has seen in full is confirmed, and
  that is enforced rather than assumed: a preview taller than its dialog scrolls
  rather than dropping what it cannot hold, and neither the reducer nor the
  footer offers a confirmation until its last row has been on screen. A plan
  blocks whole: one blocked target and nothing is written anywhere. Every
  target's absolute path is stated unabbreviated — the `~` spelling the rest of
  the application uses would soften the thing being agreed to.
- Skilled writes only inside a root it established. A skill root, or any
  directory between it and the home directory, that is a symbolic link is
  refused rather than followed: the path the preview stated has to be the path
  the write lands on. One pathname window remains between the check and the
  write; it is recorded on `apply_install` and `apply_uninstall` and tracked as
  `skilled-cb2`.
- Text from the filesystem — names, paths, link targets, operating-system error
  messages — is escaped through `components::terminal_safe` before it reaches a
  terminal, on every surface. The screens and CLI commands write to the same
  terminal by different routes and both go through it.
- Verification has three answers, not two. `VerifyReport::is_verified` means
  nothing disagreed with the plan; `is_complete` means every postcondition was
  also checked. A root the scan could not read leaves its check withheld, which
  no surface may report as a pass. This is the inventory's own rule applied to
  the operation that follows it.
- `--yes` removes the confirmation and nothing else. Install requires its
  source, skill, and agent set explicitly; uninstall requires one explicit
  skill and agent. Every guard, rescan, and verification still runs, and an
  explicitly named agent the plan cannot act on is blocked rather than skipped.
- Ownership receipts are evidence, never instructions. Nothing recreates a link
  from one, the scanner does not consult them, and a link Skilled did not create
  is never adopted. A receipt is removed only after uninstall positively
  verifies its link gone, or Forget Source has just established the described
  link inactive.
- Uninstall never removes an agent root or follows the link it removes. Object
  type, exact receipt, recorded target, and documented-root containment are
  rechecked immediately before unlinking; one failed target stops the run.
- Forget Source removes private metadata only. Any active or unreadable
  receipted link, or any receipt-set change between preview and confirmation,
  blocks the transaction; checkout and skill directories are never deleted.
- Production dependencies require explicit review.

## Non-Interactive Shell Commands

`cp`, `mv`, and `rm` may be aliased to `-i` and hang waiting for input: always
pass `-f` (`rm -rf`, `cp -rf`). Use `-y` for `apt-get`, `-o BatchMode=yes` for
`ssh`/`scp`, and `HOMEBREW_NO_AUTO_UPDATE=1` for `brew`.

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
