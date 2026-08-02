# Skilled

Skilled is a local-first terminal application for developers who use multiple
coding agents and keep skills in local Git repositories. It is being built in
Rust with Ratatui and Crossterm.

The project is early in version-one development. The current build establishes
the setup and terminal foundation; it does not yet install, repair, update, or
uninstall skills.

## What works today

- A seven-step first-run setup flow.
- Detection of Claude Code, Codex, and OpenCode roots and executables without
  launching an agent.
- Agent selection, with all three agents selected by default.
- Versioned SQLite persistence for setup completion and configured agents.
- Direct startup into Inventory after setup is complete.
- A Settings action for rerunning setup.
- Ratatui layouts at 80×24 and wider, plus a recoverable notice for smaller
  terminals.
- Terminal restoration on normal exit, startup failure, panic unwinding, and
  the Ctrl-C key path used in raw mode.

The source-registration, installation-inventory, Doctor, planning, repair,
update, and uninstall screens are still future work. Setup currently displays
explicit placeholders for the source and installation steps.

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
- `src/terminal.rs` guards raw mode and alternate-screen restoration.
- `src/paths.rs` supplies platform paths while allowing isolated test paths.

Work is tracked with [Beads](https://github.com/gastownhall/beads). Run
`bd ready` to see the next unblocked implementation slice.
