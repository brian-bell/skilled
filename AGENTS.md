# Agent Instructions

Skilled is an early Rust 2024 / Ratatui terminal application for inspecting and
managing global Claude Code, Codex, and OpenCode skills from local Git sources.
See [README.md](README.md) for current features and first use.

## Working context

- Read [docs/architecture.md](docs/architecture.md) for the module map and
  [docs/safety.md](docs/safety.md) before changing scanners, resolution,
  operations, Git guards, or verification. The detailed contracts there are
  required maintenance context.
- [GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the product
  and technical design reference; [spec/tui-prototype.html](spec/tui-prototype.html)
  is the visual reference. Read the implementing module's doc comments before
  overriding a documented bound, style, or phrase.
- Adoption of unproven links and network operations beyond the explicit update
  check remain unimplemented. Do not implement placeholders unless the active
  Beads issue puts them in scope. The confirmed fast-forward can run disclosed
  repository programs, including programs that access the network.
- Production dependencies require explicit review.

## Build and test

Requires stable Rust 1.97 or newer. Repository updates require Git 2.41 or
newer and, for SSH remotes, OpenSSH 8.4 or newer.

```bash
cargo run
cargo run -- --version
cargo run -- install --source <id-or-path> --skill <name> --agents claude-code
cargo run -- uninstall --skill <name> --agent claude-code
cargo run -- repair --skill <name> --agent claude-code
cargo run -- update --source <id-or-path>
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

Release packaging requires a clean checkout; see the additional
[package gates](docs/architecture.md#release-package).

Tests must inject temporary homes, application-data directories, and agent
roots. UI status and focus changes need both text snapshots and cell-level
style assertions; colour alone cannot carry information.

## Essential constraints

- Keep the reducer and renderer free of I/O. Typed effects run through the
  event loop; the renderer returns geometry through `RenderFeedback`.
- Use `components` primitives, `theme` colours, and `agents.rs` conventions.
  Information-bearing text must meet 4.5:1 contrast. Escape filesystem and
  error text through `components::terminal_safe` on every terminal surface.
- Ask `InventorySnapshot` what counts and findings can be stated. Preserve
  unknown, unreadable, absent, and deselected distinctions. Advertise keys
  only in contexts that handle them.
- Preview every planned mutation with absolute paths; enable confirmation only
  after the complete plan has been visible. A blocked plan blocks whole.
  `--yes` skips only the CLI prompt and requires explicit targets.
- Install refuses occupied paths and redirected roots. Repair and uninstall
  require exact receipt evidence; never adopt unproven links. Forget Source
  removes metadata only after proving every described link inactive.
- Repository updates require an explicit check, proven checkout identity, and
  a guarded fast-forward to the previewed revision. Preserve the detailed
  transport, pathname, ref, and verification contracts in `docs/safety.md`.
- Rescan after applying. Keep verified, failed, and incomplete verification
  distinct in reports and exit statuses. Unavailable metadata permits only
  degraded read-only operation; destructive migrations require a backup.

## Beads workflow

Use the [Beads skill](.agents/skills/beads/SKILL.md) and `bd` for all task
tracking. Run `bd prime` when context is missing or stale. Use `bd ready`,
`bd show <id>`, and `bd update <id> --claim` to select and claim work.
Create an issue before implementation; close it only after completion.
Use `bd remember` for durable knowledge, never `MEMORY.md` or markdown task lists.
Do not use `bd edit`, which opens an interactive editor.

The active profile is conservative: do not commit, push, or run Dolt remote
sync without explicit authority. Beads instructions do not override user or
repository restrictions. At session end, file needed follow-ups, run relevant
quality gates, close completed issues, inspect `git status`, and report changed
files, validation, issue status, and proposed commit/sync commands.

Issues live in a local Dolt database; sync uses `refs/dolt/data` on the Git
remote. `.beads/issues.jsonl` is a passive export, not the database. See the
[Beads sync model](https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md).

## Non-interactive shell commands

`cp`, `mv`, and `rm` may be aliased to `-i`: pass `-f` (`rm -rf`, `cp -rf`).
Use `-y` for `apt-get`, `-o BatchMode=yes` for `ssh`/`scp`, and
`HOMEBREW_NO_AUTO_UPDATE=1` for `brew`.
