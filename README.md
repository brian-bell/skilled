# Skilled

Skilled is a local-first terminal application for developers who use multiple
coding agents and keep skills in local Git repositories. Built in Rust with
Ratatui and Crossterm, it inspects global Claude Code, Codex, and OpenCode skills
and previews changes before applying them.

The project is early in version-one development. The current Cargo package
version is 0.2.0. It currently supports:

- First-run agent selection and registration of local Git sources, including
  common `skills/` catalogs, agent-specific catalogs, and single-skill repos.
- A filterable installation inventory, portable `SKILL.md` validation, variant
  selection, OpenCode effective resolution, and a Doctor findings view.
- Installation as individual directory symlinks, receipt-backed repair of
  incorrect or dangling links, guarded uninstall, and metadata-only Forget Source.
- Explicit repository update checks and confirmed fast-forwards to the exact
  previewed revision, followed by a rescan and verification.
- Read-only inventory when private metadata is unavailable, with unknown state
  stated explicitly.

Skilled never adopts unproven links. Install refuses occupied paths; repair and
uninstall act only on links proven by ownership receipts; Forget Source leaves
checkout content intact. Other network workflows remain future work. See the
[user guide](docs/usage.md) for operation limits and update behavior.

## Requirements

- Stable Rust 1.97 or newer to build.
- Git for local source inspection; Git 2.41 or newer for repository updates.
- OpenSSH 8.4 or newer when checking SSH remotes.
- A terminal of at least 80×24. Wider terminals show details beside the list.

Ubuntu 24.04 and macOS 15 are the current release gates. Windows is not a
release gate yet.

## Build and run

From the checkout:

```bash
cargo run
```

Or build a release binary:

```bash
cargo build --release
./target/release/skilled
./target/release/skilled --version
```

On first launch, Enter advances through setup. Select agents with Space and
add a local Git checkout with `a` during Discover Sources. Review its proposed
catalogs before registering them. Registration stores private metadata and
leaves the checkout and agent skill roots unchanged.

After setup, the application opens Inventory. Press `?` for contextual help.

| View | Key | Main actions |
| --- | --- | --- |
| Inventory | `1` | `/` filters; `x` previews uninstall of managed links. |
| Sources | `2` | `a` adds a source; `i` on a variant previews install; `x` in Repositories previews Forget Source. |
| Updates | `3` | `u` checks repositories; Enter advances to Details and then to an available update preview. |
| Doctor | `4` | Inspect findings; `r` previews repair where supported. |

Tab / Shift-Tab move focus; `j` / `k` or arrows move selections or scroll focused
details. Esc backs out. From Inventory, `s` opens Settings to rerun setup while
retaining selections and sources. Read the full [keyboard and operation guide](docs/usage.md).

## Command line

Register sources in the interactive application first. Commands use the same
planners, guards, and verification as the screens:

```bash
cargo run -- install --source <id-or-path> --skill <name> --agents claude-code
cargo run -- repair --skill <name> --agent claude-code
cargo run -- uninstall --skill <name> --agent claude-code
cargo run -- update --source <id-or-path>
```

Commands print a plan and ask for confirmation. `--yes` skips the prompt only;
all checks still run. See [flags and exit statuses](docs/usage.md#command-line),
including status `6` for incomplete install, repair, or update verification.

## Development and design

See [architecture and verification](docs/architecture.md) for the module map
and CI commands, [safety contracts](docs/safety.md) for mutation invariants,
and [AGENTS.md](AGENTS.md) for contributor workflow. Work is tracked with
Beads; `bd ready` lists available work.

[GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
version-one product and technical specification. The tracked
[interactive prototype](spec/tui-prototype.html) is the visual reference;
it uses demo data and performs no filesystem writes.
