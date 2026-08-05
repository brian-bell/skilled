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
browsing of registered repositories, skill variants, and structured details,
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
  `nav_disabled`) are recorded on their doc comments.
- `src/viewport.rs`: responsive viewport classes and workspace region geometry.
  Screens ask whether the terminal is `Compact` or `Wide` instead of comparing
  raw widths, and the detail region's two width tiers are decided here rather
  than by the screen that draws into it.
- `src/components.rs`: pure shared primitives — status badges, list rows, pane
  headers, empty states, segmented setup progress, the modal dialog frame and
  footer regions, and the key-hint bar.
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
threshold is lowered.

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
