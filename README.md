# Skilled

Skilled is a local-first terminal application for developers who use multiple
coding agents and keep skills in local Git repositories. It is being built in
Rust with Ratatui and Crossterm.

The project is early in version-one development. The current build establishes
the setup, terminal, source-registration, and read-only inspection foundation,
installs a registered skill across the agents that can use it, repairs a link
it owns, and fast-forwards a registered repository after an explicit check. It
does not yet uninstall.

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
  errors. Each repository names its checkout and the branch and revision it was
  last seen at; variants are listed under the catalog they come from, which
  states its path at the left and as much of its classification and agent
  registration as the label has room for at the right. Wide terminals show all
  three regions; compact terminals show the focused region.
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
  that was never selected, and one that has not been scanned yet. A number is
  stated only when every root in scope was read or found absent and at least one
  of them was actually read; otherwise Skilled says why it is withholding the
  count instead of reporting a zero.
- Deterministic per-agent variant selection: an exact agent-specific edition,
  then a compatible common one, and nothing an agent cannot use. Where more than
  one registered variant survives for an agent and a name, Skilled reports the
  ambiguity and names every competing variant rather than picking one.
- OpenCode effective resolution across the three roots it reads —
  `~/.config/opencode/skills` first, then `~/.agents/skills`, then
  `~/.claude/skills`. One directory reached through several roots is a benign
  alias; different directories behind one name are a conflicting duplicate; and
  another agent's edition, visible to OpenCode with no edition of its own behind
  it, is reported as exposure rather than claimed as usable. A root Skilled was
  asked to leave alone, or could not read, leaves the answer unstated instead of
  guessed at.
- A read-only Doctor view listing every finding the scan holds, ordered by the
  documented issue groups and then by severity. Each finding states what was
  observed, what it costs, the paths involved, and that no repair exists in this
  release — Skilled offers no key that would perform one.
- Installation of a registered skill variant as one individual directory
  symbolic link per agent, at that agent's own documented global root. Press `i`
  on a variant in Sources to see exactly what would happen: the source it comes
  from, the directory every link would point at, and each agent's absolute
  target path in full, with the reason for any agent left out. Nothing is
  written until that preview is confirmed.
- A plan that blocks, blocks whole. A file, a physical directory, a symbolic
  link Skilled does not own, a link that no longer resolves, an entry it could
  not read, an agent whose own directory does not exist, and a name more than
  one registered variant answers to each stop the install — and nothing is
  written to the targets that were free either. Skilled never overwrites,
  replaces, or removes anything, so an occupied path is always a refusal.
- Refusal to write a link OpenCode would not then resolve. Because OpenCode
  reads Claude Code's and Codex's roots as well as its own, a link into its root
  that another root already answers for is a postcondition Skilled can see
  failing, and it stops before writing rather than writing and reporting it.
- An ownership receipt for every link Skilled creates, recorded the moment the
  link exists and outliving the source it came from. A link Skilled did not
  create is never claimed: an identical one already in place is left alone and
  unowned.
- A rescan and a postcondition check after every install. Each link written is
  observed again and compared with the plan — the object, the variant it
  resolves to, its validation, its health, and for OpenCode the name it
  effectively loads. A check Skilled could not make over a root it never read is
  reported as unestablished rather than counted as a pass.
- Filtering the inventory by skill name, source, health, or the words the
  effective resolution adds.
- A scrollable inventory detail region. A skill installed for several agents
  outgrows the minimum terminal, so `j` / `k` move the region's window once it
  has focus, by whole lines, so a wrapped field passes the window's edge whole
  rather than being cut into a label with no value under it. Both ends of the
  window state in rows what they hide, and the notice at the foot names what
  would actually reach those rows from where the reader is standing: the
  movement keys, a region focus before them, or a larger terminal when no
  keystroke would do.
- A scrollable Doctor detail region, sharing the Inventory's window behaviour
  and its accounting for the rows it cannot show at once.
- Direct startup into Inventory after setup is complete.
- A shared-dialog Settings action for rerunning setup. Rerunning refreshes agent
  root and executable detection while retaining current agent selections and
  registered source metadata.
- A persistent application frame — product title bar, primary navigation,
  session status, workspace, and contextual key hints — drawn from the tracked
  visual prototype. The title bar states the session's context path
  (`global · user@host · macOS`), omitting any segment the environment does
  not provide; the navigation is a strip of boxed, padded tab cells; and the
  session status sits beside the tabs on wide terminals when both fit whole,
  in the title bar otherwise. Destinations without an implementation are
  shown as explicitly unavailable rather than offered, and key hints advertise
  only commands the active context handles. Inventory, Sources, and Doctor
  carry a count beside their name in the navigation row — Sources always, the
  other two whenever the scan is entitled to state one — while an unavailable
  destination carries no count at all.
- Contextual keyboard help from Setup, Inventory, Sources, Doctor, and
  Settings. Help is
  modal, lists only commands implemented in the underlying context, and closes
  before Esc changes that context.
- Ratatui layouts at 80×24 and wider, with a second detail region at 100
  columns or more, plus a recoverable notice for smaller terminals.
- Terminal restoration on normal exit, startup failure, panic unwinding, and
  the Ctrl-C key path used in raw mode.
- Receipt-backed repair of one incorrect or dangling link Skilled owns. A link
  is replaced only when its recorded target is byte-identical to a Skilled
  ownership receipt for that path and the live registry supplies a safe
  replacement. Repair never recreates an absent link or root, and never adopts
  a link it cannot prove it wrote.
- An explicit update check for a registered repository, and a fast-forward of
  the exact revision it previewed. The check is the only network operation
  Skilled itself starts, and it fetches the configured upstream with the
  repository's own hooks and `core.fsmonitor` suppressed. A check runs no
  program the checkout chose: one that configures a transport command —
  `core.sshCommand`, `core.askPass`, `core.gitProxy`,
  `core.alternateRefsCommand`, a credential helper,
  `remote.<name>.uploadpack`, `remote.<name>.vcs`, `protocol.<scheme>.command`,
  or a URL naming a transport helper such as `ext::` — is refused rather than
  checked, so checking a repository you did not author cannot run what that
  repository configured.
  The same settings in your own global or system Git configuration are yours,
  and keep working. The fast-forward is handed the repository's configuration on
  purpose, and the plan discloses it: the hooks and checkout filters it may run
  are the repository's own programs, and a filter such as Git LFS fetches over
  the network itself. Opening Updates reads cached results only.
- An update preview that states the checkout, the branch, the current and
  target revisions, every incoming commit summary, and which installed skills
  the update adds, updates, removes, renames, or restores, followed by the
  untruncated changed-file listing as evidence. Skilled fast-forwards only:
  it never resets, rebases, stashes, commits, or pushes, and a dirty,
  diverged, detached, partial-clone, submodule-changing, or upstream-less
  checkout blocks the update instead of being worked around.
- A rescan and postcondition check after every fast-forward, with the same
  three answers installation verification gives: verified, not verified, or
  verified as far as the roots Skilled could read allow.
- `skilled install`, `skilled repair`, and `skilled update` as non-interactive
  commands over the same planners, guards, rescans, and verification the
  screens run, with distinguishable exit statuses. `--yes` removes the
  confirmation and nothing else.

Uninstall, adoption of unproven links, and network operations beyond the
explicit update check are still future work. Registration, inventory, and
Doctor remain read-only: they catalog local checkouts and observe agent roots
without changing anything in them. Installing creates a link, and the
documented skill root above it when that root's own parent already exists;
repair replaces one proven link; and an update writes through Git only inside
the canonical checkout the user registered.

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
returns. `j` / `k` or arrow keys move the selection in the table, and scroll the
details region once it has focus — a region with more to show than fits says so
at the foot of its window and names the keys that reach the rest. `/` filters by
name, source, or health — Enter applies the query and Esc clears it.

From Inventory, press `2` to open Sources. In Sources, Tab and Shift-Tab move
forward and backward through Repositories, Variants, and Details; Enter advances
toward Details; and Esc returns through the region hierarchy before leaving the
screen. In a selectable list, `j` / `k` or arrow keys move the selection. Press
`a` to add another source or `1` to return to Inventory.

In Sources, press `i` on a skill variant to preview installing it. The dialog
names every agent, what would happen to it, and the exact absolute path
involved; `j` / `k` scroll it when it holds more than the terminal can show,
Enter installs, and Esc cancels. A blocked plan offers no Enter, because there
is nothing it could do. The report that follows states each step, what the
rescan afterwards made of it, and — where a postcondition could not be checked —
says so rather than reporting a verification it did not make.

From either screen, press `4` to open Doctor. It lists each finding with its
severity, stable code, skill, and agent, and its regions behave as the
Inventory's do: Tab and Shift-Tab move between the list and the details, Enter
opens the details of the selected finding on a compact terminal, `j` / `k` move
the selection and scroll the details once they have focus, and Esc leaves the
detail region and then the screen. Press `1` or `2` to return to Inventory or
Sources.

Private metadata is stored in the platform application-data directory. On
macOS, the database is normally
`~/Library/Application Support/skilled/skilled.sqlite3`.

## Install from the command line

```bash
skilled install --source <id-or-path> --skill <name> \
                --agents claude-code,codex,opencode [--yes]
```

`--source` takes the identifier Skilled gave a registered source or the path its
checkout sits at. `--agents` defaults to every configured agent. Without
`--yes`, the plan is printed and `Proceed? [y/N]` is asked; anything but a yes
cancels and writes nothing.

`--yes` removes the confirmation and nothing else. It requires `--source`,
`--skill`, and `--agents` to be given explicitly — a target set Skilled chose is
not one anybody agreed to — and every collision check, apply guard, rescan, and
verification still runs.

Exit statuses: `0` success, `1` internal error, `2` invalid request, `3` blocked
plan, `4` the apply did not complete as planned, `5` verification failed.

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
- `src/resolution.rs` decides, purely, which registered variant an agent
  resolves a name to and what OpenCode would load across the roots it reads.
- `src/operations.rs` plans installations and executes them under guard: one
  read of the machine, one pure decision over it, one re-read immediately before
  each write, and one check of a fresh scan against the plan.
- `src/cli.rs` implements `skilled install` over that same path.
- `src/validation.rs` validates the portable `SKILL.md` subset used during
  source browsing.
- `src/terminal.rs` guards raw mode and alternate-screen restoration.
- `src/paths.rs` supplies platform paths while allowing isolated test paths.

Work is tracked with [Beads](https://github.com/gastownhall/beads). Run
`bd ready` to see the next unblocked implementation slice.
