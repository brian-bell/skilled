# Architecture and development

For contributors changing Skilled. The code and checked-in configuration define
current behavior; [AGENTS.md](../AGENTS.md) is the entry point for agent workflow.

## Module map

- `src/app.rs`: application state, actions, pure reducer transitions, typed
  effects.
- `src/agents.rs`: Claude Code, Codex, and OpenCode adapters; agent path
  conventions and non-executing detection belong here.
- `src/source.rs`: local Git source inspection, catalog discovery, and skill
  candidate validation.
- `src/git.rs`: typed no-shell Git boundary for repository inspection, fetch,
  and the sole fast-forward write.
- `src/updates.rs`: repository update probing, classification, planning,
  guarded apply, and three-answer verification.
- `src/inventory.rs`: read-only scan of the native agent skill roots; owns the
  finding codes, the state vocabulary, and the count-or-phrase verdict.
- `src/operations.rs`: sibling install, repair, uninstall, and Forget Source
  pipelines. Their probes are the only machine reads before planning, their pure
  planners decide over those observations, their guarded executors re-read
  immediately before writing, and their verifiers check a fresh scan against the
  confirmed plan. Uninstall verifies the link gone and content survived before
  deleting its receipt, and forget rechecks the entire receipt set and link
  liveness before its transaction. The module reuses `inventory::Finding` for
  the spec 18.2 collision codes.
- `src/cli.rs`: the hand-parsed `skilled install`, `skilled uninstall`,
  `skilled repair`, and `skilled update` surfaces over the same planners,
  guards, rescans, and verification the TUI runs, with distinguishable exit statuses.
- `src/resolution.rs`: pure per-agent variant selection and OpenCode effective
  resolution; decides which registered variant an agent resolves a name to and
  what OpenCode would load, over data the caller already holds. It states no
  findings — `inventory.rs` maps its verdicts to codes and severities.
- `src/validation.rs`: portable `SKILL.md` front-matter validation.
- `src/store.rs`: private versioned SQLite metadata and transactional
  migrations; newer unknown schemas fail closed, a store SQLite opened
  read-only — or one whose application-data directory refuses the journal
  sidecars SQLite writes through, proven at open by a create-and-remove probe
  of Skilled's own file — is refused rather than treated as writable, and
  destructive migrations create a recoverable backup before any pending step
  runs.
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
- `src/main.rs`: `--version` reports the package identity before process
  environment discovery; no arguments runs the interactive application, and
  anything else is a command reported through an exit status.

## Build and verification

Use stable Rust 1.97 or newer, as declared in [Cargo.toml](../Cargo.toml).
The [CI workflow](../.github/workflows/ci.yml) runs these four checks on Ubuntu:

```bash
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

The release-package gate runs on Ubuntu 24.04 and macOS 15 with Rust 1.97.
Windows is not a release gate. Tests inject temporary homes, application-data directories,
repositories, and session identities. Never use real agent roots for tests.

UI verification needs both text snapshots in `tests/snapshots/` and cell-level
style assertions in `tests/tui_shell.rs`. A status or focus signal must be
readable without colour. Layouts support 80×24 and wider, with side-by-side
details from 100 columns and a recoverable notice below the minimum.

## Release package

The Cargo package identity is `skilled 0.2.0`. From a clean checkout:

```bash
cargo package --locked
cargo test --test release_package -- --ignored
```

The ignored release test packages the checkout, installs the exact payload
with a fresh Cargo home, checks `--version` without creating runtime state,
starts the TUI in a pseudo-terminal, and proves a future SQLite schema is
refused without modification. It is separate from the ordinary all-targets
suite. The package manifest includes source, Cargo metadata, the license, and
README; the detailed `docs/` guides remain in the repository. Publishing is
disabled in the manifest.

## Design references

[GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) defines the
version-one product and technical design. The tracked
[interactive prototype](../spec/tui-prototype.html) uses demo data and performs
no filesystem writes. Recorded visual departures and their rationale live in
doc comments on the implementing constants and functions. Read those comments
before changing a bound, style, or phrase.

Read [safety contracts](safety.md) before changing observation, resolution,
planning, filesystem mutations, Git operations, or verification.
