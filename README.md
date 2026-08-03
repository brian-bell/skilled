# Skilled

Skilled is a local-first terminal application for developers who use multiple
coding agents and keep skills in local Git repositories. It is being built in
Rust with Ratatui and Crossterm.

The project is early in version-one development. The current build establishes
the setup, terminal, and local source-registration foundation; it does not yet
install, repair, update, or uninstall skills.

## Design references

- [GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
  authoritative version-one product and technical specification.
- [`spec/tui-prototype.html`](spec/tui-prototype.html) is the tracked,
  interactive visual reference. It uses demo data and performs no filesystem
  writes.

## What works today

- A seven-step first-run setup flow.
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
- A Sources browser for registered repositories and valid or invalid variants,
  including current Git state, catalog classification, agent compatibility, and
  recoverable source or catalog errors.
- Versioned SQLite persistence for setup, configured agents, source metadata,
  and confirmed catalog roots.
- Direct startup into Inventory after setup is complete.
- A Settings action for rerunning setup.
- Ratatui layouts at 80×24 and wider, plus a recoverable notice for smaller
  terminals.
- Terminal restoration on normal exit, startup failure, panic unwinding, and
  the Ctrl-C key path used in raw mode.

Installation inventory, Doctor, planning, installation, repair, update, remote
fetching, and uninstall behavior are still future work. Registration is
deliberately read-only: it catalogs local checkouts but does not copy, link, or
modify skills in agent roots.

## Requirements

- Stable Rust 1.97 or newer.
- macOS is the current acceptance platform. The implementation avoids
  unnecessary macOS coupling, but other platforms are not release gates yet.

## Run

```bash
cargo run
```

On first launch, use Enter to move through setup. The Detect Agents step also
supports:

- `j` / `k` or arrow keys to move.
- Space to toggle the focused agent.
- Esc to go back.
- `q` or Ctrl-C to quit.

After setup, press `s` to open Settings and rerun the wizard.

During setup's Discover Sources step, or later from Sources, press `a` and enter
a path anywhere inside a local Git checkout. Enter inspects the checkout and
shows its canonical repository, branch, revision, and proposed catalog roots.
In catalog confirmation:

- `j` / `k` or arrow keys select a catalog root.
- Space includes or excludes the root.
- `c` switches between common and agent-specific classification.
- `1`, `2`, and `3` toggle Claude Code, Codex, and OpenCode compatibility.
- Enter registers the selected metadata; Esc cancels.

From Inventory, press `2` to open Sources. In Sources, Tab switches between the
repository and variant panes, `j` / `k` or arrow keys move the selection, `a`
adds another source, and `1` returns to Inventory.

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
agent skill directories. Ratatui behavior is covered by snapshots under
`tests/snapshots/`.

## Architecture

- `src/app.rs` owns state transitions and emits typed effects.
- `src/runner.rs` owns the terminal loop and executes effects.
- `src/tui.rs` renders state without external side effects.
- `src/agents.rs` isolates agent discovery conventions and their documentation
  snapshots.
- `src/store.rs` owns versioned SQLite metadata and migrations.
- `src/source.rs` performs bounded catalog discovery and read-only Git
  inspection.
- `src/validation.rs` validates the portable `SKILL.md` subset used during
  source browsing.
- `src/terminal.rs` guards raw mode and alternate-screen restoration.
- `src/paths.rs` supplies platform paths while allowing isolated test paths.

Work is tracked with [Beads](https://github.com/gastownhall/beads). Run
`bd ready` to see the next unblocked implementation slice.
