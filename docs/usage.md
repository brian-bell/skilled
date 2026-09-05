# Using Skilled

Skilled works with local Git checkouts registered through its interactive
application. Run setup before using the mutation commands. Build and first-use
requirements are in the [README](../README.md).

## Setup and keyboard navigation

```bash
cargo run
cargo run -- --version
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

After setup, from Inventory press `s` to open Settings and rerun the wizard. Rerunning preserves
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

From Inventory or Sources, press `4` to open Doctor. It lists each finding with its
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

## Repository updates

Press `3` from Inventory, Sources, or Doctor to open Updates. Opening the view
reads cached results. Press `u` to check registered repositories explicitly;
Esc cancels a running check. Select a repository, press Enter to focus Details,
and, when its current verdict offers an update, Enter again to open the plan.
Scroll through the complete plan before confirming it.

The preview names the checkout, branch, current and target revisions, every
incoming commit summary, and affected installed skills. The complete changed-file
listing follows as evidence. Skilled fast-forwards to the exact previewed
revision; it never resets, rebases, stashes, commits, or pushes. Dirty, diverged,
detached, partial-clone, submodule-changing, or upstream-less checkouts block
updates. A removal that would leave content in the skill directory also blocks.
A directory replaced by a regular file is disclosed separately as a target
that stops being a skill. Invalid target `SKILL.md` content is likewise
disclosed as no longer loadable, with verification checking the named failure.
Sources registered before checkout identities were recorded must be
re-registered before updating; Skilled does not adopt a replacement checkout.

The check fetches objects and publishes the configured remote-tracking ref,
with hooks and `core.fsmonitor` suppressed. It refuses checkout-configured
transport programs; user global and system settings remain subject to the
user's transport policy. The fast-forward uses repository configuration and
its preview discloses hooks, filesystem monitors, checkout filters, and
signature verification programs it may run. Those programs can themselves
access the network. See [the update safety contracts](safety.md#repository-updates)
for exact guards and known concurrency boundaries.

## Command line

After `cargo build --release`, use `./target/release/skilled` in place of
`skilled` below, or use `cargo run --` followed by the command and flags.

```bash
skilled install --source <id-or-path> --skill <name> \
                [--agents claude-code,codex,opencode] [--yes]
skilled repair --skill <name> --agent <agent> [--yes]
skilled uninstall --skill <name> --agent <agent> [--yes]
skilled update --source <id-or-path> [--yes]
```

`--source` accepts a registered source identifier or its checkout path.
Agent names are `claude-code`, `codex`, and `opencode`. Install defaults to all
configured agents when `--agents` is omitted. Repair and uninstall accept
exactly one agent. Source registration and Forget Source are interactive only.

Each command prints a plan and asks `Proceed? [y/N]`; declining writes no
planned mutation. The update command performs its explicit check before that
prompt, so objects and the remote-tracking ref can already have been updated.

`--yes` skips only confirmation. Install then requires explicit `--source`,
`--skill`, and `--agents`; repair and uninstall require `--skill` and `--agent`;
update requires `--source`. Every ownership, collision, path, apply, rescan,
and verification guard still runs. These commands use the same operation
pipelines as the interactive screens.

| Exit status | Meaning |
| --- | --- |
| `0` | Success, no work needed, or the user declined the plan. |
| `1` | Internal failure, such as unavailable metadata. |
| `2` | Invalid request. |
| `3` | Blocked request; no planned mutation applied. |
| `4` | Apply did not complete as planned; inspect the report for partial writes or unrecorded ownership. |
| `5` | Post-operation verification failed. |
| `6` | Install, repair, or update applied, but a postcondition within the selected scope could not be checked. |

A deselected root alone does not cause status `6`. Uninstall reports success
when its unlink was verified even if the inert receipt could not be deleted;
the printed report still states that metadata failure.

## Inventory and ownership

Skilled scans immediate children of the selected native roots:

| Agent | Native global root |
| --- | --- |
| Claude Code | `~/.claude/skills` |
| Codex | `~/.agents/skills` |
| OpenCode | `~/.config/opencode/skills` |

The scan never launches an agent or writes to those roots. It validates exact
`SKILL.md` filenames, UTF-8 content, YAML frontmatter, names, and descriptions.
A physical copy or a link outside registered variants remains unmanaged.
Unreadable registration data leaves provenance unverified. Counts distinguish
read, absent, unreadable, deselected, and not-yet-scanned roots; an unavailable
count is explained instead of shown as zero.

Variant selection prefers an agent's own edition, then a compatible common
variant. Ambiguity is reported with the competing variants. OpenCode effective
resolution checks its native root, then Codex's, then Claude Code's. Multiple
links to one directory are aliases; different directories behind the same name
conflict. Visibility of another agent's edition does not establish usability,
and unread or deselected roots leave effective resolution incomplete.

Inventory provenance alone does not authorize a write. Repair and uninstall
require a matching ownership receipt and recheck the link's raw target and
object type immediately before acting. An identical unowned link is never
adopted. Install refuses occupied paths and plans that OpenCode could not
resolve. Forget Source refuses active or unreadable receipted links.

## Unavailable metadata

If private metadata cannot be opened or read safely, the interactive application
opens a degraded read-only inventory and shows the database path and failure.
It retains independently readable selection and source data and does not treat
missing registry knowledge as proof that a link is unmanaged. Mutation actions
are unavailable in this session. CLI mutations fail rather than proceeding
without metadata. Fix the reported metadata problem and restart to recover.
