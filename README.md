# Skilled

Skilled is a local-first terminal application for developers who use multiple
coding agents and keep skills in local Git repositories. It is being built in
Rust with Ratatui and Crossterm.

The project is early in version-one development. The current build establishes
the setup, terminal, source-registration, and read-only inspection foundation;
it does not yet install, repair, update, or uninstall skills.

## Design references

- [GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
  authoritative version-one product and technical specification.
- [`spec/tui-prototype.html`](spec/tui-prototype.html) is the tracked,
  interactive visual reference. It uses demo data and performs no filesystem
  writes.

## What works today

- A prototype-aligned seven-step first-run setup flow with explicit segmented
  progress, per-root scan results, and responsive shared-dialog layouts.
- Detection of Claude Code, Codex, and OpenCode roots and executables without
  launching an agent.
- Agent selection, with all three agents selected by default.
- Registration of local Git checkouts from setup or the Sources screen. Skilled
  resolves nested input paths to their canonical repository root and records
  metadata without writing to agent installation directories.
- Discovery of common `skills/` catalogs, supported agent-specific catalog
  roots, and repositories containing one root `SKILL.md`.
- Portable skill validation for exact filenames, YAML frontmatter, names,
  descriptions, readable UTF-8 content, and immediate catalog children.
- A responsive Sources browser with Repositories, Variants, and structured
  Details regions for current Git state, catalog classification, agent
  compatibility, valid or invalid skills, and recoverable source or catalog
  errors. Wide terminals show all three regions; compact terminals show the
  focused region.
- Versioned SQLite persistence for setup, configured agents, source metadata,
  and confirmed catalog roots.
- A read-only inventory of the documented global skill roots — `~/.claude/skills`,
  `~/.agents/skills`, and `~/.config/opencode/skills` — with one row per skill,
  a per-agent cell for each, and a health word. Skilled reads the immediate
  children of each root only: it never recurses, never launches an agent, and
  never writes.
- Resolution of an installed symbolic link to the registered source, catalog,
  and variant it points at, by canonical path equality. A physical copy, or a
  link into anything Skilled does not manage, stays explicitly unmanaged rather
  than being claimed. When a registered checkout cannot be read, provenance is
  reported as unverified rather than denied.
- Health findings with stable codes for dangling and unresolvable links,
  unreadable entries, and every portable-validation failure, each carrying the
  observation behind it. A stray file beside the skill directories is reported
  as not a skill rather than as a broken installation.
- Per-root accounting that distinguishes a root that was read from one that
  does not exist, one that could not be read in full, one belonging to an agent
  that was never selected, and one that has not been scanned yet. Counts are
  withheld rather than reported as zero whenever a root was not read.
- Filtering the inventory by skill name, source, or health.
- Direct startup into Inventory after setup is complete.
- A shared-dialog Settings action for rerunning setup. Rerunning refreshes agent
  root and executable detection while retaining current agent selections and
  registered source metadata.
- A persistent application frame — product title bar, primary navigation,
  session status, workspace, and contextual key hints — drawn from the tracked
  visual prototype. Destinations without an implementation are shown as
  explicitly unavailable rather than offered, and key hints advertise only
  commands the active context handles.
- Contextual keyboard help from Setup, Inventory, Sources, and Settings. Help is
  modal, lists only commands implemented in the underlying context, and closes
  before Esc changes that context.
- Ratatui layouts at 80×24 and wider, with a second detail region at 100
  columns or more, plus a recoverable notice for smaller terminals.
- Terminal restoration on normal exit, startup failure, panic unwinding, and
  the Ctrl-C key path used in raw mode.

Doctor, planning, installation, repair, update, remote fetching, and uninstall
behavior are still future work. Registration and inventory are deliberately
read-only: they catalog local checkouts and observe agent roots, but never
copy, link, or modify anything in them.

## Requirements

- Stable Rust 1.97 or newer.
- macOS is the current acceptance platform. The implementation avoids
  unnecessary macOS coupling, but other platforms are not release gates yet.

## Run

```bash
cargo run
```

On first launch, use Enter to move through all seven setup steps; Summary labels
the final action `Enter Inventory`. The Detect Agents step also supports:

- `j` / `k` or arrow keys to move.
- Space to toggle the focused agent.
- Esc to go back.
- `q` or Ctrl-C to quit.

Press `?` in Setup or any implemented top-level view to open its contextual
keyboard reference. Press Esc to close help; ordinary `q` does not bypass an
open dialog.

After setup, press `s` to open Settings and rerun the wizard. Rerunning preserves
the selected agents and registered sources while refreshing non-executing root
and executable detection.

During setup's Discover Sources step, or later from Sources, press `a` and enter
a path anywhere inside a local Git checkout. Enter inspects the checkout and
shows its canonical repository, branch, revision, and proposed catalog roots.
In catalog confirmation:

- `j` / `k` or arrow keys select a catalog root.
- Space includes or excludes the root.
- `c` switches between common and agent-specific classification.
- `1`, `2`, and `3` toggle Claude Code, Codex, and OpenCode compatibility.
- Enter registers the selected metadata; Esc cancels.

In Inventory, Tab and Shift-Tab move between the skill table and its details;
Enter opens the details of the selected skill on a compact terminal, and Esc
returns. `j` / `k` or arrow keys move the selection, and `/` filters by name,
source, or health — Enter applies the query and Esc clears it.

From Inventory, press `2` to open Sources. In Sources, Tab and Shift-Tab move
forward and backward through Repositories, Variants, and Details; Enter advances
toward Details; and Esc returns through the region hierarchy before leaving the
screen. In a selectable list, `j` / `k` or arrow keys move the selection. Press
`a` to add another source or `1` to return to Inventory.

Private metadata is stored in the platform application-data directory. On
macOS, the database is normally
`~/Library/Application Support/skilled/skilled.sqlite3`.

## Build and verify

```bash
cargo build --release
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Tests use temporary homes and repositories; they do not inspect or mutate real
agent skill directories. Ratatui behavior is covered two ways: text snapshots
under `tests/snapshots/` and cell-level style assertions in
`tests/tui_shell.rs`.

## Architecture

- `src/app.rs` owns state transitions and emits typed effects.
- `src/runner.rs` owns the terminal loop and executes effects.
- `src/tui.rs` composes the application shell and screens without external side
  effects.
- `src/theme.rs` defines every colour as a semantic token; no other module
  names a colour.
- `src/viewport.rs` classifies terminal width and lays out workspace regions.
- `src/components.rs` provides the shared badge, row, header, empty-state,
  segmented-progress, dialog frame and footer regions, and key-hint primitives.
- `src/agents.rs` isolates agent discovery conventions and their documentation
  snapshots.
- `src/store.rs` owns versioned SQLite metadata and migrations.
- `src/source.rs` performs bounded catalog discovery and read-only Git
  inspection.
- `src/inventory.rs` performs the bounded, read-only scan of the native agent
  skill roots and owns the finding codes it reports.
- `src/validation.rs` validates the portable `SKILL.md` subset used during
  source browsing.
- `src/terminal.rs` guards raw mode and alternate-screen restoration.
- `src/paths.rs` supplies platform paths while allowing isolated test paths.

Work is tracked with [Beads](https://github.com/gastownhall/beads). Run
`bd ready` to see the next unblocked implementation slice.
