# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd prime` for full workflow context.

> **Architecture in one line:** Issues live in a local Dolt database
> (`.beads/dolt/`); cross-machine sync uses `bd dolt push/pull` (a
> git-compatible protocol), stored under `refs/dolt/data` on your git
> remote — separate from `refs/heads/*` where your code lives.
> `.beads/issues.jsonl` is a passive export, not the wire protocol.
>
> See [SYNC_CONCEPTS.md](https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md)
> for the one-screen overview and anti-patterns (don't treat JSONL as the
> source of truth; don't `bd import` during normal operation; don't
> reach for third-party Dolt hosting before trying the default).

## Project Status

Skilled is an early Rust 2024 and Ratatui terminal application for inspecting
and eventually managing global coding-agent skills. Implemented so far:
prototype-aligned seven-step first-run setup with segmented progress, agent
detection and selection, SQLite-backed setup persistence, local Git source
registration with catalog confirmation, responsive three-region Sources
browsing of registered repositories, catalog-grouped skill variants, and
structured details,
a read-only installation inventory of the documented native agent skill roots
with per-agent status, detail, filtering, and health findings, shared-dialog
Settings setup reset, reducer-owned contextual help, responsive size handling,
and guarded terminal restoration.

Doctor findings, Updates, and every filesystem or network mutation beyond the
private metadata database are not implemented yet. Do not turn the current
placeholders into behavior unless the active Beads issue places that work in
scope, and do not display a count, finding, status, or key hint the code
cannot currently produce.

Truthfulness is a hard requirement of the inventory in particular. The scanner
keeps apart what it read, what it could not read, what does not exist, what it
was never asked to look at, and what it has not looked at yet; and it keeps
"came from no registered source" apart from "could not tell". Every rendered
count and phrase must follow those distinctions rather than flattening them.
A count may only be shown when every root it covers was read or found absent
and at least one of those roots was actually read: finding every root absent is
a complete answer about the roots but not a measurement of their contents, so
it earns a phrase rather than a zero. That decision lives in
`InventorySnapshot::stated_skill_count`, and every surface summarising the
roots — the navigation tab as much as the inventory subtitle — asks it instead
of re-deriving the rule. A root that could not be read in full contributes
nothing and says why; a stray file beside the skill directories is listed as
not a skill rather than counted as a broken installation; a symbolic link is
claimed as managed only when it resolves by canonical path to a registered
source variant.
[GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
product and technical source of truth. The tracked `spec/tui-prototype.html` is
the visual design reference.

## Build and Test

The project requires stable Rust 1.97 or newer.

```bash
cargo run
cargo build --release
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Tests use temporary homes and application-data directories. Never point a test
at the real user home or real agent skill roots. Ratatui layouts are verified
two ways: Insta snapshots under `tests/snapshots/` capture the text of a whole
screen, and `tests/tui_shell.rs` asserts styles cell by cell against the
rendered buffer. A status or focus signal needs both, because colour alone is
not an acceptable cue.

## Application Architecture

- `src/app.rs`: application state, actions, pure reducer transitions, and typed
  persistence effects.
- `src/agents.rs`: Claude Code, Codex, and OpenCode adapters plus non-executing
  root and executable detection. Agent path conventions and documentation
  snapshots belong here.
- `src/source.rs`: local Git source inspection, catalog discovery, and skill
  candidate validation.
- `src/inventory.rs`: read-only scan of the three native agent skill roots.
  Classifies immediate children only — it never recurses, never spawns a
  process, and never writes. Resolution to a registered source is canonical
  path equality against that source's included catalog candidates and nothing
  else, so content that merely resembles a variant is never adopted. Owns the
  stable finding codes, the per-root and per-row state vocabulary, and the
  single count-or-phrase verdict every summarising surface defers to.
- `src/validation.rs`: portable `SKILL.md` front-matter validation.
- `src/store.rs`: private versioned SQLite metadata and migrations. Newer
  unknown schemas fail closed.
- `src/theme.rs`: semantic presentation tokens translated from the prototype
  palette. Every colour in the application is defined here; a screen asks for a
  role such as `Tone::Warning` or `nav_active()` rather than naming a colour. A
  test enforces that `Color::` appears in no other module. Information-bearing
  text must meet WCAG 4.5:1 against its surface; a theme unit test guards
  every such role, and the two accepted `FAINT` exemptions (`empty_glyph`,
  `nav_disabled`) are recorded on their doc comments. A new surface reuses an
  existing one where it can rather than minting a tint: `group_label()`, the
  line that names the catalog a run of variants belongs to, is muted text on
  the same band the persistent chrome uses.
- `src/viewport.rs`: responsive viewport classes and workspace region geometry.
  Screens ask whether the terminal is `Compact` or `Wide` instead of comparing
  raw widths, and the detail region's two width tiers are decided here rather
  than by the screen that draws into it.
- `src/components.rs`: pure shared primitives — status badges, list rows, pane
  headers, empty states, segmented setup progress, the modal dialog frame and
  footer regions, and the key-hint bar. A row may be several lines tall:
  `list_row_lines` is the primitive, and `list_row` is the one-line case of it,
  so a multi-line entry bands and marks itself exactly as a flat row does.
- `src/tui.rs`: composes the persistent shell (title bar, navigation, session
  status, workspace, Setup and Settings dialogs, contextual help, key hints)
  from those primitives. Pure: it does not access SQLite, the filesystem, or
  the terminal event source.
- `src/input.rs`: contextual key-event to action mapping.
- `src/runner.rs`: terminal event loop and effect execution boundary.
- `src/terminal.rs`: Crossterm raw-mode/alternate-screen ownership and
  restoration guards.
- `src/paths.rs`: injectable home, application-data, and executable search
  paths.

Keep `update` free of filesystem and database work. New external work should be
represented as typed effects and performed outside the reducer; the
installation scan runs as `Effect::ScanInstallations` for this reason. Keep agent
conventions behind adapters rather than spreading paths or enablement rules
through UI and scanner code. Build new screens from `components` primitives and
`theme` tokens rather than ad hoc styles. The title-bar and key-hint rows are
painted on `theme::chrome_band()` beneath their text, so the shell reads as
band, navigation surface, and workspace. The key-hint bar and the navigation
row are contracts: a hint or destination may only appear when `src/input.rs`
actually handles it in that context, and a navigation count may only appear
when the data behind it supports one. Sources counts the registry, which is
always fully known; Inventory asks the snapshot; and a destination this release
cannot open renders no count at all, where the prototype fakes one. Production
dependencies require explicit review.

The inventory table departs from the prototype in several recorded ways. Its
column headings are uppercase as in the prototype's grid head, but muted rather
than faint, because a heading that names a column is information-bearing and has
to meet 4.5:1. Its Skill and Source columns stop growing at 36 and 24 cells, so
a very wide terminal ends the table after Health and leaves slack instead of
stretching the identity columns around short labels; the prototype's grid grows
the same columns without bound, and a name longer than the cap is still read in
full in the detail region beside the table. The selected row's highlight band
crosses that slack all the same — a band stopping at the health badge would read
as a row ending mid-region. And in the Source column a label that places content
with at least one registered installation — a source name, `mixed`, `multiple
sources` — stays body text while `not registered` and `unverified` are muted,
which narrows the prototype's blanket muting of every source cell; the shared
style does not merge two answers the words keep apart.

The detail region beside that table records departures of its own. Its section
kickers are uppercase as the prototype sets them but muted rather than faint,
for the same reason the table headings are: a kicker that names the section
under it is information-bearing and has to meet 4.5:1. That is
`theme::detail_section_title()` and not `section_title()`, so cyan stays
reserved for focus and selection accents inside a pane; the Sources detail
sections — REPOSITORY, CATALOG, VARIANT — read in the same language, because
one kicker style across the detail regions is the point. Each per-agent section
heading carries that agent's own health badge, standing in for the prototype's
tone-coloured `.path-line` left borders: a terminal has no border to tone, so
the tone moves into the words. The cost is accepted rather than hidden — for a
row installed under a single agent the same badge appears under the title and
again in that agent's heading, beside the table's Health column. The section
leads with the skill name in `pane_heading()` and a bare badge, dropping the
`Name:` and `Health:` field labels, because the badge words already say what
they mean and the labels only repeated column headings the table has just
shown; the Details pane header still names the same skill, and that repetition
is deliberate, since the header is the focus contract and the title belongs to
the section anatomy. The region is painted on its own `DETAIL_SURFACE`
background before the text margin, so the surface reaches edges the text does
not, in the wide aside and the compact drill-in alike — the prototype keeps
`.detail-pane`'s background in its narrow media query too.

Where the prototype fixes that aside at 400px, the region has two width tiers:
40 columns, and 50 from a workspace of 151 columns up. The threshold is 151 and
not the ~140 the issue suggested because 151 is the least width at which the
table's 36- and 24-cell identity caps bind on both sides of the crossing, so
every table column is identical either side of it. At 140 the wider aside would
take ten columns the table was still using, and widening the terminal from 139
to 140 would ellipsize skill names that fit just before — the opposite of what
widening should do. That reasoning is recorded on
`DETAIL_REGION_WIDE_THRESHOLD` and pinned by a rendering test that fails if the
threshold is lowered. The split is one geometry for every screen so the aside
does not jump between tabs, and the Sources panes are bounded for the same
reason the table's columns are, so the crossing costs them slack there too: the
Repositories pane is capped at `REPOSITORIES_PANE_MAX_WIDTH`, and the variants
pane lays its content out to at most `VARIANTS_CONTENT_MAX_WIDTH`, which is the
width it keeps on the far side of the crossing.

The Sources regions are drawn on the same unboxed scaffold as the inventory
table, and record their own departures. `render_pane_scaffold` gives a pane its
header line, the rule that closes it, and the body beneath; regions are divided
by a column of vertical rule, and a region that opens on one is set in from it
by a single column of gutter. The gutter belongs to the rule rather than to the
pane, so a region at the screen edge keeps none and the detail region — whose
scaffold is that same separator plus a one-column margin — reads as one anatomy
with the panes beside it rather than a second one. Bounding a header is one shared
`pane_header` helper: the subtitle is cut to what the pane can hold, because a
status cut mid-word says neither what it is nor that there was more of it,
while the heading that names the pane is never cut. The Repositories pane takes
42% of a narrow primary region and stops at 34 columns, which binds from a
primary of 81; the wide-detail crossing takes the primary from 110 columns to
101, so the pane is 34 either side of it and every repository entry is laid out
identically. The variants pane keeps the 65 columns that are left there — 101
less the pane, the rule, and the gutter — and bounds its content to that rather
than to its own width, so widening past the threshold takes columns out of
slack and never out of a catalog path or a variant name that was readable a
column earlier. A variant name stops earning width at the same cap a skill name
does in the inventory table, and the detail region still gives it in full.

A repository entry is the prototype's three-line `.source-row`: what the source
is called, the checkout it names, and the state it was last seen in beside
`branch@short head`. It is built from `components::list_row_lines`, so the
focus marker repeats down all three lines — a marker beside the first alone
would say where the entry starts, not how far it reaches — and the selection
band crosses every one of them. Each line is bounded to the pane rather than
wrapped: a wrapped path would push one entry's state line into the next and
leave the list without a fixed entry height, so it could no longer be windowed
or banded. The path line is muted where the prototype's `.source-path` is
faint, for the reason the table headings are: it names the checkout the entry
stands for. Variants are grouped under their catalog, with each catalog's own
scan failure beneath its own label rather than stacked above the whole list,
and an empty catalog saying `no variants` rather than showing a bare label.
Every rendered row is a focus position — each candidate, and each catalog's
state row — so the selection can rest on an error or an empty catalog as well
as on a variant, the window follows it, and a source with more rows than the
pane holds keeps every one reachable whatever mixture it is; the Details
CATALOG section follows whichever row the band is on. The
label gives the catalog's path first and then whichever qualifiers — its
classification, and which agents it is registered for — the pane can hold
whole, shedding the classification first because a claim of every agent or of
one named agent is the more specific fact. Chosen that way the label can only
ever say more as the pane widens, which is the promise
`DETAIL_REGION_WIDE_THRESHOLD` makes for the table's columns. A shed qualifier
leaves no mark where a shortened path leaves an ellipsis, and nothing on the
line claims the qualifiers were stated; the detail region gives both facts in
full under CATALOG. Two departures from the prototype's `.catalog-title`: its
path and qualifiers are set `space-between`, hard to either edge, and here they
are adjacent, because the pane's slack is what a selected row's band crosses
and a label split across it would read as two columns the rows beneath do not
have. And its band is a surface rather than a cue — barely above the terminal
background, as `#0b1016` is against `#0b0f14` in the prototype — so what
separates a label from its rows is that the label is muted and starts flush
where the rows are indented past their marker column. A window scrolled deep
into a long group pins that group's label to its own first row, because rows
read without it name a variant without naming the catalog it came from, which
is the question the per-row path used to answer.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

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
