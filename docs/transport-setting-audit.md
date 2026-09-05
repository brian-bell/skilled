# Explicit-check transport and executable-setting audit

Audit date: 2026-09-05. Beads: `skilled-0z9`.
Audited Skilled revision: `28771796ac8a961d860b509097ecde2974805e27`.

## Result

The current guard is not sufficient to establish that an explicit check runs
no checkout-selected programs. The audit found incorrect upload-pack
precedence, destructive normalization of configuration keys and values, and
a missing protection against configuration-based hooks in newer Git. These
are separate from the known configuration-change race in `skilled-88j`:
the configurations described below can be present before the first guard.

The remote-local `uploadpack.packObjectsHook` question is resolved: Git
ignores that setting in repository scope. The same setting in protected
global configuration does execute during local transport, as a positive
control confirmed. It should not be added indiscriminately to the client's
repository-setting refusal.

This report records an audit, not fixes. Runtime code and repository tests
were not changed. `skilled-0z9` remains open because its original scope also
requires fixes and permanent regressions. The passing existing tests do not
cover the newly identified counterexamples.

| Finding | Evidence | Follow-up |
| --- | --- | --- |
| F1: upload-pack is first-value-wins, not last-value-wins | Git source at the supported floor and current version; Git 2.50.1 execution trace | `skilled-0z9.1`, P1 |
| F2: lowercasing credential URL paths merges distinct keys | Git 2.50.1 credential execution trace and source | `skilled-0z9.1`, P1 |
| F3: proxy disabling is byte-sensitive | Git 2.50.1 trace and `connect.c`; parser comparison | `skilled-0z9.1`, P1 |
| F4: configured hooks survive the hooks-directory override | Marker executed by the publication command on Git 2.55.0 | `skilled-0z9.2`, P1 |
| F5: Windows can remove inherited guard variables | Source-established mechanism; Windows execution not tested | `skilled-0z9.3`, P2 |
| F6: bundle fetching is an additional execution/ref-publication path | Local bundle ref created during dry-run fetch on Git 2.50.1 | `skilled-d1i`, P2 |

## Scope and method

The boundary is Skilled's explicit update check and the inspection commands
that support it. It includes object transfer and the subsequent tracking-ref
publication. The confirmed `merge --ff-only` intentionally receives repository
configuration and discloses the programs it may run; its execution policy is
outside this audit's refusal requirement.

The review used:

1. Skilled's typed `UpdateOp` arguments, environment construction, synchronous
   and cancellable paths, transport parser, filter-free status, and source
   inspection boundary in [git.rs](../src/git.rs),
   [updates.rs](../src/updates.rs), and [source.rs](../src/source.rs).
2. The configuration documentation corpus in Git v2.41.0 (Skilled's minimum)
   and v2.55.0, screening command/program/shell/helper descriptions and
   following relevant entries into their consumers. Adjacent command manuals
   were included where the config corpus delegates details, notably
   credentials, attributes, archive, and hooks.
3. Git implementation source where prose does not settle precedence or
   reachability: `remote.c`, `connect.c`, `credential.c`, `prompt.c`, `hook.c`,
   `upload-pack.c`, `bundle-uri.c`, `builtin/fetch.c`, `builtin/diff-tree.c`,
   `refs.c`, and `compat/mingw.c`.
4. Isolated subprocess probes and the existing relevant Skilled test suites.

Runtime probes used macOS Git `2.50.1 (Apple Git-155)` and an independently
built upstream Git v2.55.0. The latter was built in a temporary directory
with `NO_CURL`, `NO_GETTEXT`, `NO_TCLTK`, `NO_PERL`, and `NO_PYTHON`; it was
used for local ref/hook tests, not HTTP validation. All probe repositories,
homes, config files, marker programs, and build outputs were temporary.
No real agent roots, real user credential helpers, or external Git servers
were used. Git v2.41.0 was source-reviewed, not executed.

The trusted base remains the installed Git and its bundled helpers, executable
search path, inherited user environment, and the user's global/system
configuration. Git necessarily starts programs such as `git-upload-pack`,
`git-index-pack`, `git-remote-https`, and SSH. The intended distinction is
whether the checkout selects executable code. This report is not a proof
against compromised Git, libcurl/TLS backends, malicious trusted helpers,
arbitrary shared-library loading, every vendor patch, or concurrent filesystem
replacement. Known race boundaries remain in [safety.md](safety.md).

## Operations and existing defenses

| Phase | Git operations or behavior | Existing defense and audit conclusion |
| --- | --- | --- |
| Configuration and identity | `config`, `rev-parse`, `symbolic-ref`, `for-each-ref`, `ls-remote --get-url` | Fixed built-in subcommands; URL query does not contact remote. Includes carry their Git-reported scope. |
| Worktree inspection | `status`, `ls-files`, `check-attr` | `core.fsmonitor=false`, `core.untrackedCache=false`, `GIT_OPTIONAL_LOCKS=0`; observed clean/process filters overridden to empty and required flags to false. Filter ambiguity stays unknown. |
| Object inspection | `merge-base`, `rev-list`, raw `diff-tree`, `ls-tree`, `cat-file` | No patch/textconv/signature requests; partial-clone refusal and `GIT_NO_LAZY_FETCH=1` in typed update operations. The latter is effective only in Git versions supporting it. |
| Transfer | `fetch --porcelain --dry-run` with explicit refspec | No tags, pruning, FETCH_HEAD writing, submodule recursion, or auto-maintenance; monitor and traditional hooks suppressed. Executable settings rechecked immediately beforehand. F1–F3 remain parser gaps; F6 is an auxiliary path. |
| Transport selection | effective URL, narrowed `GIT_ALLOW_PROTOCOL`, scoped SSH command | Handles first fetch URL and `insteadOf`; supplied protocol list narrows user policy. Custom helper URLs blocked. SSH chosen from non-repository scopes and exported with batch mode. |
| Ref publication | `update-ref --no-deref`, expected old value | Traditional hooks redirected; configured hook system is not suppressed on Git 2.55.0 (F4). |
| Apply | `merge --ff-only` | Deliberate disclosure/confirmation boundary; do not carry inspection suppression into it by accident. |

There are two inspection constructors: `git::command` and
`source::git_command`. A change to one is not automatically a change to the
other. Similarly, synchronous and cancellable probes must preserve identical
policy. Inspection subprocess output is captured in pipes, so configured
pagers do not get a terminal. Existing filtering protects observed settings;
concurrent introduction of new settings is not made safe merely by this audit.

## Executable and transport-setting coverage

“Covered” below describes the mechanism at the audited revision, not a claim
that the whole check is safe despite the findings. Git config canonicalizes
section and variable names, but preserves subsection case; it also preserves
quoted whitespace. Those distinctions must survive any policy parser.

| Setting/family | Git semantics and execution point | Skilled disposition |
| --- | --- | --- |
| `core.sshCommand` | Last config value; `GIT_SSH_COMMAND` takes precedence. Used for SSH connection and potentially variant probing. | Repository value refused; actual exported command independently selected by scope. Core mechanism sound; preserve bytes instead of blanket trimming. |
| `core.askPass` | Last config value, below `GIT_ASKPASS`, above `SSH_ASKPASS` when preceding variables are absent. | Refused, but fetch also sets `GIT_ASKPASS` to an empty string, preventing fallback to configured/SSH askpass. `GIT_TERMINAL_PROMPT=0` separately prevents terminal prompts. |
| `core.gitProxy` | First entry matching target domain; `GIT_PROXY_COMMAND` overrides. Exact `none` disables, including the command portion of `none for DOMAIN`. | All repository entries conservatively considered. F3: trimming/case folding misrecognizes disabling; domain-scoped `none` produces false refusals. |
| `core.alternateRefsCommand` | Last config string; shell command used when enumerating alternate object-store ref tips. | Refused. Empty/whitespace strings must not be generalized into a documented universal “off” rule. |
| `credential.helper`, `credential.<url>.helper` | Helper lists, empty resets, and URL-context matching. Multiple helpers may run until sufficient credentials exist. Subsection paths are case-sensitive. | Per-key grouping/reset logic is incomplete as a model of URL matching. F2 is a concrete bypass caused by lowercasing paths. |
| `remote.<name>.uploadpack` | **First** configured value is retained; duplicates warn. Executed locally for local transport, remotely through SSH for SSH transport. | F1: incorrectly classified as last-value scalar. |
| `remote.<name>.vcs` | Last configured string selects a `git-remote-<vcs>` helper. | Refused and protocol allowlist provides a second boundary. Empty is not a general documented helper-disable spelling; Git can attempt `remote-`. |
| `remote.<name>.url` | Ordered URL list; fetch uses first, after longest matching `url.<base>.insteadOf` rewrite. | Correctly asks `ls-remote --get-url`; rejects helper scheme and binds protocol ceiling. Preserve remote subsection case in other related parsing. |
| `url.<base>.insteadOf` | Multiple rewrite prefixes; longest matching prefix wins. Value is not a program itself, but can select a helper URL. | Effective URL plus allowlist cover selection; equal-length and unusual URL cases should stay delegated to Git. |
| `protocol.allow`, `protocol.<name>.allow` | Scalar policies (`always`, `never`, `user`), narrowed by inherited allowlist and operation origin in Skilled. | Bound for ordinary transport. Auxiliary bundle path and Windows environment stripping need separate attention. |
| `protocol.<scheme>.command` | No corresponding stock Git config consumer found in the reviewed versions. `ext::` carries its command in the URL. | Present in refusal regex and inaccurately described as an ext command setting. Remove/correct the claim with evidence; retain URL protection. |
| `core.fsmonitor` | Boolean daemon switch or executable monitor path, depending on value/version. | Explicit false override on inspections, including source inspection; existing executable-marker regression. |
| `core.hooksPath` | Scalar path selecting traditional hook directory. | Overridden for fetch/publication, but does not settle configured hooks: F4. |
| `hook.<name>.command`, `.event`, `.enabled`; `hook.<event>.enabled` | Newer Git: command last wins; event list supports empty reset; enable switches affect execution. Command/event names preserve subsection identity. | Missing from guard/suppression. `reference-transaction` is definitely reachable. |
| `filter.<driver>.clean`, `.process`, `.required` | Driver values selected by attribute; process filter takes precedence over clean/smudge when configured. Scalars within driver. | Observed values neutralized during status; tests preserve unknown dirtiness. Filter names and values should retain Git's case/byte rules. |
| `filter.<driver>.smudge` | Checkout conversion program. | Check does not check out files; confirmed merge may execute and discloses filters. |
| `diff.external`, `diff.<driver>.command`, `.textconv` | Scalar commands used when external diff or conversion output is requested. | Raw tree comparison and fixed subject-only commit output do not request these; no external diff/textconv phase needed. |
| `core.pager`, `pager.<cmd>` | Pager selection with environment precedence and per-command enable/command values. | Piped stdout prevents normal pager activation. A future terminal-inheriting subprocess would need re-audit. |
| `gc.recentObjectsHook` | Multi-valued shell commands consulted by relevant object-retention walks. | Auto-maintenance disabled for fetch; the check does not request GC/repack. |
| `uploadpack.packObjectsHook` | Scalar **protected-config-only** server setting; substitutes pack-objects command. | Remote-local setting ignored; trusted global positive control executes. See decision below. |
| `fetch.bundleURI`, bundle-list URI entries | URI/data selection; bundle fetch can launch built-in HTTPS helper and publish bundle refs. | Not in executable regex, reasonably so as a URI; nonetheless F6 breaks assumptions about the operations reached. |
| `core.unsetenvvars` (Windows) | Last comma-separated variable-name list; removed before child spawning. | Not refused/bound; can undermine environment-based defenses in child processes. Source concern F5; needs Windows reproduction. |
| `ssh.variant` | Chooses fixed argument conventions for the selected SSH program; may avoid/use discovery probe. | Does not name a new executable; repository SSH command remains independently scoped. |
| `http.*`, `http.<url>.*`, `remote.<name>.proxy`, `.proxyAuthMethod` | URL-specific/scalar connection, TLS, proxy and authentication parameters; credential helpers reached through authentication. | No additional arbitrary shell-command config field found here. TLS backend/key-type behavior belongs to dependency/runtime trust; this audit did not certify every TLS engine/provider. |
| `extensions.partialClone`, `remote.*.promisor`, `.partialCloneFilter` | Enable lazy object fetching and associated transport execution. | Refused as partial clone; repeated guards and supported `GIT_NO_LAZY_FETCH` protect typed reads. |

Primary definitions and consumers:
[core configuration](https://github.com/git/git/blob/v2.55.0/Documentation/config/core.adoc),
[remote configuration](https://github.com/git/git/blob/v2.55.0/Documentation/config/remote.adoc),
[remote parser](https://github.com/git/git/blob/v2.55.0/remote.c),
[connection implementation](https://github.com/git/git/blob/v2.55.0/connect.c),
[credential matching](https://github.com/git/git/blob/v2.55.0/credential.c),
[credential manual](https://github.com/git/git/blob/v2.55.0/Documentation/gitcredentials.adoc),
[prompt implementation](https://github.com/git/git/blob/v2.55.0/prompt.c),
[attributes/filter manual](https://github.com/git/git/blob/v2.55.0/Documentation/gitattributes.adoc),
[raw diff implementation](https://github.com/git/git/blob/v2.55.0/builtin/diff-tree.c),
[protocol configuration](https://github.com/git/git/blob/v2.55.0/Documentation/config/protocol.adoc),
[ext transport](https://github.com/git/git/blob/v2.55.0/Documentation/git-remote-ext.adoc).

### Command families excluded by reachability

The config sweep also considered the following executable families. Their
exclusion is based on the fixed operations above, not on treating their values
as safe or on adding them all to a transport regex.

| Family | Why it does not execute during this check |
| --- | --- |
| `alias.*` / newer `alias.*.command` | Fixed existing built-ins are invoked; aliases do not replace these commands. |
| `core.editor`, `sequence.editor`, `core.comment*`, `rebase.*` execution controls | No editing, commit creation, interactive rebase, or sequencer operation. |
| `gpg.program`, `gpg.<format>.program`, `gpg.ssh.defaultKeyCommand`, signing policy | No signature verification/signing requested by check's object output. Merge verification remains disclosed apply behavior. |
| `merge.<driver>.driver`, `mergetool.<tool>.cmd`, `difftool.<tool>.cmd`, tool paths | No content merge or interactive tool operation during check. |
| `interactive.diffFilter` | No interactive patch selection. |
| `submodule.<name>.update` custom commands | No submodule update; fetch explicitly disables recursion and status ignores submodule dirtiness. |
| `remote.*.receivepack`, receive hooks/`receive.procReceiveRefs`, push-signing controls | No push or receive-pack. |
| `tar.<format>.command` | No archive operation. This key lives in the archive manual, not only the standalone config fragments. |
| `trailer.<alias>.cmd` / `.command` | No trailer insertion or `interpret-trailers`; fixed `%s` summaries do not invoke trailer commands. |
| `browser.*.cmd`, `man.*.cmd`, viewers, `web.browser`, `help.*`, `instaweb.*`, `guitool.*.cmd` | No help/browser/GUI operation; valid fixed subcommands avoid autocorrection. |
| `imap.tunnel`, send-email command fields and SMTP program selection | No mail operation. |
| Maintenance schedules/tasks, GC/repack hooks | Fetch's auto-maintenance disabled; check invokes none of those maintenance commands. |
| Includes, attribute/config paths, `core.worktree`, object-store routing | Files/data and routing, not independent program fields. Still security-sensitive: scope, pinned checkout, filter handling, and lazy-fetch guards must continue to apply. |

## Findings and required regressions

### F1 — Upload-pack's first value survives a later empty value

`effective_transport_entry` takes the last entry for everything except helpers
and proxies. Git's `remote.c` instead assigns `uploadpack` only while its
pointer is unset. A later duplicate emits a diagnostic but does not replace
the first value. This is present in both
[v2.41.0](https://github.com/git/git/blob/v2.41.0/remote.c) and
[v2.55.0](https://github.com/git/git/blob/v2.55.0/remote.c).

In a disposable repository with a local `origin`, this Git 2.50.1 probe:

```sh
git config --add remote.origin.uploadpack /usr/bin/true
git config --add remote.origin.uploadpack ''
GIT_TRACE=1 git ls-remote origin
```

executes `/usr/bin/true` with the remote path. The transport then fails because
the program does not speak Git, but execution already happened. Skilled's
parser chooses the empty final entry and permits it. This is a comparison
between actual Git behavior and the reviewed parser, not a newly added
end-to-end Skilled regression.

Required regression: use a marker-writing upload-pack wrapper, append an
empty value, run the public explicit check, and assert refusal before the
marker exists. Include global-first/local-later and local-first/command-later
cases: appending a `-c` value cannot override this first-wins field. Do not
copy the generic scalar strategy into `skilled-88j`.

### F2 — Credential URL subsection case is security-relevant

`parse_transport_settings` lowercases the entire key. Git preserves subsection
case, and credential URL paths match case-sensitively. Consequently:

```ini
[credential]
    useHttpPath = true
[credential "https://example.test/Repo"]
    helper = !echo PATH_CASE_EVIL
[credential "https://example.test/repo"]
    helper =
```

causes Git 2.50.1 to execute the first helper for a credential query with
`protocol=https`, `host=example.test`, and `path=Repo`. Skilled merges those
two distinct keys, sees the empty final reset, and finds no helper to refuse.
The query is supplied to `git credential fill`; it requires no network.

Required regressions: the above case through an explicit check with an
authentication fixture, case-distinct remote names, and generic versus
URL-specific helper resets in both ordering directions. Preserve the original
Git key/value bytes and their scope/order. Either reproduce Git's context
matching correctly or document a conservative refusal that cannot hide a
potentially applicable repository helper. Exact-key list grouping alone does
not establish context-equivalent behavior.

### F3 — Proxy disabling must not trim or fold command values

Git recognizes the exact command `none`; `connect.c` uses length and byte
comparison after parsing the optional ` for DOMAIN` suffix. Skilled trims all
values and compares `none` case-insensitively. A quoted `"none "` remains
nonempty and distinct in Git's config output. Git 2.50.1 trace shows an attempt
to execute that command, while Skilled treats it as disabled. Uppercase
`NONE` is likewise not Git's disabling spelling. A separate probe placed an
executable named `NONE` in a temporary PATH; Git 2.50.1 executed its marker
before failing the transport with exit 128. This establishes actual execution,
not just an attempted lookup of a nonexistent program.

There is also an availability issue: `none for example.test` correctly bypasses
the proxy for that host, while Skilled refuses it as executable. Retaining all
proxy entries is otherwise a defensible conservative policy, provided the
report does not call it exact first-match resolution.

Required regressions: exact `none`, domain-scoped `none`, mixed-case `NONE`,
quoted leading/trailing whitespace, first matching command followed by `none`,
and a trusted first match preceding repository entries. Use a temporary PATH
with a harmless marker program where an executable name is needed. Audit
blanket trimming in SSH/alternate-ref/helper handling as well; an empty value
is not a universal executable-setting reset.

### F4 — Configured reference-transaction hooks bypass directory suppression

On the locally built Git 2.55.0, a temporary repository configured with:

```ini
[hook "audit"]
    command = echo invoked >> /absolute/temporary/hook-ran
    event = reference-transaction
```

executed the marker command during this publication operation:

```sh
git -c core.hooksPath=/dev/null -c core.fsmonitor=false \
    update-ref --no-deref refs/remotes/audit/main "$oid" "$zero_oid"
```

The marker contained `invoked preparing`, `invoked prepared`, and
`invoked committed`. Installed Git 2.50.1 did not execute the configured hook.
The version comparison does not identify the first affected release.

[Git's hook implementation](https://github.com/git/git/blob/v2.55.0/hook.c)
adds configured hooks independently of traditional hooks. The
[configuration definitions](https://github.com/git/git/blob/v2.55.0/Documentation/config/hook.adoc)
specify command/list/reset/enable semantics. The v2.55.0 `core.hooksPath`
documentation still describes `/dev/null` as disabling all hooks; the
source and executed counterexample take precedence over that broad prose.

Required regressions: configured and traditional reference-transaction hooks
on every ref-writing check path, including bundle handling if retained;
disabled hooks and event resets; synchronous/cancellable paths. Establish a
supported-version strategy for event suppression or refusal. Merely adding
`hook.*.command` to a last-value regex will not model the event relationships
and inherited command/local event combinations. Keep merge behavior explicit.

### F5 — Windows child environment stripping needs a platform test

[Git's Windows implementation](https://github.com/git/git/blob/v2.55.0/compat/mingw.c)
reads `core.unsetenvvars` from ordinary configuration, replaces the stored
comma-separated list on subsequent values, and removes those variables before
spawning a child. There is no protected-scope restriction in that callback.

Skilled relies on inherited `GIT_ALLOW_PROTOCOL`, `GIT_SSH_COMMAND`, prompting
variables, and other guards. A checkout-controlled list naming those variables
can therefore change what a Git child inherits. The effect depends on which
process has already consumed a value and which child reads it again; source
inspection alone is not an end-to-end exploit demonstration.

Required regression on Windows: configure removal of each guard variable,
trace the relevant child paths, and assert that a repository helper, askpass,
or widened transport cannot execute. Windows is not a release gate today,
but the audit must not silently describe this mechanism as platform-neutral.

### F6 — Bundle URI fetching reaches additional helpers and ref writes

An isolated Git 2.50.1 repository configured with `fetch.bundleURI` pointing
to a local bundle made with `git bundle create data.bundle --all` created
`refs/bundles/heads/master` despite all of Skilled's fetch flags:

```sh
git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c gc.auto=0 \
    fetch --porcelain --dry-run --no-auto-maintenance \
    --no-write-fetch-head --no-tags --no-prune --no-prune-tags \
    --recurse-submodules=no --refmap= -- origin \
    +HEAD:refs/skilled/fetch/audit
git for-each-ref --format='%(refname)'
# refs/bundles/heads/master
```

This is an auxiliary ref write, not execution of an arbitrary URI as a shell
command. It matters because ref writes trigger hooks, and because the stated
fetch/no-ref-write proof assumes a single transfer path.

[bundle-uri.c](https://github.com/git/git/blob/v2.55.0/bundle-uri.c) dispatches
HTTP(S) downloads directly to `git-remote-https`; other URIs are handled as
file copies after optional `file://` removal. It does not use the ordinary
transport dispatcher for that download. The audit did not reproduce an HTTP
policy bypass, and does not claim that arbitrary helper schemes execute here.
The direct helper path needs a separate inherited-policy test.

Required follow-up: decide whether bundle fetching belongs in the check,
account for its refs and credentials/hooks, and test protocol policy on that
path. Do not state that `--dry-run` alone means no refs can be written.

## Decision: remote upload-pack pack-objects hook

The relevant trust boundary is the configuration read by the server-side
upload-pack process. In both runtime versions:

| Configuration placement | Marker result during a fresh local fetch |
| --- | --- |
| Remote bare repository's local config | Not executed |
| Fetch client's `-c uploadpack.packObjectsHook=...` | Not executed in this local-transport probe |
| Isolated global config visible to upload-pack | Executed |

Each hook was an executable temporary script that wrote a marker then ran its
arguments with `exec "$@"`. Each fetch used a fresh empty client so pack
generation was required. The global positive control rules out an inert
fixture or lack of pack generation as the explanation for the local negative.

The protected-config callback is explicit in
[v2.41.0 upload-pack.c](https://github.com/git/git/blob/v2.41.0/upload-pack.c)
and [v2.55.0 upload-pack.c](https://github.com/git/git/blob/v2.55.0/upload-pack.c).
Protected scope includes system/global/command, but a client's command-scope
configuration is not automatically the remote process's command scope. The
negative client `-c` probe must not be generalized into “protected hooks never
run over local transport.”

Decision: remote-local `uploadpack.packObjectsHook` is outside the missing
repository-executable-key refusal set for the reviewed Git versions. Preserve
the global behavior as trusted configuration. An SSH server may execute its
own trusted hooks/programs; the client cannot establish their complete policy
by inspecting its registered checkout. A repository-provided
`remote.*.uploadpack` is different and remains a refusal (F1).

## Consequences for skilled-88j

The vetted-configuration work needs a per-setting strategy, not a universal
append-only `-c` override:

- Upload-pack is first-wins, so appending a vetted value is ineffective.
- Proxies are first-match and may include domain restrictions; a later value
  does not generally outrank a prior repository entry.
- Credentials combine list resets with URL matching; preserve scope, order,
  path case, and actual values.
- SSH is already independently selected by scope. The explicitly empty
  `GIT_ASKPASS` blocks configured/SSH askpass fallback on the reviewed Unix
  path; `GIT_TERMINAL_PROMPT=0` separately prevents terminal prompts. Revise
  the old assumption that `core.askPass` necessarily remains live there.
- Configured hooks add event relationships, not merely a command scalar.
- Auxiliary bundle processes and Windows environment propagation belong in
  the child-process model.

The audit does not select a wholesale configuration-isolation design. Any
such design must preserve the user's trusted settings and checkout identity,
and cannot be justified by these findings alone without its own tests.

## Validation and limits

Executed against the audited Skilled checkout:

| Check | Result |
| --- | --- |
| `cargo test --lib git::tests` | 28 passed |
| `cargo test --test update_flow` | 137 passed |
| Git 2.55.0 local build | Successful; feature exclusions listed above |
| Configured-hook marker probes | 2.50.1 did not run it; 2.55.0 ran it |
| Pack hook local/global controls | Local ignored; global executed on both runtime versions |
| Upload-pack, credential path case, proxy trace probes | Confirmed Git behavior differs from current parser |
| Bundle dry-run ref probe | Extra bundle ref created on 2.50.1 |

These are existing regression suites and temporary audit experiments. No new
permanent regression tests were added. The parser counterexamples were checked
against the implementation, not installed as failing end-to-end Skilled tests.
Windows, network authentication, TLS engines/providers, vendor Git variants,
and the entire Git 2.41–2.55 release range were not runtime-tested. No release
build or full application suite is needed to validate this documentation-only
change; those gates remain appropriate for the subsequent runtime fixes.

## Reproducing the isolated environment

Use a fresh temporary directory and a subprocess environment that removes
inherited `GIT_*` variables. Set `HOME` and `XDG_CONFIG_HOME` inside the
temporary directory, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`,
`GIT_TERMINAL_PROMPT=0`, and synthetic author/committer identities. Invoke Git
by absolute path. For the protected-global positive control, replace only
`GIT_CONFIG_GLOBAL` with a temporary file containing the hook setting.

Initialize temporary repositories and create an empty commit. For publication,
use its `rev-parse HEAD` as the new object and forty zeroes as the expected
absent SHA-1 ref. For upload-pack tests, create a local bare remote, push only
the synthetic commit into it, then fetch into a fresh temporary client. The
only writes or commands in these probes should be fixture creation, local
Git operations, and marker programs owned by the probe.

For credential case matching, feed this input to `git credential fill` with
`GIT_TRACE=1` and the F2 config; a failure to obtain credentials is expected
after the marker helper has been invoked:

```text
protocol=https
host=example.test
path=Repo

```

Use marker/trace evidence rather than exit status alone: a program can execute
before Git rejects its output. For the proxy probe use `git://example.test/`
and a temporary executable search path; do not require any server to exist.

## Source index

Pinned source trees make this a reproducible audit snapshot rather than a
claim about all future Git versions:

- [Git v2.41.0 config corpus](https://github.com/git/git/tree/v2.41.0/Documentation/config)
- [Git v2.55.0 config corpus](https://github.com/git/git/tree/v2.55.0/Documentation/config)
- [Git v2.55.0 config syntax/scope manual](https://github.com/git/git/blob/v2.55.0/Documentation/git-config.adoc)
- [Git v2.55.0 fetch implementation](https://github.com/git/git/blob/v2.55.0/builtin/fetch.c)
- [Git v2.55.0 ref transactions](https://github.com/git/git/blob/v2.55.0/refs.c)
- [Git v2.55.0 pager implementation](https://github.com/git/git/blob/v2.55.0/pager.c)
- [Skilled transport regressions](../tests/update_flow.rs)
- [Skilled safety contracts](safety.md#inspection-and-transport-policy)

Re-audit when adding a Git subcommand, changing output formats, inheriting a
terminal, enabling another fetch path, changing supported Git versions, or
altering the configuration/environment isolation policy.
