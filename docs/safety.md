# Safety contracts

For maintainers changing observation, resolution, planning, apply guards, or
verification. These are implementation constraints, including known race
boundaries; preserve them when modifying the modules named here. See the
[module map](architecture.md) and [user guide](usage.md) for navigation and usage.

## Mutation scope

Install creates one directory symbolic link per agent and may create the
agent's documented skill root only when its parent already exists. Occupied
install paths refuse. Repair replaces one proven link without recreating an
absent link or root. Uninstall removes only an exact receipted link. Forget
Source deletes private metadata after proving every described link inactive;
it never deletes a checkout or skill content.

Before a pending destructive metadata migration, the store creates one
consistent, uniquely named SQLite backup beside the database. An occupied
backup path is never overwritten. Unknown newer schemas and read-only stores
cannot be used as writable metadata. Interactive startup degrades to read-only
inventory when metadata is unavailable, retaining independently recovered
selection and registry data and stating what remains unknown.

An explicit update check can store Git objects and publish the upstream
remote-tracking ref. The confirmed fast-forward is the only update operation
that writes the checkout's worktree. Opening Updates never fetches.

## State, rendering, and truthful observations

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

## Confirmation and link mutations

- No planned install, repair, uninstall, forget, or fast-forward mutation runs
  until a plan the user has seen in full is confirmed, and
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
  the repair rather than falling back to a destructive rename. Windows stays
  remove-then-create — it cannot rename over an existing directory link — but
  the removal is handle-bound: the destination is pinned with
  `FILE_FLAG_OPEN_REPARSE_POINT` and delete sharing denied, proven through
  that handle to be a symbolic link byte-identical to the proven target —
  the same equivalence the Unix exchange applies to its displaced object —
  and deleted through the same handle with a POSIX-semantics disposition, so
  a stranger's object arriving after the recheck is refused rather than
  deleted, a filesystem that refuses that disposition refuses the repair,
  and what remains is the install-class fail-if-exists window at creation.

- `--yes` removes the confirmation and nothing else. Install requires
  `--source`, `--skill`, and `--agents` explicitly; uninstall and repair each
  require `--skill` and `--agent`; update requires `--source`. Every ownership,
  collision, path, apply, rescan, and verification gate still runs, and a named agent the plan cannot
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

## Repository updates

### Checkout identity and process binding

Repository updates write through Git only inside the canonical checkout the
user registered. They begin only after an explicit check, fast-forward to the
exact previewed object, and never reset, rebase, stash, commit, or push. The
check and the apply each pin the checkout first — the directory is opened
once, its identity is proven through that handle, and every Git process they
run enters the held directory with `fchdir(2)` before executing — so a
checkout renamed or replaced under the pathname between any guard and any
spawn changes what the pathname names, never what the processes read or
write.

The pathname is still re-read immediately before `merge`: a proven
repository that is no longer at the path the user confirmed is refused
rather than written wherever it went, and a rename inside that last gap
loses only the refusal — the write lands in the proven repository, not an
impostor's.

Bound spawns also pin what discovery would re-decide inside the
held directory (`GIT_DIR=.git`, `GIT_WORK_TREE=.`): a deleted `.git`
refuses rather than walking up into a parent repository, and a
`core.worktree` written afterwards cannot move the merge's writes. The
`.git` object itself stays name-resolved — Git accepts no
descriptor-pinned repository — and the argument for that boundary lives on
`git::RepositoryHandle`. Verification re-reads identity by pathname
afterwards, as the observation it always was. On platforms without
`fchdir` the handle spawns by pathname as before, and the guard-order
narrowing is what remains.

### Inspection and transport policy

The explicit check suppresses hooks and monitors and refuses observed
checkout-configured transport programs, within the concurrency boundaries below.
Hooks are pointed at the null device, `core.fsmonitor` is turned off on every inspection — it is an
executable Git runs for `status` and `fetch` alike, and `core.hooksPath` does
not reach it — and the partial-clone refusal is re-asked before the preview's
object reads and again in the apply guard, rather than remembered from the
check, so neither can make Git fetch lazily. The reads that follow the write
are covered by that refusal too, asked once more at the boundary between the
fast-forward and the first read after it: the plan discloses that the merge
may run the checkout's hooks, a `post-merge` hook can configure a promisor
remote, and refreshing the registered source (`status` and `cat-file`) and
reading HEAD and the worktree for verification would then fetch. Only those
repository-dependent postconditions are withheld — the agent roots are always
scanned and the disclosed installations always compared, because walking a
filesystem cannot make Git fetch anything. The withheld evidence names which
of the three cases it was: a marker observed, an inspection that could not
answer, or a write the guard refused before it ran. A report that established
no repository postcondition is incomplete, which no surface may reduce to a
pass, the exit status included.

The transport half of the claim is a
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
guessing at their intent.

A refusal is a read, though, and the fetch it
guards is a later process, so the refusal is re-asked immediately before the
fetch rather than remembered from the top of the probe — and the two settings
that can be bound instead of trusted are. Git is handed the transport
allowlist itself as `GIT_ALLOW_PROTOCOL`, which overrides any `protocol.*`
permission the checkout grants itself, so a helper URL, an `insteadOf`
rewrite, or a `remote.<name>.vcs` written in after the refusal reaches no
program.

That list is a ceiling and never a grant: it overrides the user's
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

### Fetch and ref publication

The fetch writes no ref at all. Git dereferences a symbolic ref when it
updates one, so any ref the fetch wrote — the tracking ref or a staging
name — could be substituted between a check and Git's own transaction and
send a forced refspec into whatever it points at, a local branch included.
The fetch therefore runs with `--dry-run`, which stores the objects and
skips every ref update, and the fetched object comes back through
`--porcelain`'s report under a per-invocation `refs/skilled/fetch/` name
that is only ever a name.

The tracking ref is then published from the
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

### Cached findings and legacy registrations

Cached update findings exist only after an explicit check. A changed `HEAD`
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

### Preview and verification

Repository update verification keeps the same three answers as installation
verification. The confirmation gate covers the complete plan statement, the
incoming commit summaries included, because those are what the fast-forward
brings in; the untruncated changed-file listing is non-gating evidence below
it. Where the fast-forward *started* is verified too, not only where it
landed: `merge --ff-only` takes no expected-current-revision, so a branch
another process moved between the guard and the write can land on the
previewed object having applied a range nobody was shown. The log entry the
merge left on the branch names its starting point, and a start other than
the previewed revision is a verification failure; a repository that logs no
reference updates leaves the check withheld rather than passed.
A disclosed removal is verified as the same link, raw target included,
and not merely as some dangling entry under the same name. Every other
installation is held to the same test: a fast-forward writes inside the
repository and nowhere near an agent root, so a link whose raw target changed
was rewritten by something outside the plan, and health and resolution cannot
see it when the new target reaches the same variant by another route. Repair
proves ownership by comparing a raw target against a receipt byte for byte,
so a retarget passed off as verified costs that link its evidence too.

### Installation aliases

An update's affected installations are decided by the variant each
installation resolves to, never by the name a root holds. A link installed as
`alias` pointing at skill `demo` is matched under `demo` and disclosed as
`alias`: matching on the root entry's own name would leave it out of the
preview and let verification report its lost resolution, after the write, as
a regression nobody was shown.

### Removals, type changes, and invalidation

A candidate is a skill by virtue of its `SKILL.md`, not its directory.
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
would do. One object is the exception, because there the outcome is exact
rather than unknown: a regular file the target revision keeps at the candidate
itself. The link keeps precisely the target it has and that target stops being
a skill, so the plan states the type change on its own line — `target stops
being a skill · <name>`, never as a removal — and verification holds the
update to the same link resolving to content no agent can load. A symbolic
link and a submodule stay refusals: Git records a link as a blob too, and
where it leads is exactly what the plan cannot state. The worktree is still
asked for a disclosed type change, because an occupant both keeps the
directory standing and stops Git writing a file at that path at all.
The worktree half is a live read, so the apply guard asks it again
over the worktree as it then stands: an occupant that arrived after the
preview refuses the write rather than being applied. Under the apply guard
that walk is descriptor-bound on Linux and macOS — it descends from the pinned
checkout's held directory with `openat(2)` and never consults the pathname,
so a checkout renamed aside and restored around it cannot clear the guard
with a vacant decoy, and a worktree ancestor that is a symbolic link
refuses rather than being followed. `cached_update_check`
decides it too, as it already does for the incoming-collision and submodule
findings — the cached check is what Updates advertises and what Doctor reads,
so a finding only the preview raises would leave the list offering an update
the preview then refuses. It is one local Git process per candidate the
update touches, so it takes the check's cancellation flag and returns no
answer at all rather than a partial one: a cancelled analysis records no
check. Both `Effect::CheckUpdates` and `Effect::PlanRepositoryUpdate` rescan
the roots first, because a check and the preview that follows it decide the
same installation-dependent findings and a link made while the application
stayed open would otherwise be in one and not the other.

The confirmed apply
rescans them a last time and refuses a plan whose affected installations no
longer match that reading — the dialog and the typed command alike — because
the preview's set was read before the confirmation waited, and a link created
in that window would otherwise be found only by the post-write scan, once the
repository had already moved.

A document the target
revision keeps but no longer validates is read too: an installation whose
`SKILL.md` fails the portable core there is disclosed as not loading at the
target revision rather than as updated in place, under the name its own root
holds, because that is the name the scan compares the document's against. That
one is stated rather than blocked — upstream's broken front matter is
upstream's to fix, and refusing every later fast-forward would strand the
checkout — so the preview reads it and the cancellable check does not: a
disclosure cannot leave the check offering an update the preview then refuses,
which is the only reason the collision, submodule, and occupant findings are
decided twice. Verification holds the update to the finding code the preview
named, so a different breakage under the same name is still undisclosed. A
rename names the installations it leaves without a target alongside the pair
of skill names, because a link installed under a name of its own is not named
by the pair and verification holds it to that outcome regardless.

### Missing tracking refs

A configured upstream whose remote-tracking ref is absent is not an
unconfigured one. `Upstream::revision` is optional for exactly that state,
and the explicit check fetches it rather than reporting `source.no_upstream`
and leaving the repository unable to update until the user fetched by hand.

## Terminal output and verification

- Text from the filesystem — names, paths, link targets, operating-system error
  messages — is escaped through `components::terminal_safe` before it reaches a
  terminal, on every surface. The screens and CLI commands write to the same
  terminal by different routes and both go through it.

- Verification has three answers, not two. `VerifyReport::is_verified` means
  nothing disagreed with the plan; `is_complete` means every postcondition was
  also checked. A root the scan could not read leaves its check withheld, which
  no surface may report as a pass. This is the inventory's own rule applied to
  the operation that follows it. The exit status is such a surface: `skilled
  update`, `skilled install`, and `skilled repair` all report an incomplete
  verification as its own status rather than as success, because a script
  reads only that. A check the user's own agent selection precludes — the
  ancillary OpenCode resolution over a deselected root — is not incomplete on
  that surface: `VerifyReport::is_complete_for_selection` draws the same line
  `counts_are_complete` does, where a deselected root is complete scope and an
  unreadable one is not, so the ordinary sub-three-agent configuration keeps
  exiting `0`.
