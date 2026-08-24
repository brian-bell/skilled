# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd prime` for full
workflow context; the managed Beads sections at the end of this file cover
commands, the sync model, and the session-close protocol.

## Project Status

Skilled is an early Rust 2024 / Ratatui terminal application for inspecting
and managing global coding-agent skills. First-run setup, local Git source
registration, Sources browsing, a read-only installation inventory, OpenCode
effective resolution across its documented roots, a read-only Doctor findings
view, degraded read-only startup when private metadata is unavailable,
installation, receipt-backed repair of incorrect or dangling links,
guarded uninstall, metadata-only Forget Source, and explicit repository update
checks with guarded fast-forwards — each previewed, confirmed, rescanned, and
verified — are implemented.

Filesystem mutation stays narrow. Installation creates one directory symbolic
link per agent and may create the documented skill root when its own parent
already exists; every occupied install path is a refusal. Repair replaces one
observed symbolic link only when its raw target is byte-identical to the newest
matching ownership receipt and the live registry supplies a safe replacement;
it never recreates an absent link or root. Uninstall removes only an exact
receipted link after rechecking its type, target, root, and ownership. Forget
Source deletes private registration/catalog/receipt metadata only after proving
every described link inactive; it never deletes a checkout or skill content.
A pending destructive metadata migration first creates one consistent, uniquely
named SQLite backup beside the database; an occupied backup path is never
overwritten. Updates perform network access only after an explicit check and
write only through the guarded repository fast-forward. Adoption of unproven
links and network operations beyond that explicit update check remain
unimplemented: do not turn their placeholders into behavior unless the active
Beads issue places that work in scope, and do not display a count, finding,
status, or key hint the code cannot currently produce.

[GitHub issue #3](https://github.com/brian-bell/skilled/issues/3) is the
product and technical source of truth. The tracked `spec/tui-prototype.html`
is the visual design reference. Design rationale — including every recorded
departure from the prototype — lives as doc comments on the constants and
functions that implement it, not in this file; read the module you are
changing before overriding a bound, style, or phrase it documents.

## Build and Test

Requires stable Rust 1.97 or newer.
Repository updates require Git 2.41 or newer — the explicit check reads its
fetch result from `git fetch --porcelain`, which 2.41 introduced — and, for
SSH remotes, OpenSSH 8.4 or newer.

```bash
cargo run
cargo run -- install --source <id-or-path> --skill <name> --agents claude-code
cargo run -- uninstall --skill <name> --agent claude-code
cargo run -- repair --skill <name> --agent claude-code
cargo build --release
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Tests use temporary homes and application-data directories; never point a test
at the real user home or real agent skill roots. Ratatui layouts are verified
two ways — Insta snapshots under `tests/snapshots/` capture a screen's text,
and `tests/tui_shell.rs` asserts styles cell by cell — and a status or focus
signal needs both, because colour alone is not an acceptable cue.

## Architecture

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
  guards, rescans, and
  verification the TUI runs, with distinguishable exit statuses.
- `src/resolution.rs`: pure per-agent variant selection and OpenCode effective
  resolution; decides which registered variant an agent resolves a name to and
  what OpenCode would load, over data the caller already holds. It states no
  findings — `inventory.rs` maps its verdicts to codes and severities.
- `src/validation.rs`: portable `SKILL.md` front-matter validation.
- `src/store.rs`: private versioned SQLite metadata and transactional
  migrations; newer unknown schemas fail closed, a store SQLite opened
  read-only is refused rather than treated as writable, and destructive
  migrations create a recoverable backup before any pending step runs.
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
- `src/main.rs`: no arguments runs the interactive application; anything else is
  a command, reported through an exit status.

## Invariants

- Keep `update` free of filesystem, process, and database work: external work
  is a typed `Effect` performed by the runner (see `Effect::ScanInstallations`
  and the snapshot reset in `enter_inventory`).
- The reducer is geometry-blind, and stays that way. What only the renderer can
  measure crosses back the other way: `tui::render` returns a `RenderFeedback`
  and the runner notes it before reading the next key. `None` there means the
  frame did not draw the thing, which is not the same as measuring zero.
- Truthfulness is a hard requirement of the inventory. Every summarising
  surface asks `InventorySnapshot` — `stated_skill_count`, `scan_pending`,
  `no_agent_configured` — rather than re-deriving what may be claimed, and
  the scanner's distinctions (read, unreadable, absent, not selected, not
  scanned yet; "not registered" apart from "could not tell") are never
  flattened into one another.
- Every colour comes from a `theme.rs` role; a test enforces that `Color::`
  appears in no other module, and information-bearing text must meet WCAG
  4.5:1 against its surface.
- Build screens from `components` primitives and `theme` tokens; keep agent
  conventions behind the `agents.rs` adapters.
- A variant's stored compatibility set records which agents *discover* a
  catalog root, not which can use the edition in it: OpenCode reads Claude
  Code's and Codex's roots, so a `.claude/skills` catalog is registered for
  OpenCode while holding another agent's edition. Anything deciding usability
  asks `VariantRef::usable_by`, which reads the catalog's own path through
  `AgentAdapter::owns_source_catalog`.
- Key hints and counts stay truthful: a key-bar hint appears only when
  `src/input.rs` handles it in that context, and a count only when the data
  behind it supports one. A tab's route digit is caption rather than hint —
  every available destination shows its number, the active view's included,
  where pressing it is simply inert; a destination this release cannot open
  still shows none. Counts render as `·N` so a bare amber digit cannot read
  as the next tab's route key — the prototype separates the two classes by
  colour alone, which a terminal may not rest on.
- Nothing is written until a plan the user has seen in full is confirmed, and
  that is enforced rather than assumed: a preview taller than its dialog scrolls
  rather than dropping what it cannot hold, and neither the reducer nor the
  footer offers a confirmation until its last row has been on screen. A plan
  blocks whole: one blocked target and nothing is written anywhere. Every
  target's absolute path is stated unabbreviated — the `~` spelling the rest of
  the application uses would soften the thing being agreed to.
- Installation writes only inside a root it established. A skill root, or any
  directory between it and the home directory, that is a symbolic link is
  refused rather than followed: the path the preview stated has to be the path
  the write lands on. Install retains a fail-if-exists pathname window between
  its check and `symlink`, recorded on `apply_install` and `apply_uninstall` and
  tracked as `skilled-cb2`. Repair replaces through an atomic exchange: the
  displaced object must be byte-identical to the proven link before it is
  removed, a stranger's object is swapped back intact and refused — or, if the
  swap back itself fails, preserved at a reported temporary path while the
  repair reports as partial — and a filesystem that cannot exchange refuses
  the repair rather than falling back to a destructive rename. Windows keeps
  its documented non-atomic remove-and-create fallback, tracked as
  `skilled-tdm`.
- Repository updates write through Git only inside the canonical checkout the
  user registered. They begin only after an explicit check, fast-forward to the
  exact previewed object, and never reset, rebase, stash, commit, or push. The
  check and the apply each pin the checkout first — the directory is opened
  once, its identity is proven through that handle, and every Git process they
  run enters the held directory with `fchdir(2)` before executing — so a
  checkout renamed or replaced under the pathname between any guard and any
  spawn changes what the pathname names, never what the processes read or
  write. The pathname is still re-read immediately before `merge`: a proven
  repository that is no longer at the path the user confirmed is refused
  rather than written wherever it went, and a rename inside that last gap
  loses only the refusal — the write lands in the proven repository, not an
  impostor's. Bound spawns also pin what discovery would re-decide inside the
  held directory (`GIT_DIR=.git`, `GIT_WORK_TREE=.`): a deleted `.git`
  refuses rather than walking up into a parent repository, and a
  `core.worktree` written afterwards cannot move the merge's writes. The
  `.git` object itself stays name-resolved — Git accepts no
  descriptor-pinned repository — and the argument for that boundary lives on
  `git::RepositoryHandle`. Verification re-reads identity by pathname
  afterwards, as the observation it always was. On platforms without
  `fchdir` the handle spawns by pathname as before, and the guard-order
  narrowing is what remains.
- An explicit check runs no repository code. Hooks are pointed at the null
  device, `core.fsmonitor` is turned off on every inspection — it is an
  executable Git runs for `status` and `fetch` alike, and `core.hooksPath` does
  not reach it — and the partial-clone refusal is re-asked before the preview's
  object reads and again in the apply guard, rather than remembered from the
  check, so neither can make Git fetch lazily. Reads that follow the write are
  not covered; `skilled-cbq` has that. The transport half of the claim is a
  refusal rather than a suppression: a checkout that names a program for Git
  to run while fetching — `core.sshCommand`, `core.askPass`, `core.gitProxy`,
  `core.alternateRefsCommand`, a credential helper,
  `remote.<name>.uploadpack`, `remote.<name>.vcs`, `protocol.<scheme>.command`,
  or a URL naming a transport helper — blocks the check with
  `source.repository_transport_unsupported` instead. The URL is the one
  `ls-remote --get-url` reports, because `insteadOf` rewrites the configured
  value on the way to the transport and a remote with several URLs is fetched
  from the first while `--get` answers with the last. A setting the checkout
  goes on to disable is not a refusal: an empty credential helper resets the
  list and a scalar's last value wins. Scope is the
  whole distinction, and `--show-scope` is what draws it: the same key in the
  user's own global or system configuration is theirs and keeps working, while
  the same key inside the checkout is refused. There is no documented way to
  turn a credential helper or an upload-pack program off the way there is for
  a hook, and reconstructing which of the user's scopes was meant would be
  guessing at their intent. A refusal is a read, though, and the fetch it
  guards is a later process, so the refusal is re-asked immediately before the
  fetch rather than remembered from the top of the probe — and the two settings
  that can be bound instead of trusted are. Git is handed the transport
  allowlist itself as `GIT_ALLOW_PROTOCOL`, which overrides any `protocol.*`
  permission the checkout grants itself, so a helper URL, an `insteadOf`
  rewrite, or a `remote.<name>.vcs` written in after the refusal reaches no
  program. That list is a ceiling and never a grant: it overrides the user's
  `protocol.*` policy as readily as the checkout's, so what is handed over is
  the ceiling narrowed by an inherited `GIT_ALLOW_PROTOCOL` and by the user's
  own policy — `protocol.file.allow=never` is a hardening people apply, and
  re-enabling it would be the bypass. Git's three states are all kept: `user`
  is not `always`, so `GIT_PROTOCOL_FROM_USER=0` still refuses the transports
  that sit at that policy, `file`, `ftp`, and `ftps` by default among them; a
  policy value Git would abort on refuses rather than permits; and the
  inherited list is split and matched exactly the way Git matches it, with an
  unreadable one leaving nothing. Narrowing to nothing is a real answer there
  rather than a reason to fall back. The checkout's scopes are left out
  of that reading, so a repository can neither widen the ceiling nor deny
  someone else's fetch. `core.sshCommand` is the one setting Skilled reads and exports
  itself, so it is read with `--show-scope` at the moment of use and the
  checkout's scopes are struck out of the answer, whenever the value arrived.
  The rest are read by Git out of the repository configuration when the fetch
  starts, and closing that gap by reproducing each vetted value as a `-c`
  override is `skilled-88j`. The fast-forward is the opposite case by design:
  it is handed the repository's configuration, and the plan discloses the
  hooks, the monitor, the checkout filters, and the signature program it may
  run. That last one is disclosed rather than suppressed on purpose:
  `merge.verifySignatures` is a policy, and passing `--no-verify-signatures`
  would overrule it wherever the user set it — fast-forwarding to an unsigned
  tip Git had been told to refuse. Re-reading the object the preview named
  settles which object is merged, not who vouches for it.
- The fetch writes no ref at all. Git dereferences a symbolic ref when it
  updates one, so any ref the fetch wrote — the tracking ref or a staging
  name — could be substituted between a check and Git's own transaction and
  send a forced refspec into whatever it points at, a local branch included.
  The fetch therefore runs with `--dry-run`, which stores the objects and
  skips every ref update, and the fetched object comes back through
  `--porcelain`'s report under a per-invocation `refs/skilled/fetch/` name
  that is only ever a name. The tracking ref is then published from the
  reported object with `update-ref --no-deref` and an expected old value, so
  a ref another fetch advanced meanwhile is refused rather than rolled
  back — unless it already holds the very object that was reported, which is
  nothing to refuse. `--no-deref` confines that one write to the named ref;
  what it cannot do is refuse a ref made symbolic inside the final
  check-to-write gap, because Git offers no single operation that asserts a
  ref's kind and value together — such a ref is replaced in place, its
  referent untouched, and a ref that was symbolic any earlier refuses the
  check twice over. The argument for that residual is recorded on
  `git::fetch_upstream` and in the `skilled-q59` closeout.
- Cached update findings exist only after an explicit check. A changed `HEAD`
  or changed known dirtiness supersedes the cached verdict; opening Updates is
  therefore a metadata-only operation and never performs network access. A
  recorded verification finding is exempt: it is an observation of the state it
  would otherwise be superseded by, and Doctor must not lose it. So is
  `source.identity_unproven`, which records a fact about the registration
  rather than the checkout: no observation of the standing checkout changes
  what a fresh check would answer, and only the identity being recorded by
  re-registration — or the source becoming unreadable — supersedes it. A
  source registered before identities were recorded never adopts the identity
  of whatever stands at its path; updates against it are refused with that
  finding until the user re-registers the checkout, and every other surface
  works over the row exactly as far as it did before — a checkout still
  containing the stored head loads without user action, while one that does
  not is refused as changed, the same head-containment reading every row gets.
- Repository update verification keeps the same three answers as installation
  verification. The confirmation gate covers the complete plan statement, the
  incoming commit summaries included, because those are what the fast-forward
  brings in; the untruncated changed-file listing is non-gating evidence below
  it. A disclosed removal is verified as the same link, raw target included,
  and not merely as some dangling entry under the same name. Every other
  installation is held to the same test: a fast-forward writes inside the
  repository and nowhere near an agent root, so a link whose raw target changed
  was rewritten by something outside the plan, and health and resolution cannot
  see it when the new target reaches the same variant by another route. Repair
  proves ownership by comparing a raw target against a receipt byte for byte,
  so a retarget passed off as verified costs that link its evidence too.
- An update's affected installations are decided by the variant each
  installation resolves to, never by the name a root holds. A link installed as
  `alias` pointing at skill `demo` is matched under `demo` and disclosed as
  `alias`: matching on the root entry's own name would leave it out of the
  preview and let verification report its lost resolution, after the write, as
  a regression nobody was shown.
- A candidate is a skill by virtue of its `SKILL.md`, not its directory.
  Classification asks the target revision for that document as a regular file —
  Git records a symbolic link as a blob too, and the scanner and portable
  validation both refuse a linked skill document — so a deleted or relinked
  skill document is a removal even where a tracked file stays beside it, and a
  catalog whose skill is the repository root is no longer retained by
  definition. A candidate the update was disclosed as emptying — a removal, or
  the old side of a rename — whose path would still hold anything afterwards
  blocks with `source.removal_leaves_content`. Three ways it can: the root
  catalog, which no update removes; the target tree, which may keep an entry
  there or turn an ancestor into a symbolic link `ls-tree` will not walk
  through, redirecting the path without ever appearing at it; and the worktree,
  where anything the update does not delete stays — including an empty
  directory, which Git's untracked and ignored lists never name because they
  name files. The link would then resolve to something that is not a skill
  rather than losing its target, so the preview cannot state what the write
  would do. The worktree half is a live read, so the apply guard asks it again
  over the worktree as it then stands: an occupant that arrived after the
  preview refuses the write rather than being applied. `cached_update_check`
  decides it too, as it already does for the incoming-collision and submodule
  findings — the cached check is what Updates advertises and what Doctor reads,
  so a finding only the preview raises would leave the list offering an update
  the preview then refuses. It is one local Git process per candidate the
  update touches, so it takes the check's cancellation flag and returns no
  answer at all rather than a partial one: a cancelled analysis records no
  check. Both `Effect::CheckUpdates` and `Effect::PlanRepositoryUpdate` rescan
  the roots first, because a check and the preview that follows it decide the
  same installation-dependent findings and a link made while the application
  stayed open would otherwise be in one and not the other. Whether the document
  the target keeps is a *valid* skill is not read — `skilled-3o5` has that. A
  rename names the installations it leaves without a target alongside the pair
  of skill names, because a link installed under a name of its own is not named
  by the pair and verification holds it to that outcome regardless.
- A configured upstream whose remote-tracking ref is absent is not an
  unconfigured one. `Upstream::revision` is optional for exactly that state,
  and the explicit check fetches it rather than reporting `source.no_upstream`
  and leaving the repository unable to update until the user fetched by hand.
- Text from the filesystem — names, paths, link targets, operating-system error
  messages — is escaped through `components::terminal_safe` before it reaches a
  terminal, on every surface. The screens and CLI commands write to the same
  terminal by different routes and both go through it.
- Verification has three answers, not two. `VerifyReport::is_verified` means
  nothing disagreed with the plan; `is_complete` means every postcondition was
  also checked. A root the scan could not read leaves its check withheld, which
  no surface may report as a pass. This is the inventory's own rule applied to
  the operation that follows it. The exit status is such a surface: `skilled
  update` reports an incomplete verification as its own status rather than as
  success, because a script reads only that. Install and repair still exit `0`
  there; `skilled-exm` has it.
- `--yes` removes the confirmation and nothing else. Install requires
  `--source`, `--skill`, and `--agents` explicitly; uninstall and repair each
  require `--skill` and `--agent`. Every ownership, collision, path, apply,
  rescan, and verification gate still runs, and a named agent the plan cannot
  act on is a blocked request rather than a silent skip.
- Ownership receipts are evidence, never instructions. The scanner does not
  consult them and they outlive their source. Repair replaces a link only when
  the link's raw target is byte-identical to a receipt for that path; it never
  recreates a link from a receipt alone, and an unproven link is never adopted
  by writing one. A receipt is removed only after uninstall positively verifies
  its link gone, or Forget Source has just established the described link
  inactive.
- Uninstall never removes an agent root or follows the link it removes. Object
  type, exact receipt, recorded target, and documented-root containment are
  rechecked immediately before unlinking; one failed target stops the run.
- Forget Source removes private metadata only. Any active or unreadable
  receipted link, or any receipt-set change between preview and confirmation,
  blocks the transaction; checkout and skill directories are never deleted.
- Production dependencies require explicit review.

## Non-Interactive Shell Commands

`cp`, `mv`, and `rm` may be aliased to `-i` and hang waiting for input: always
pass `-f` (`rm -rf`, `cp -rf`). Use `-y` for `apt-get`, `-o BatchMode=yes` for
`ssh`/`scp`, and `HOMEBREW_NO_AUTO_UPDATE=1` for `brew`.

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
