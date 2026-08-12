# Skilled

Skilled is a local-first terminal application for developers who use multiple
coding agents and keep skills in local Git repositories. It is being built in
Rust with Ratatui and Crossterm.

The project is early in version-one development. The current build establishes
the setup, terminal, source-registration, and read-only inspection foundation,
installs and safely uninstalls registered skills across the agents that can use
them, repairs the incorrect and dangling links it owns, and can forget inactive
source metadata. It does not yet update or fetch anything over the network.

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
- A Doctor view listing every finding the scan holds, ordered by the documented
  issue groups and then by severity. Each finding states what was observed, what
  it costs, and the paths involved. The findings this release can act on offer
  `r`; the rest name no key, because Skilled has none to offer them yet.
- Receipt-backed repair of an incorrect or dangling link Skilled owns, from
  Doctor (`r`) or `skilled repair`. The link's raw target must still be
  byte-identical to the newest matching ownership receipt and the live registry
  must supply a safe replacement, or the repair is refused. Skilled replaces
  that one link and nothing else: it never recreates an absent link, never
  creates a skill root, and never adopts a link it cannot prove is its own.
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
  link exists. A link Skilled did not create is never claimed: an identical one
  already in place is left alone and unowned. Receipts are deleted only after a
  guarded removal is positively verified or a source forget has established
  the described link inactive.
- Guarded uninstall from Inventory (`x`) or `skilled uninstall`: Skilled removes
  only a symlink whose path, object type, recorded target, documented root, and
  ownership receipt still match the preview. It never removes the agent root or
  follows the link into canonical content, then rescans and verifies both that
  the link is gone and that resolving content survived.
- Metadata-only Forget Source from the Sources Repositories pane (`x`). Active
  or unreadable receipted links block the operation; otherwise one transaction
  removes the source registration, catalogs, cached scan state, and inactive
  receipts while leaving the checkout and every skill directory untouched.
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

Update and remote fetching are still future work. Registration and inventory
remain read-only, and Doctor writes nothing of its own. Install creates only a
link and, when allowed, its documented root; uninstall removes only verified
managed links; repair replaces only one proven link; Forget Source removes only
private metadata.

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
Press `x` on a managed skill to preview removing its receipted links. The dialog
states every absolute path and offers confirmation only after the complete plan
has been visible.

From Inventory, press `2` to open Sources. In Sources, Tab and Shift-Tab move
forward and backward through Repositories, Variants, and Details; Enter advances
toward Details; and Esc returns through the region hierarchy before leaving the
screen. In a selectable list, `j` / `k` or arrow keys move the selection. Press
`a` to add another source or `1` to return to Inventory.
In the Repositories pane, press `x` to preview forgetting the selected source's
private metadata. Active or unreadable managed links block confirmation.

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
detail region and then the screen. On a finding this release can act on, `r`
previews the one link Skilled would replace — the same preview, confirmation,
rescan, and verification the install dialog uses. Press `1` or `2` to return to
Inventory or Sources.

Private metadata is stored in the platform application-data directory. On
macOS, the database is normally
`~/Library/Application Support/skilled/skilled.sqlite3`.

## Install, repair, and uninstall from the command line

```bash
skilled install --source <id-or-path> --skill <name> \
                --agents claude-code,codex,opencode [--yes]
skilled repair --skill <name> --agent <agent> [--yes]
skilled uninstall --skill <name> --agent <agent> [--yes]
```

`--source` takes the identifier Skilled gave a registered source or the path its
checkout sits at. `--agents` defaults to every configured agent. Without
`--yes`, the plan is printed and `Proceed? [y/N]` is asked; anything but a yes
cancels and writes nothing.

`--yes` removes the confirmation and nothing else. It requires `--source`,
`--skill`, and `--agents` to be given explicitly — a target set Skilled chose is
not one anybody agreed to — and every collision check, apply guard, rescan, and
verification still runs.

Repair's `--agent` is singular for the same reason: it replaces exactly one
link. The plan is refused unless an ownership receipt proves the link is
Skilled's own, its raw target still matches that receipt byte for byte, and the
current registry offers a replacement variant that agent can use. Skilled takes
the same metadata guard the interactive path takes, rechecks the registration
and the destination under it, then rescans and verifies the replacement.

Uninstall's `--agent` is deliberately singular. It removes only that agent's
still-matching managed link; an absent receipt, changed target, changed object
type, or redirected root blocks the request.

For both, `--yes` skips only the prompt, and still requires `--skill` and
`--agent` to be given explicitly.

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
- `src/operations.rs` plans install, repair, uninstall, and source-forget
  operations; each executor rechecks the facts that authorize its narrowly
  scoped mutation and verifies the result.
- `src/cli.rs` implements `skilled install`, `skilled repair`, and
  `skilled uninstall` over those same planner/apply paths.
- `src/validation.rs` validates the portable `SKILL.md` subset used during
  source browsing.
- `src/terminal.rs` guards raw mode and alternate-screen restoration.
- `src/paths.rs` supplies platform paths while allowing isolated test paths.

Work is tracked with [Beads](https://github.com/gastownhall/beads). Run
`bd ready` to see the next unblocked implementation slice.
